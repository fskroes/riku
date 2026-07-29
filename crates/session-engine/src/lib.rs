//! The live local-session pipeline shared by the Board and the Collector.
//!
//! [`Engine`] owns the runtime-dependent work around the runtime-agnostic
//! `sessions` crate: discovery, filesystem watching, periodic status refresh,
//! diff enrichment, machine stamping, and broadcasting complete local events.
//! Callers use its small interface to take a snapshot, subscribe to local changes,
//! or resolve a local transcript; transport and presentation stay in their
//! adapters.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use sessions::{
    Change, ClaudeSource, CodexSource, DiffCache, Event, Session, SessionSource, SessionStore,
    WatchGuard,
};
use tokio::sync::broadcast;
use tracing::{info, warn};

/// How often the Engine re-evaluates status so a quiet session can become
/// `Finished` without another filesystem event.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// A running local-session pipeline. Dropping it stops its filesystem watcher.
pub struct Engine {
    store: Arc<Mutex<SessionStore>>,
    events: broadcast::Sender<Event>,
    diff_cache: Arc<DiffCache>,
    machine: Arc<str>,
    // The guard is intentionally private: callers only need the Engine's
    // snapshot/event interface, but it must live as long as that interface does.
    _watch_guard: Option<WatchGuard>,
}

impl Engine {
    /// Discover sessions below the Claude and optional Codex roots, then start the
    /// live watcher and periodic refresh loop. Must be called inside a Tokio
    /// runtime because the refresh loop is spawned onto it.
    pub fn start(
        claude_root: PathBuf,
        codex_root: Option<PathBuf>,
        machine: impl Into<Arc<str>>,
    ) -> Self {
        // Canonicalize so filesystem event paths match discovery keys. A root that
        // does not exist yet keeps its given spelling; discovery and watching then
        // degrade gracefully.
        let claude_root = std::fs::canonicalize(&claude_root).unwrap_or(claude_root);
        let codex_root = codex_root.map(|root| std::fs::canonicalize(&root).unwrap_or(root));

        let mut sources: Vec<Box<dyn SessionSource>> =
            vec![Box::new(ClaudeSource::new(claude_root))];
        if let Some(root) = codex_root {
            sources.push(Box::new(CodexSource::new(root)));
        }
        let roots: Vec<PathBuf> = sources.iter().flat_map(|source| source.roots()).collect();
        let store = Arc::new(Mutex::new(SessionStore::new(sources)));
        let (events, _) = broadcast::channel::<Event>(1024);
        let diff_cache = Arc::new(DiffCache::new());
        let machine = machine.into();

        let count = store.lock().unwrap().scan(Utc::now()).len();
        info!(?roots, sessions = count, machine = %machine, "discovered sessions");

        let watch_guard = start_watch(
            &roots,
            store.clone(),
            events.clone(),
            diff_cache.clone(),
            machine.clone(),
        );
        spawn_refresh(
            store.clone(),
            events.clone(),
            diff_cache.clone(),
            machine.clone(),
        );

        Engine {
            store,
            events,
            diff_cache,
            machine,
            _watch_guard: watch_guard,
        }
    }

    /// A current, fully enriched and machine-stamped view of local sessions.
    /// This can invoke git, so async adapters should call it on a blocking worker.
    pub fn snapshot(&self) -> Vec<Session> {
        let mut sessions = self.store.lock().unwrap().snapshot(Utc::now());
        for session in &mut sessions {
            self.diff_cache.enrich(session);
            stamp(session, &self.machine);
        }
        sessions
    }

    /// Subscribe to complete local-session events. Every `Upsert` is enriched and
    /// stamped before it is sent; a lagged receiver can recover with [`snapshot`](Self::snapshot).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Resolve a local session's trusted transcript path for deep-linking. The
    /// returned session is machine-stamped like every Engine read; the diff is not
    /// enriched, since a deep-link needs only the transcript path.
    pub fn find_by_id(&self, id: &str) -> Option<(PathBuf, Session)> {
        let (transcript, mut session) = self.store.lock().unwrap().find_by_id(id, Utc::now())?;
        stamp(&mut session, &self.machine);
        Some((transcript, session))
    }

