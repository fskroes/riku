//! The shared wire protocol for both remote hops.
//!
//! The currency is [`sessions::Event`] — every `Upsert` is a full Session
//! snapshot, so both streams are idempotent and self-healing (a dropped frame or a
//! reconnect re-syncs on the next snapshot). One JSON object per event:
//!
//! - **Collector → Relay** (push): newline-delimited JSON — one `Event` per line.
//! - **Relay → Board** (fan-out): Server-Sent Events, the `Event` JSON in each
//!   `data:` field (mirrors the shape of the board's own `/api/events`, so the same
//!   "snapshot then live stream" contract holds end to end).
//!
//! A single shared token gates both roles, presented as `Authorization: Bearer …`.

use axum::http::{header::AUTHORIZATION, HeaderMap};
use axum::response::sse::Event as SseEvent;
use sessions::Event;

/// The `Authorization` scheme prefix for the shared token.
const BEARER: &str = "Bearer ";

/// Whether `headers` carry the expected shared token as `Authorization: Bearer …`.
/// The token is a shared secret (not a per-user password), so a plain compare is
/// used; there is no per-account timing surface to protect.
pub fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix(BEARER))
        .is_some_and(|tok| tok == expected)
}

/// The `Authorization` header value a Collector or board sends.
pub fn bearer(token: &str) -> String {
    format!("{BEARER}{token}")
}

/// Encode an `Event` for the Relay→Board SSE fan-out: the whole `Event` JSON in the
/// `data:` field. Named-event framing is not used — the subscriber is a Rust board,
/// not a browser, so it decodes the `Event` directly (see [`SseDecoder`]).
pub fn to_sse(event: &Event) -> SseEvent {
    SseEvent::default()
        .json_data(event)
        .expect("Event serializes to JSON")
}

/// Encode an `Event` as one NDJSON line (trailing `\n`) for the Collector→Relay push.
pub fn to_ndjson_line(event: &Event) -> String {
    let mut s = serde_json::to_string(event).expect("Event serializes to JSON");
    s.push('\n');
    s
}

/// Accumulates streamed bytes and yields complete `\n`-terminated lines. Chunk
/// boundaries from an HTTP body can split a line, so a partial tail is held until
/// its newline arrives.
#[derive(Default)]
struct LineBuffer {
    buf: Vec<u8>,
}

impl LineBuffer {
    /// Append `bytes` and return every complete line now available (without its
    /// terminating newline). A trailing partial line stays buffered.
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            // Drop the trailing '\n' (and a '\r' if the sender used CRLF).
            let end = line.len() - 1;
            let end = if end > 0 && line[end - 1] == b'\r' {
                end - 1
            } else {
                end
            };
            lines.push(String::from_utf8_lossy(&line[..end]).into_owned());
        }
        lines
    }
}

/// Decodes an NDJSON `Event` stream (the Collector→Relay push body). Each non-empty
/// line is one `Event`; a line that fails to parse is skipped rather than aborting
/// the connection, so one malformed frame cannot desync the whole machine.
#[derive(Default)]
pub struct NdjsonDecoder {
    lines: LineBuffer,
}

impl NdjsonDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.lines
            .push(bytes)
            .into_iter()
            .filter_map(|line| decode_event(&line))
            .collect()
    }
}

/// Decodes the Relay→Board SSE stream back into `Event`s. Only `data:` fields carry
/// payload; comment keep-alive lines (`: ping`) and any other SSE fields are
/// ignored. Since each event is a single-line `data:` JSON object, lines are decoded
/// as they complete without tracking event-frame boundaries.
#[derive(Default)]
pub struct SseDecoder {
    lines: LineBuffer,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut out = Vec::new();
        for line in self.lines.push(bytes) {
            if let Some(data) = line.strip_prefix("data:") {
                if let Some(event) = decode_event(data.trim()) {
                    out.push(event);
                }
            }
        }
        out
    }
}

/// Parse one JSON line as an `Event`, warning (not failing) on a malformed frame.
fn decode_event(line: &str) -> Option<Event> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    match serde_json::from_str::<Event>(line) {
        Ok(event) => Some(event),
        Err(e) => {
            tracing::warn!(error = %e, "skipping malformed wire event");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use chrono::Utc;
    use sessions::{Session, Status, Tool};

    fn session(id: &str, machine: &str) -> Session {
        Session {
            id: id.into(),
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
            attention_reason: None,
            cost_usd: None,
            diff: None,
            machine: Some(machine.into()),
        }
    }

    #[test]
    fn authorized_accepts_only_the_exact_token() {
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, "s3cret")); // missing
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer wrong"));
        assert!(!authorized(&headers, "s3cret"));
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer s3cret"));
        assert!(authorized(&headers, "s3cret"));
    }

    #[test]
    fn ndjson_round_trips_events_across_a_split_chunk() {
        let up = Event::Upsert(session("a1", "desk"));
        let rm = Event::Removed { id: "a1".into() };
        let mut wire = to_ndjson_line(&up);
        wire.push_str(&to_ndjson_line(&rm));

        // Deliver the bytes in two arbitrary chunks to exercise the line buffer.
        let (head, tail) = wire.as_bytes().split_at(wire.len() / 3);
        let mut dec = NdjsonDecoder::default();
        let mut events = dec.push(head);
        events.extend(dec.push(tail));

        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0], Event::Upsert(s) if s.id == "a1" && s.machine.as_deref() == Some("desk"))
        );
        assert!(matches!(&events[1], Event::Removed { id } if id == "a1"));
    }

    #[test]
    fn sse_decoder_skips_pings_and_extracts_data() {
        // Render the SSE frame the way `to_sse` + axum would (`data: <json>\n\n`),
        // preceded by a keep-alive comment the decoder must ignore.
        let json = serde_json::to_string(&Event::Upsert(session("b2", "mate"))).unwrap();
        let framed = format!(": ping\n\ndata: {json}\n\n");

        let (head, tail) = framed.as_bytes().split_at(framed.len() / 2);
        let mut dec = SseDecoder::default();
        let mut events = dec.push(head);
        events.extend(dec.push(tail));

        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::Upsert(s) if s.id == "b2"));
    }
}
