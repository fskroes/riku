//! The board's Relay subscription client.
//!
//! Connects to a Relay's `/subscribe` fan-out, decodes the SSE `Event`s, and hands
//! each to the board through an [`Update`]. It reconnects on its own after any error
//! or a clean stream end, so a Relay restart needs no supervision. On every fresh
//! connection it emits [`Update::Connected`] *before* the snapshot that follows, so
//! the board can reset its remote view and let the snapshot rebuild it — the way the
//! Relay drops nothing durable and Collectors simply re-push (ADR 0004).

use std::time::Duration;

use futures::StreamExt;
use reqwest::header::AUTHORIZATION;
use sessions::Event;
use tracing::{info, warn};

use crate::wire::{bearer, SseDecoder};

/// How long to wait before re-dialing the Relay after a dropped subscription.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// A change surfaced to the board by [`subscribe`].
///
/// `Event` dominates the enum's size because it carries a full Session snapshot —
/// the same reason `sessions::Event` allows the lint. `Update` is delivered once
/// per event to an in-process callback, not a hot path, and boxing it here while the
/// wire `Event` stays unboxed would be an odd asymmetry, so the lint is allowed.
#[allow(clippy::large_enum_variant)]
pub enum Update {
    /// A fresh connection to the Relay was established. The board should treat its
    /// remote view as stale; the snapshot that follows immediately rebuilds it.
    Connected,
    /// The connection dropped; the board is now reconnecting (relay status = amber).
    Disconnected,
    /// A decoded snapshot or live event to apply.
    Event(Event),
}

/// Subscribe to `relay_url` forever, invoking `on_update` for each state change.
/// Never returns; drive it with `tokio::spawn`.
pub async fn subscribe(relay_url: String, token: String, mut on_update: impl FnMut(Update) + Send) {
    let endpoint = format!("{}/subscribe", relay_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    loop {
        match connect(&client, &endpoint, &token, &mut on_update).await {
            Ok(()) => info!("relay subscription ended; reconnecting"),
            Err(e) => warn!(error = %e, "relay subscription error; reconnecting"),
        }
        on_update(Update::Disconnected);
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// One subscription connection: signal `Connected`, then stream decoded events until
/// the connection ends.
async fn connect(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    on_update: &mut (impl FnMut(Update) + Send),
) -> reqwest::Result<()> {
    let resp = client
        .get(endpoint)
        .header(AUTHORIZATION, bearer(token))
        .send()
        .await?
        .error_for_status()?;

    on_update(Update::Connected);

    let mut stream = resp.bytes_stream();
    let mut decoder = SseDecoder::default();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        for event in decoder.push(&bytes) {
            on_update(Update::Event(event));
        }
    }
    Ok(())
}