    /// Return local sessions in one working directory, each machine-stamped like
    /// every Engine read. The Board uses this to build Work Links without exposing
    /// the store itself; the diff is not enriched, since a Work Link carries no diff.
    pub fn sessions_in(&self, cwd: &str) -> Vec<Session> {
        let mut sessions: Vec<Session> = self
            .store
            .lock()
            .unwrap()
            .snapshot(Utc::now())
            .into_iter()
            .filter(|session| session.cwd.as_deref() == Some(cwd))
            .collect();
        for session in &mut sessions {
            stamp(session, &self.machine);
        }
        sessions
    }
}

/// This machine's name, never empty. Board and Collector both use it to label
/// sessions consistently; a caller may still supply an explicit name to `start`.
pub fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|hostname| hostname.into_string().ok())
        .filter(|hostname| !hostname.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Stamp a session unless it already carries a remote machine identity. Private to
/// the Engine: machine stamping is an internal step of local-session production, not
/// a capability the Engine hands out. Every read that returns a `Session`
/// ([`snapshot`](Engine::snapshot), [`sessions_in`](Engine::sessions_in),
/// [`find_by_id`](Engine::find_by_id)) applies it, so an adapter never has to
/// re-stamp Engine output.
fn stamp(session: &mut Session, machine: &str) {
    if session.machine.is_none() {
        session.machine = Some(machine.to_string());
    }
}

fn finalize(event: &mut Event, diff_cache: &DiffCache, machine: &str) {
    if let Event::Upsert(session) = event {
        diff_cache.enrich(session);
        stamp(session, machine);
    }
}

fn start_watch(
    roots: &[PathBuf],
    store: Arc<Mutex<SessionStore>>,
    events: broadcast::Sender<Event>,
    diff_cache: Arc<DiffCache>,
    machine: Arc<str>,
) -> Option<WatchGuard> {
    let should_report = {
        let store = store.clone();
        move |path: &std::path::Path| store.lock().unwrap().owns_path(path)
    };
    match sessions::watch(roots, should_report, move |change| {
        let event = {
            let mut store = store.lock().unwrap();
            match change {
                Change::Modified(path) => store.ingest(&path, Utc::now()),
                Change::Removed(path) => store.remove(&path),
            }
        };
        // The watcher owns a dedicated thread, keeping git work off the runtime.
        if let Some(mut event) = event {
            finalize(&mut event, &diff_cache, &machine);
            let _ = events.send(event);
        }
    }) {
        Ok(guard) => Some(guard),
        Err(error) => {
            warn!(error = %error, "failed to start filesystem watcher; live updates disabled");
            None
        }
    }
}

fn spawn_refresh(
    store: Arc<Mutex<SessionStore>>,
    events: broadcast::Sender<Event>,
    diff_cache: Arc<DiffCache>,
    machine: Arc<str>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            // Probe process liveness off the runtime (it shells out to ps/lsof
            // with a 2s budget) and before taking the store lock. `None` means the
            // probe failed — skip the liveness pass entirely so a bad tick can
            // never mass-finish sessions; the mtime refresh still runs.
            let alive_cwds = tokio::task::spawn_blocking(sessions::probe_alive_cwds)
                .await
                .ok()
                .flatten();
            let mut refreshed = {
                let mut store = store.lock().unwrap();
                let now = Utc::now();
                let mut events = store.refresh(now);
                if let Some(alive) = &alive_cwds {
                    events.extend(store.apply_liveness(alive, now));
                }
                events
            };
            let cache = diff_cache.clone();
            let machine = machine.clone();
            let events_to_send = tokio::task::spawn_blocking(move || {
                for event in &mut refreshed {
                    finalize(event, &cache, &machine);
                }
                refreshed
            })
            .await
            .expect("diff enrichment does not panic");
            for event in events_to_send {
                let _ = events.send(event);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{local_hostname, stamp, Engine};
    use chrono::Utc;
    use sessions::{Event, Session, Status, Tool};
    use tokio::sync::broadcast;

    /// Write a one-line Claude transcript for `session_id` under `root/<slug>/`,
    /// so a test can drive discovery or the live watcher off a real file. Keeps the
    /// transcript JSON skeleton in one place across the Engine's tests.
    fn write_transcript(root: &std::path::Path, slug: &str, session_id: &str, cwd: &str) {
        let project = root.join(slug);
        fs::create_dir_all(&project).unwrap();
        let transcript = serde_json::json!({
            "type": "assistant",
            "sessionId": session_id,
            "timestamp": "2026-07-19T10:00:00Z",
            "cwd": cwd,
            "gitBranch": "main",
            "message": {
                "model": "claude-opus-4-8",
                "usage": { "input_tokens": 100, "output_tokens": 10 },
                "content": [{ "type": "text", "text": session_id }]
            }
        })
        .to_string();
        fs::write(project.join("session.jsonl"), format!("{transcript}\n")).unwrap();
    }

    fn bare_session() -> Session {
        Session {
            id: "s".into(),
            tool: Tool::Claude,
            project: "p".into(),
            model: None,
            branch: None,
            cwd: None,
            tokens_in: 0,
            tokens_out: 0,
            activity: None,
            last_event_at: Utc::now(),
            status: Status::Active,
            attention: None,
            cost_usd: None,
            diff: None,
            sub_agent_roster: Vec::new(),
            machine: None,
        }
    }

    #[test]
    fn stamps_an_unlabelled_session_without_overwriting_a_remote_label() {
        let mut session = bare_session();
        stamp(&mut session, "loki.local");
        assert_eq!(session.machine.as_deref(), Some("loki.local"));
        stamp(&mut session, "forge-desktop");
        assert_eq!(session.machine.as_deref(), Some("loki.local"));
    }

    #[test]
    fn local_hostname_is_never_empty() {
        assert!(!local_hostname().is_empty());
    }

    #[tokio::test]
    async fn a_watched_transcript_update_reaches_a_subscription_stamped_with_the_machine() {
        // Prove the live path — filesystem watch → ingest → finalize → broadcast —
        // carries this machine's identity to a subscriber, not just the snapshot path.
        // This is the Engine's own guarantee, independent of any Board or Relay wiring.
        let root = tempfile::tempdir().unwrap();
        let engine = Engine::start(root.path().to_path_buf(), None, "loki.local");
        let mut rx = engine.subscribe();

        // Write a transcript *after* subscribing so the update travels the live
        // watcher path rather than being seen only by the initial scan.
        write_transcript(root.path(), "-Users-x-repos-bar", "watched-1", "/Users/x/repos/bar");

        // The watcher debounces (~250ms) on a dedicated thread, so allow a generous
        // budget before giving up rather than racing a fixed sleep.
        let upserted = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match rx.recv().await {
                    Ok(Event::Upsert(session)) if session.id == "watched-1" => return session,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        panic!("event stream closed before the watched update arrived")
                    }
                }
            }
        })
        .await
        .expect("a watched transcript update should reach the subscription");

        assert_eq!(upserted.machine.as_deref(), Some("loki.local"));
    }

    #[tokio::test]
    async fn snapshot_exposes_an_enriched_machine_stamped_local_session() {
        let root = tempfile::tempdir().unwrap();
        write_transcript(root.path(), "-Users-x-repos-foo", "engine-1", "/Users/x/repos/foo");

        let engine = Engine::start(root.path().to_path_buf(), None, "loki.local");
        let sessions = engine.snapshot();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "engine-1");
        assert_eq!(sessions[0].machine.as_deref(), Some("loki.local"));
    }

    #[tokio::test]
    async fn sessions_in_returns_machine_stamped_sessions() {
        // The Work-Link read is machine-stamped like every Engine read, so the Board
        // never has to re-stamp a candidate (see `Engine::sessions_in`).
        let root = tempfile::tempdir().unwrap();
        write_transcript(root.path(), "-Users-x-repos-foo", "engine-1", "/Users/x/repos/foo");

        let engine = Engine::start(root.path().to_path_buf(), None, "loki.local");
        let sessions = engine.sessions_in("/Users/x/repos/foo");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "engine-1");
        assert_eq!(sessions[0].machine.as_deref(), Some("loki.local"));
    }

    #[tokio::test]
    async fn find_by_id_returns_a_machine_stamped_session() {
        // The deep-link read is machine-stamped too, keeping the invariant uniform.
        let root = tempfile::tempdir().unwrap();
        write_transcript(root.path(), "-Users-x-repos-foo", "engine-1", "/Users/x/repos/foo");

        let engine = Engine::start(root.path().to_path_buf(), None, "loki.local");
        let (_, session) = engine.find_by_id("engine-1").expect("session resolves by id");
        assert_eq!(session.machine.as_deref(), Some("loki.local"));
    }
}
