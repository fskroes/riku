//! The shared wire protocol for both remote hops, and the distinct Relay wire types.
//!
//! ADR 0010 keeps the rich local-domain [`sessions::Session`] off the network: the
//! wire carries [`WireSession`] / [`WireEvent`], whose Attention holds only the
//! privacy-safe **remote** evidence (allowlisted structured fields). The Collector
//! converts `Session → WireSession` before sending, so local display evidence
//! physically cannot become network payload; a subscribing board converts back
//! (`WireSession → Session`), degrading any legacy or evidence-less Attention to
//! "Input required, details on the source machine" rather than inventing an
//! explanation.
//!
//! The currency is one JSON object per event:
//!
//! - **Collector → Relay** (push): newline-delimited JSON — one `WireEvent` per line.
//! - **Relay → Board** (fan-out): Server-Sent Events, the `WireEvent` JSON in each
//!   `data:` field.
//!
//! A single shared token gates both roles, presented as `Authorization: Bearer …`.
//! The `x-riku-attn` header negotiates the Attention protocol capability (§ADR 0010):
//! a peer that omits it is treated as legacy and its Attention degrades safely.

use axum::http::{header::AUTHORIZATION, HeaderMap};
use axum::response::sse::Event as SseEvent;
use chrono::{DateTime, Utc};
use sessions::{Attention, AttentionCause, DiffStat, Event, Session, Status, Tool};
use serde::{Deserialize, Serialize};

/// The `Authorization` scheme prefix for the shared token.
const BEARER: &str = "Bearer ";

/// The header that advertises a peer's Attention-protocol capability. A Collector
/// (on `POST /collect`) and a board (on `GET /subscribe`) send it; a peer that omits
/// it is a legacy (pre-ADR-0010) component whose Attention must degrade.
pub const CAPABILITY_HEADER: &str = "x-riku-attn";

/// The current Attention-protocol capability value: typed lifecycle + evidence.
pub const CAPABILITY_V2: &str = "2";

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

/// Whether `headers` advertise the ADR 0010 Attention capability. A peer that does
/// not is treated as legacy (its richer Attention state degrades).
pub fn speaks_v2(headers: &HeaderMap) -> bool {
    headers
        .get(CAPABILITY_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == CAPABILITY_V2)
}

/// The Attention a Session carries on the wire: the typed cause, Attention Since,
/// and only the **remote** (allowlisted-structured-fields) evidence. Local display
/// evidence is structurally absent, so it cannot cross the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireAttention {
    pub cause: AttentionCause,
    pub since: DateTime<Utc>,
    /// The allowlisted remote evidence, or `None` when nothing was safe to send.
    #[serde(default)]
    pub evidence: Option<String>,
    /// The allowlisted fields could not explain the need — the board points at the
    /// source machine rather than showing a guess.
    #[serde(default)]
    pub details_on_source: bool,
}

/// The Session as it crosses the Relay: every scalar field of [`Session`] plus a
/// wire-safe [`WireAttention`]. Distinct from `Session` by design (ADR 0010) so the
/// compiler prevents rich local state from becoming network payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSession {
    pub id: String,
    pub tool: Tool,
    pub project: String,
    pub model: Option<String>,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub activity: Option<String>,
    pub last_event_at: DateTime<Utc>,
    pub status: Status,
    #[serde(default)]
    pub attention: Option<WireAttention>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub diff: Option<DiffStat>,
    #[serde(default)]
    pub machine: Option<String>,
    /// A legacy (pre-ADR-0010) Collector sends `attentionReason` ("waiting" |
    /// "error") in place of `attention`. Captured only so the board can detect and
    /// degrade legacy Attention; never re-serialized onto the wire.
    #[serde(
        default,
        rename = "attentionReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub legacy_attention_reason: Option<String>,
}

