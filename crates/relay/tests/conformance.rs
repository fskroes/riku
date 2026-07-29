//! The shared Attention conformance harness (ADR 0010).
//!
//! A single table-driven suite that drives every Session Source *and* the
//! Collector–Relay path through identical rules. Each case starts from a sequence of
//! raw transcript records, folds them through the real source (the primary seam:
//! transcript fixture → Session projection), then projects the result onto the wire
//! type the Collector would send. Assertions are on external behavior only — the
//! built Session and the bytes that would cross the wire — never reducer internals.
//!
//! Dimensions covered here: replacement, correlated resolution, deterministic
//! replay, duplicate records, malformed tails, and privacy (sanitization, allowlist,
//! discard of raw candidates). Mixed-version degradation is covered by
//! [`legacy_wire`]; reconnect convergence and atomic SSE updates are covered by
//! `tests/integration.rs` (snapshot-on-connect + whole-Session upserts) and asserted
//! atomically in [`upsert_is_atomic`].

use std::path::Path;

use chrono::{DateTime, Utc};
use relay::wire::{WireEvent, WireSession};
use sessions::{
    AttentionCause, ClaudeSource, CodexSource, FileState, Session, SessionSource, Status,
};

/// Which Session Source a case exercises.
#[derive(Clone, Copy)]
enum Src {
    Claude,
    Codex,
}

/// The externally-observable outcome a case asserts.
struct Expect {
    /// The typed cause on the built Session, or `None` for no Attention.
    cause: Option<AttentionCause>,
    /// A substring the **local** display evidence must contain (source machine).
    local_contains: Option<&'static str>,
    /// The exact **remote** (allowlisted) evidence the wire carries, if any.
    remote_evidence: Option<&'static str>,
    /// Substrings that must NOT appear anywhere in the wire JSON — the privacy
    /// negative space (arbitrary commands, arguments, prose, error output).
    wire_forbidden: &'static [&'static str],
}

struct Case {
    name: &'static str,
    src: Src,
    lines: Vec<String>,
    expect: Expect,
}

fn ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// A fresh fold for `src`. Every case here is an Agent Session transcript, so the
/// path is a placeholder shaped like one — a source reads it to decide which of the
/// two things it is about to fold, but nothing else about it matters to folding.
fn file_state(src: Src) -> FileState {
    let fold = match src {
        Src::Claude => ClaudeSource::new("/x".into()).new_fold(Path::new("/x/p/s.jsonl")),
        Src::Codex => {
            CodexSource::new("/x".into()).new_fold(Path::new("/x/2026/07/19/rollout-1.jsonl"))
        }
    };
    FileState::new(fold)
}

/// Fold `lines` (joined, newline-terminated) into a Session at a fixed clock. mtime
/// is recent so the status rule reflects Attention presence, not staleness.
fn project(src: Src, lines: &[String]) -> Option<Session> {
    let mut fs = file_state(src);
    let mut body = lines.join("\n");
    body.push('\n');
    fs.feed(body.as_bytes());
    fs.build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
}

// ---- Claude Code record builders -------------------------------------------------

fn c_tool_use(id: &str, tuid: &str, name: &str, input: serde_json::Value) -> String {
    serde_json::json!({
        "type": "assistant", "sessionId": id, "cwd": "/a/foo",
        "timestamp": "2026-07-19T10:00:00Z",
        "message": { "model": "m", "stop_reason": "tool_use",
            "content": [{ "type": "tool_use", "id": tuid, "name": name, "input": input }] }
    })
    .to_string()
}

fn c_tool_result(id: &str, tuid: &str) -> String {
    serde_json::json!({
        "type": "user", "sessionId": id, "cwd": "/a/foo",
        "message": { "content": [{ "type": "tool_result", "tool_use_id": tuid, "content": "ok" }] }
    })
    .to_string()
}

fn c_api_error(id: &str, text: &str) -> String {
    serde_json::json!({
        "type": "assistant", "isApiErrorMessage": true, "sessionId": id, "cwd": "/a/foo",
        "timestamp": "2026-07-19T10:01:00Z",
        "message": { "model": "<synthetic>", "content": [{ "type": "text", "text": text }] }
    })
    .to_string()
}

// ---- Codex CLI record builders ---------------------------------------------------

fn x_meta(id: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-07-19T10:00:00Z", "type": "session_meta",
        "payload": { "id": id, "cwd": "/a/foo", "thread_source": "user", "git": { "branch": "main" } }
    })
    .to_string()
}

fn x_event(kind: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-07-19T10:00:05Z", "type": "event_msg", "payload": { "type": kind }
    })
    .to_string()
}

