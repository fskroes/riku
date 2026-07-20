//! The Collector loop: watch one machine's Agent Sessions and push them to a Relay.
//!
//! It reuses the very same pipeline the board runs locally — [`SessionStore`] over
//! the Claude Code + Codex CLI [`SessionSource`]s, the filesystem watcher, the
//! periodic status refresh, and live git `+/-` enrichment — so a remote card
//! carries the same stats a local one does (User Story 18). The only differences
//! from the board are the two ends: every Session is stamped with this machine's
//! name before it leaves, and instead of serving HTTP it pushes NDJSON `Event`s up
//! to the Relay over one long-lived request, reconnecting on its own after a Relay
//! restart or a network blip (User Story 14).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use collector::{
    Change, ClaudeSource, CodexSource, DiffCache, Event, Session, SessionSource, SessionStore,
    WatchGuard,
};
use futures::StreamExt;
use reqwest::header::AUTHORIZATION;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{info, warn};

use crate::wire::{bearer, to_ndjson_line};

/// How often statuses are re-evaluated so time-based transitions (a quiet session
/// crossing into Finished) reach the Relay without a file change. Matches the board.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// How long to wait before re-dialing the Relay after a dropped or refused push.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Everything the Collector needs: where the Relay is, the shared token, which roots
/// to watch, and the machine label to stamp on every Session.
pub struct CollectorConfig {
    pub relay_url: String,
    pub token: String,
    pub claude_root: PathBuf,
    pub codex_root: Option<PathBuf>,
    pub machine: String,
}

/// Run the Collector until the process is stopped. Discovers sessions, starts
/// watching, and pushes to the Relay forever (reconnecting as needed).
pub async fn run(config: CollectorConfig) {
    // Canonicalize like the board so watcher event paths match discovery keys.
    let claude_root =
        std::fs::canonicalize(&config.claude_root).unwrap_or_else(|_| config.claude_root.clone());
    let codex_root = config
        .codex_root
        .clone()
        .map(|r| std::fs::canonicalize(&r).unwrap_or(r));

    let mut sources: Vec<Box<dyn SessionSource>> =
        vec![Box::new(ClaudeSource::new(claude_root.clone()))];
    if let Some(root) = codex_root.clone() {
        sources.push(Box::new(CodexSource::new(root)));
    }
    let roots: Vec<PathBuf> = sources.iter().flat_map(|s| s.roots()).collect();

    let store = Arc::new(Mutex::new(SessionStore::new(sources)));
    let (tx, _) = broadcast::channel::<Event>(1024);
    let diff = Arc::new(DiffCache::new());
    let machine: Arc<str> = Arc::from(config.machine);

    let count = {
        let mut guard = store.lock().unwrap();
        guard.scan(Utc::now()).len()
    };
    info!(?roots, sessions = count, machine = %machine, relay = %config.relay_url, "collector discovered sessions");

    // Kept alive for the whole push loop; dropping it stops filesystem events.
    let _guard = start_watch(&roots, store.clone(), tx.clone(), diff.clone(), machine.clone());
    spawn_refresh(store.clone(), tx.clone(), diff.clone(), machine.clone());

    push_loop(&config.relay_url, &config.token, store, tx, diff, machine).await;
    drop(_guard);
}

/// Stamp a Session with this machine's name unless it already carries one. Local
/// store sessions are always unstamped, so this always sets it; the guard is kept
/// for symmetry with the board and to never overwrite an already-tagged session.
fn stamp(session: &mut Session, machine: &str) {
    if session.machine.is_none() {
        session.machine = Some(machine.to_string());
    }
}

/// Enrich an upsert with live git `+/-` and stamp its machine; pass others through.
fn finalize(event: &mut Event, diff: &DiffCache, machine: &str) {
    if let Event::Upsert(session) = event {
        diff.enrich(session);
        stamp(session, machine);
    }
}

fn start_watch(
    roots: &[PathBuf],
    store: Arc<Mutex<SessionStore>>,
    tx: broadcast::Sender<Event>,
    diff: Arc<DiffCache>,
    machine: Arc<str>,
) -> Option<WatchGuard> {
    let result = collector::watch(roots, move |change| {
        let now = Utc::now();
        let event = {
            let mut guard = store.lock().unwrap();
            match change {
                Change::Modified(path) => guard.ingest(&path, now),
                Change::Removed(path) => guard.remove(&path),
            }
        };
        // The watcher thread does the git call (TTL-cached), off any async worker.
        if let Some(mut event) = event {
            finalize(&mut event, &diff, &machine);
            let _ = tx.send(event);
        }
    });
    match result {
        Ok(guard) => Some(guard),
        Err(e) => {
            warn!(error = %e, "failed to start filesystem watcher; live updates disabled");
            None
        }
    }
}

fn spawn_refresh(
    store: Arc<Mutex<SessionStore>>,
    tx: broadcast::Sender<Event>,
    diff: Arc<DiffCache>,
    machine: Arc<str>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let mut events = {
                let mut guard = store.lock().unwrap();
                guard.refresh(Utc::now())
            };
            let diff = diff.clone();
            let machine = machine.clone();
            let events = tokio::task::spawn_blocking(move || {
                for event in &mut events {
                    finalize(event, &diff, &machine);
                }
                events
            })
            .await
            .expect("diff enrichment does not panic");
            for event in events {
                let _ = tx.send(event);
            }
        }
    });
}

/// Dial the Relay and push forever, reconnecting after any drop.
async fn push_loop(
    relay_url: &str,
    token: &str,
    store: Arc<Mutex<SessionStore>>,
    tx: broadcast::Sender<Event>,
    diff: Arc<DiffCache>,
    machine: Arc<str>,
) {
    let endpoint = format!("{}/collect", relay_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    loop {
        match push_once(&client, &endpoint, token, &store, &tx, &diff, &machine).await {
            Ok(()) => info!("relay push ended; reconnecting"),
            Err(e) => warn!(error = %e, "relay push failed; reconnecting"),
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// One push connection: send the current snapshot, then stream live `Event`s, until
/// the connection drops. Subscribing to the broadcast *before* snapshotting closes
/// the gap where a change could slip between the two; the receiving board dedupes an
/// overlapping upsert idempotently.
async fn push_once(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    store: &Arc<Mutex<SessionStore>>,
    tx: &broadcast::Sender<Event>,
    diff: &Arc<DiffCache>,
    machine: &Arc<str>,
) -> reqwest::Result<()> {
    let rx = tx.subscribe();

    let mut snapshot: Vec<Session> = {
        let guard = store.lock().unwrap();
        guard.snapshot(Utc::now())
    };
    for session in &mut snapshot {
        diff.enrich(session);
        stamp(session, machine);
    }
    let snapshot = futures::stream::iter(
        snapshot
            .into_iter()
            .map(|s| Ok::<_, std::io::Error>(to_ndjson_line(&Event::Upsert(s)))),
    );
    let live = BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(event) => Some(Ok::<_, std::io::Error>(to_ndjson_line(&event))),
            // Lagged while we were momentarily behind: drop it. The next refresh (or
            // the reconnect snapshot) re-syncs, since every upsert is a full snapshot.
            Err(_) => None,
        }
    });

    let body = reqwest::Body::wrap_stream(snapshot.chain(live));
    // `send().await` stays pending for the life of the connection: it drives the
    // chunked upload of the (endless) body and only resolves when the Relay closes
    // the connection or it drops — at which point we reconnect and re-snapshot.
    client
        .post(endpoint)
        .header(AUTHORIZATION, bearer(token))
        .body(body)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
