//! The Collector loop: watch one machine's Agent Sessions and push them to a Relay.
//!
//! It reuses the very same [`Engine`] the board runs locally — discovery of Claude
//! Code + Codex CLI sessions, watching, status refresh, live git `+/-` enrichment,
//! and machine stamping — so a remote card carries the same stats a local one does
//! (User Story 18). This adapter only pushes the Engine's snapshot and NDJSON event
//! stream to the Relay over one long-lived request, reconnecting after a Relay
//! restart or network blip (User Story 14).

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use reqwest::header::AUTHORIZATION;
use session_engine::Engine;
use sessions::{Event, Session};
use tokio_stream::wrappers::BroadcastStream;
use tracing::{info, warn};

use crate::wire::{bearer, to_ndjson_line};

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
    let engine = Engine::start(config.claude_root, config.codex_root, config.machine);
    info!(relay = %config.relay_url, "collector started local session engine");
    push_loop(&config.relay_url, &config.token, &engine).await;
}

/// Dial the Relay and push forever, reconnecting after any drop.
async fn push_loop(relay_url: &str, token: &str, engine: &Engine) {
    let endpoint = format!("{}/collect", relay_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    loop {
        match push_once(&client, &endpoint, token, engine).await {
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
    engine: &Engine,
) -> reqwest::Result<()> {
    let rx = engine.subscribe();
    let snapshot: Vec<Session> = engine.snapshot();
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