fn x_approval(event: &str, call_id: &str, command: serde_json::Value) -> String {
    serde_json::json!({
        "timestamp": "2026-07-19T10:00:06Z", "type": event,
        "payload": { "call_id": call_id, "command": command }
    })
    .to_string()
}

fn x_assistant(text: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-07-19T10:00:07Z", "type": "response_item",
        "payload": { "type": "message", "role": "assistant",
            "content": [{ "type": "output_text", "text": text }] }
    })
    .to_string()
}

/// The full case table — every Session Source, every lifecycle rule.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "claude: pending tool_use → Input, tool name on the wire, args not",
            src: Src::Claude,
            lines: vec![c_tool_use(
                "s1",
                "toolu_1",
                "Bash",
                serde_json::json!({ "command": "deploy --secretdir /tmp/hunter2things" }),
            )],
            expect: Expect {
                cause: Some(AttentionCause::Input),
                local_contains: Some("deploy"),
                remote_evidence: Some("Bash"),
                wire_forbidden: &["deploy", "secretdir", "hunter2things"],
            },
        },
        Case {
            name: "claude: correlated resolution clears the wait",
            src: Src::Claude,
            lines: vec![
                c_tool_use("s1", "toolu_1", "Bash", serde_json::json!({})),
                c_tool_result("s1", "toolu_1"),
            ],
            expect: Expect {
                cause: None,
                local_contains: None,
                remote_evidence: None,
                wire_forbidden: &[],
            },
        },
        Case {
            name: "claude: a newer need replaces the current one",
            src: Src::Claude,
            lines: vec![
                c_tool_use("s1", "toolu_1", "Read", serde_json::json!({})),
                c_tool_result("s1", "toolu_1"),
                c_tool_use(
                    "s1",
                    "toolu_2",
                    "Grep",
                    serde_json::json!({ "pattern": "needle" }),
                ),
            ],
            expect: Expect {
                cause: Some(AttentionCause::Input),
                local_contains: Some("Grep"),
                remote_evidence: Some("Grep"),
                wire_forbidden: &["needle"],
            },
        },
        Case {
            name: "claude: API error → Session error, no error text on the wire",
            src: Src::Claude,
            lines: vec![c_api_error("s1", "panic: index 9 out of bounds")],
            expect: Expect {
                cause: Some(AttentionCause::Error),
                local_contains: Some("panic"),
                remote_evidence: None,
                wire_forbidden: &["panic", "out of bounds"],
            },
        },
        Case {
            name: "claude: duplicate tool_use records leave the need unchanged",
            src: Src::Claude,
            lines: vec![
                c_tool_use("s1", "toolu_1", "Bash", serde_json::json!({})),
                c_tool_use("s1", "toolu_1", "Bash", serde_json::json!({})),
            ],
            expect: Expect {
                cause: Some(AttentionCause::Input),
                local_contains: Some("Bash"),
                remote_evidence: Some("Bash"),
                wire_forbidden: &[],
            },
        },
        Case {
            name: "codex: approval → Approval, kind on the wire, command not",
            src: Src::Codex,
            lines: vec![
                x_meta("r1"),
                x_event("task_started"),
                x_approval(
                    "exec_approval_request",
                    "call_9",
                    serde_json::json!(["rm", "-rf", "/tmp/secretdir"]),
                ),
            ],
            expect: Expect {
                cause: Some(AttentionCause::Approval),
                local_contains: Some("rm -rf"),
                remote_evidence: Some("exec"),
                wire_forbidden: &["rm", "secretdir"],
            },
        },
        Case {
            name: "codex: approval answered by an assistant message clears",
            src: Src::Codex,
            lines: vec![
                x_meta("r1"),
                x_approval("exec_approval_request", "call_9", serde_json::json!(["ls"])),
                x_assistant("running the command"),
            ],
            expect: Expect {
                cause: None,
                local_contains: None,
                remote_evidence: None,
                wire_forbidden: &[],
            },
        },
        Case {
            name: "codex: turn_aborted → Session error",
            src: Src::Codex,
            lines: vec![x_meta("r1"), x_event("task_started"), x_event("turn_aborted")],
            expect: Expect {
                cause: Some(AttentionCause::Error),
                local_contains: None,
                remote_evidence: None,
                wire_forbidden: &[],
            },
        },
        Case {
            name: "codex: recovery — a new turn after an abort supersedes it",
            src: Src::Codex,
            lines: vec![x_meta("r1"), x_event("turn_aborted"), x_event("task_started")],
            expect: Expect {
                cause: None,
                local_contains: None,
                remote_evidence: None,
                wire_forbidden: &[],
            },
        },
    ]
}