impl From<Session> for WireSession {
    /// Project a local Session onto the wire. Only the **remote** evidence is
    /// carried; the local `evidence` field is never read, so it cannot leak.
    fn from(s: Session) -> Self {
        let attention = s.attention.map(|a| WireAttention {
            cause: a.cause,
            since: a.since,
            details_on_source: a.remote_evidence.is_none(),
            evidence: a.remote_evidence,
        });
        WireSession {
            id: s.id,
            tool: s.tool,
            project: s.project,
            model: s.model,
            branch: s.branch,
            cwd: s.cwd,
            tokens_in: s.tokens_in,
            tokens_out: s.tokens_out,
            activity: s.activity,
            last_event_at: s.last_event_at,
            status: s.status,
            attention,
            cost_usd: s.cost_usd,
            diff: s.diff,
            machine: s.machine,
            legacy_attention_reason: None,
        }
    }
}

impl From<WireSession> for Session {
    /// Rebuild a Session for the board from a wire session. The wire's remote
    /// evidence becomes the board's display evidence; a legacy or evidence-less
    /// Attention degrades to Input required with details only on the source machine.
    fn from(w: WireSession) -> Self {
        let attention = degrade_attention(
            w.attention,
            w.legacy_attention_reason.as_deref(),
            w.status,
            w.last_event_at,
        );
        Session {
            id: w.id,
            tool: w.tool,
            project: w.project,
            model: w.model,
            branch: w.branch,
            cwd: w.cwd,
            tokens_in: w.tokens_in,
            tokens_out: w.tokens_out,
            activity: w.activity,
            last_event_at: w.last_event_at,
            status: w.status,
            attention,
            cost_usd: w.cost_usd,
            diff: w.diff,
            machine: w.machine,
        }
    }
}

/// Resolve the board-side Attention from a wire session: a v2 Attention is carried
/// through (its remote evidence becomes local display evidence); a legacy peer's
/// `attentionReason`, or an Attention status with no structured Attention at all,
/// degrades to Input required with `details_on_source` set — an honest generic
/// cause rather than fabricated evidence, identity, or timing (ADR 0010).
fn degrade_attention(
    attention: Option<WireAttention>,
    legacy_reason: Option<&str>,
    status: Status,
    last_event_at: DateTime<Utc>,
) -> Option<Attention> {
    if let Some(a) = attention {
        return Some(Attention {
            cause: a.cause,
            since: a.since,
            evidence: a.evidence,
            details_on_source: a.details_on_source,
            remote_evidence: None,
        });
    }
    // A legacy Attention (old `attentionReason`, or a bare Attention status) has no
    // trustworthy cause, evidence, or since — degrade rather than fabricate.
    if legacy_reason.is_some() || status == Status::Attention {
        return Some(Attention {
            cause: AttentionCause::Input,
            since: last_event_at,
            evidence: None,
            details_on_source: true,
            remote_evidence: None,
        });
    }
    None
}

/// A change on the wire — the wire-typed analog of [`sessions::Event`].
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WireEvent {
    Upsert(WireSession),
    Removed { id: String },
}

impl From<Event> for WireEvent {
    fn from(e: Event) -> Self {
        match e {
            Event::Upsert(s) => WireEvent::Upsert(s.into()),
            Event::Removed { id } => WireEvent::Removed { id },
        }
    }
}

impl From<WireEvent> for Event {
    fn from(e: WireEvent) -> Self {
        match e {
            WireEvent::Upsert(s) => Event::Upsert(s.into()),
            WireEvent::Removed { id } => Event::Removed { id },
        }
    }
}

/// Encode a `WireEvent` for the Relay→Board SSE fan-out: the whole event JSON in the
/// `data:` field. The subscriber decodes it directly (see [`SseDecoder`]).
pub fn to_sse(event: &WireEvent) -> SseEvent {
    SseEvent::default()
        .json_data(event)
        .expect("WireEvent serializes to JSON")
}

/// Encode a `WireEvent` as one NDJSON line (trailing `\n`) for the Collector→Relay push.
pub fn to_ndjson_line(event: &WireEvent) -> String {
    let mut s = serde_json::to_string(event).expect("WireEvent serializes to JSON");
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

/// Decodes an NDJSON `WireEvent` stream (the Collector→Relay push body). Each
/// non-empty line is one event; a line that fails to parse is skipped rather than
/// aborting the connection, so one malformed frame cannot desync the whole machine.
#[derive(Default)]
pub struct NdjsonDecoder {
    lines: LineBuffer,
}

impl NdjsonDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<WireEvent> {
        self.lines
            .push(bytes)
            .into_iter()
            .filter_map(|line| decode_event(&line))
            .collect()
    }
}

/// Decodes the Relay→Board SSE stream back into `WireEvent`s. Only `data:` fields
/// carry payload; comment keep-alive lines (`: ping`) and any other SSE fields are
/// ignored.
#[derive(Default)]
pub struct SseDecoder {
    lines: LineBuffer,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<WireEvent> {
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

/// Parse one JSON line as a `WireEvent`, warning (not failing) on a malformed frame.
fn decode_event(line: &str) -> Option<WireEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    match serde_json::from_str::<WireEvent>(line) {
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
    use axum::http::{HeaderMap, HeaderValue};
    use chrono::Utc;

    fn wire_session(id: &str, machine: &str) -> WireSession {
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
            attention: None,
            cost_usd: None,
            diff: None,
            machine: Some(machine.into()),
        }
        .into()
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
    fn capability_header_is_recognized() {
        let mut headers = HeaderMap::new();
        assert!(!speaks_v2(&headers));
        headers.insert(CAPABILITY_HEADER, HeaderValue::from_static("2"));
        assert!(speaks_v2(&headers));
    }

    #[test]
    fn local_evidence_never_reaches_the_wire() {
        // A Session with rich local evidence but a stricter remote rendering.
        let mut s = wire_session("a1", "desk");
        let base: Session = s.clone().into();
        let mut base = base;
        base.attention = Some(Attention {
            cause: AttentionCause::Approval,
            since: Utc::now(),
            evidence: Some("exec: rm -rf /tmp/secretstuff".into()),
            details_on_source: false,
            remote_evidence: Some("exec".into()),
        });
        base.status = Status::Attention;
        s = base.into();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("exec"));
        assert!(!json.contains("secretstuff")); // local args never serialized
        // The wire attention carries only the allowlisted remote evidence.
        assert_eq!(s.attention.as_ref().unwrap().evidence.as_deref(), Some("exec"));
    }

    #[test]
    fn legacy_attention_reason_degrades_to_input_details_on_source() {
        // A pre-ADR-0010 Collector's session: `attentionReason` + attention status,
        // no structured `attention`.
        let legacy = serde_json::json!({
            "id": "old1", "tool": "claude", "project": "p",
            "model": null, "branch": null, "cwd": null,
            "tokensIn": 0, "tokensOut": 0, "activity": null,
            "lastEventAt": "2026-07-19T10:00:00Z",
            "status": "attention", "attentionReason": "waiting"
        })
        .to_string();
        let wire: WireSession = serde_json::from_str(&legacy).unwrap();
        let session: Session = wire.into();
        let a = session.attention.unwrap();
        assert_eq!(a.cause, AttentionCause::Input);
        assert!(a.details_on_source);
        assert_eq!(a.evidence, None); // never fabricated
    }

    #[test]
    fn ndjson_round_trips_events_across_a_split_chunk() {
        let up = WireEvent::Upsert(wire_session("a1", "desk"));
        let rm = WireEvent::Removed { id: "a1".into() };
        let mut wire = to_ndjson_line(&up);
        wire.push_str(&to_ndjson_line(&rm));

        let (head, tail) = wire.as_bytes().split_at(wire.len() / 3);
        let mut dec = NdjsonDecoder::default();
        let mut events = dec.push(head);
        events.extend(dec.push(tail));

        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0], WireEvent::Upsert(s) if s.id == "a1" && s.machine.as_deref() == Some("desk"))
        );
        assert!(matches!(&events[1], WireEvent::Removed { id } if id == "a1"));
    }

    #[test]
    fn sse_decoder_skips_pings_and_extracts_data() {
        let json = serde_json::to_string(&WireEvent::Upsert(wire_session("b2", "mate"))).unwrap();
        let framed = format!(": ping\n\ndata: {json}\n\n");

        let (head, tail) = framed.as_bytes().split_at(framed.len() / 2);
        let mut dec = SseDecoder::default();
        let mut events = dec.push(head);
        events.extend(dec.push(tail));

        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], WireEvent::Upsert(s) if s.id == "b2"));
    }
}