#[test]
fn conformance_table() {
    for case in cases() {
        let session = project(case.src, &case.lines)
            .unwrap_or_else(|| panic!("[{}] produced no session", case.name));

        // Cause + status agree.
        let cause = session.attention.as_ref().map(|a| a.cause);
        assert_eq!(cause, case.expect.cause, "[{}] cause", case.name);
        let expect_attention = case.expect.cause.is_some();
        assert_eq!(
            session.status == Status::Attention,
            expect_attention,
            "[{}] status/attention agreement",
            case.name
        );

        // Local display evidence (source machine) contains the source's own words.
        if let Some(needle) = case.expect.local_contains {
            let local = session
                .attention
                .as_ref()
                .and_then(|a| a.evidence.as_deref())
                .unwrap_or("");
            assert!(
                local.contains(needle),
                "[{}] local evidence {local:?} missing {needle:?}",
                case.name
            );
        }

        // Project onto the wire the way the Collector would, then assert the remote
        // rendering and the privacy negative space.
        let wire: WireSession = session.clone().into();
        let wire_attention = wire.attention.as_ref();
        assert_eq!(
            wire_attention.and_then(|a| a.evidence.as_deref()),
            case.expect.remote_evidence,
            "[{}] remote evidence",
            case.name
        );
        // A card with a cause but no allowlisted evidence must point at the source.
        if expect_attention && case.expect.remote_evidence.is_none() {
            assert!(
                wire_attention.map(|a| a.details_on_source).unwrap_or(false),
                "[{}] evidence-less remote card must set details_on_source",
                case.name
            );
        }
        let wire_json = serde_json::to_string(&WireEvent::Upsert(wire)).unwrap();
        for forbidden in case.expect.wire_forbidden {
            assert!(
                !wire_json.contains(forbidden),
                "[{}] wire leaked {forbidden:?}: {wire_json}",
                case.name
            );
        }

        // Deterministic replay: re-folding the same records reproduces the Session.
        let replay = project(case.src, &case.lines).unwrap();
        assert_eq!(session, replay, "[{}] deterministic replay", case.name);
    }
}

#[test]
fn malformed_tail_is_ignored_and_committed_garbage_is_skipped() {
    // A committed malformed line is skipped; a genuine need still surfaces.
    let mut fs = file_state(Src::Claude);
    let mut body = String::from("{ not json at all }\n");
    body.push_str(&c_tool_use("s1", "toolu_1", "Bash", serde_json::json!({})));
    body.push('\n');
    // A trailing fragment with no newline is a mid-write line — deferred, not consumed.
    body.push_str("{\"type\":\"assis");
    fs.feed(body.as_bytes());

    let s = fs
        .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
        .unwrap();
    assert_eq!(
        s.attention.map(|a| a.cause),
        Some(AttentionCause::Input),
        "the good record survived the malformed tail"
    );
}

#[test]
fn legacy_wire_degrades_to_input_details_on_source() {
    // Mixed versions: a pre-ADR-0010 Collector's session (old `attentionReason`, no
    // structured `attention`) degrades to Input required with details only on the
    // source — never fabricated evidence, identity, or timing.
    let legacy = serde_json::json!({
        "id": "old-1", "tool": "claude", "project": "p",
        "model": null, "branch": null, "cwd": null,
        "tokensIn": 0, "tokensOut": 0, "activity": null,
        "lastEventAt": "2026-07-19T10:00:00Z",
        "status": "attention", "attentionReason": "error"
    })
    .to_string();

    let wire: WireSession = serde_json::from_str(&legacy).unwrap();
    let session: Session = wire.into();
    let a = session.attention.expect("degraded attention");
    assert_eq!(a.cause, AttentionCause::Input);
    assert!(a.details_on_source);
    assert_eq!(a.evidence, None);
}

#[test]
fn upsert_is_atomic() {
    // A reconnecting board converges via a whole-Session upsert: cause, since, and
    // evidence travel together in one frame, so a decode can never yield a torn card
    // (e.g. a new cause with stale evidence).
    let s = project(
        Src::Codex,
        &[
            x_meta("r1"),
            x_approval("exec_approval_request", "c1", serde_json::json!(["ls"])),
        ],
    )
    .unwrap();
    let wire: WireSession = s.into();
    let frame = serde_json::to_string(&WireEvent::Upsert(wire)).unwrap();

    let decoded: WireEvent = serde_json::from_str(&frame).unwrap();
    let WireEvent::Upsert(ws) = decoded else {
        panic!("expected an upsert");
    };
    let a = ws.attention.expect("attention present");
    // All three components decoded from the single atomic frame.
    assert_eq!(a.cause, AttentionCause::Approval);
    assert_eq!(a.evidence.as_deref(), Some("exec"));
    let _since: DateTime<Utc> = a.since;
}
