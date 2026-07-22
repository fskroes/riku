//! The shared Attention lifecycle reducer and the one-way privacy boundary.
//!
//! ADR 0010 puts all lifecycle policy in one place: Session Sources translate
//! tool-specific transcript records into normalized [`Observation`]s, and
//! [`AttentionReducer`] folds those into the single current [`Attention`] — owning
//! its identity, replacement, correlated resolution, and Attention Since. No source
//! reimplements this; a source only decides *which* observation a record is.
//!
//! Evidence crosses a one-way boundary here, on the source machine. A source hands
//! the reducer a raw [`NeedEvidence`] candidate; the reducer immediately renders it
//! into a bounded, sanitized **local** excerpt and a stricter allowlisted **remote**
//! rendering, then drops the raw candidate. Local-domain [`Attention`] keeps both;
//! the Collector projects only the remote rendering onto the wire, and rich local
//! evidence never becomes network payload.

use std::borrow::Cow;

use chrono::{DateTime, Utc};

use crate::model::AttentionCause;

/// Longest evidence excerpt retained or transported. Enforced *before* state
/// retention and transport (ADR 0010): an oversized transcript record cannot bloat
/// memory or the wire. The UI's line clamp is a separate presentation concern.
const EVIDENCE_MAX_CHARS: usize = 240;

/// The marker a redacted sensitive value is replaced with in display evidence.
const REDACTED: &str = "‹redacted›";

/// A normalized lifecycle observation a Session Source translates a transcript
/// record into. The reducer owns everything downstream — sources carry no policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// A new (or restated) human need. `key` is the source's stable correlation id
    /// for the need — a matching [`Resolved`](Self::Resolved) clears it, and a
    /// *different* key replaces the current need (resetting Attention Since). `at`
    /// is when the need began, or `None` when the record carries no timestamp.
    Need {
        key: String,
        cause: AttentionCause,
        evidence: NeedEvidence,
        at: Option<DateTime<Utc>>,
    },
    /// The need with this `key` was answered, cancelled, or withdrawn — a
    /// correlated resolution. Clears the current need only if the key matches.
    Resolved { key: String },
    /// Resumed or completed work supersedes any current need (recovery is free).
    /// Sources emit this only for genuine forward progress, never for uninformative
    /// activity, so Attention Since stays stable through transcript noise.
    Superseded,
}

/// The raw evidence candidate a source extracts for a need. Rendered once by the
/// reducer into local + remote display forms and then discarded — it is never
/// retained. Distinct variants let the allowlist decide, per kind, what may cross
/// the wire (a tool *name* may; its arguments, prose, and error output may not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedEvidence {
    /// A tool the agent is calling: the tool `name` (allowlisted) plus optional
    /// argument `detail` (local only).
    Tool { name: String, detail: Option<String> },
    /// A pending approval: a structured `kind` label (allowlisted) plus optional
    /// command `detail` (local only).
    Approval { kind: String, detail: Option<String> },
    /// An error ending: bounded error `text` (local only — error output is never
    /// allowlisted for the wire).
    Error { text: Option<String> },
    /// A free-text prompt/question (local only — prose is never allowlisted).
    Prompt { text: String },
    /// No safe excerpt is available.
    None,
}

/// The reducer's current need, before Attention Since is resolved against a file's
/// mtime fallback. Carried on the [`Projection`](crate::fold::Projection) so the
/// shared builder can stamp a concrete `since`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAttention {
    pub cause: AttentionCause,
    /// When the need began, or `None` if the source recorded no timestamp (the
    /// builder then falls back to the file mtime).
    pub since: Option<DateTime<Utc>>,
    /// Bounded, sanitized, source-faithful local display evidence, or `None`.
    pub evidence: Option<String>,
    /// The allowlisted rendering the Collector projects onto the wire, or `None`.
    pub remote_evidence: Option<String>,
}

/// One shared lifecycle reducer per transcript fold. Owns the single current need.
#[derive(Debug, Default)]
pub struct AttentionReducer {
    current: Option<Need>,
}

#[derive(Debug)]
struct Need {
    key: String,
    cause: AttentionCause,
    since: Option<DateTime<Utc>>,
    evidence: Option<String>,
    remote_evidence: Option<String>,
}

impl AttentionReducer {
    /// Fold one observation into the current Attention.
    pub fn apply(&mut self, obs: Observation) {
        match obs {
            Observation::Need {
                key,
                cause,
                evidence,
                at,
            } => {
                // A restatement of the current need leaves Attention Since (and its
                // evidence) untouched — repeated or duplicate records are inert.
                if self.current.as_ref().is_some_and(|c| c.key == key) {
                    return;
                }
                // A different need replaces the current one and resets since. The
                // raw candidate is rendered here and then dropped.
                let (evidence, remote_evidence) = render_evidence(evidence);
                self.current = Some(Need {
                    key,
                    cause,
                    since: at,
                    evidence,
                    remote_evidence,
                });
            }
            Observation::Resolved { key } => {
                if self.current.as_ref().is_some_and(|c| c.key == key) {
                    self.current = None;
                }
            }
            Observation::Superseded => self.current = None,
        }
    }

    /// The current need, or `None` when the session needs nothing from the human.
    pub fn current(&self) -> Option<PendingAttention> {
        self.current.as_ref().map(|c| PendingAttention {
            cause: c.cause,
            since: c.since,
            evidence: c.evidence.clone(),
            remote_evidence: c.remote_evidence.clone(),
        })
    }

    /// Drop all state (the transcript was truncated or rewritten).
    pub fn reset(&mut self) {
        self.current = None;
    }
}

/// Render a raw candidate into `(local, remote)` display evidence, enforcing the
/// sanitize/allowlist/bound rules before anything is retained. The candidate is
/// consumed, never kept.
fn render_evidence(ev: NeedEvidence) -> (Option<String>, Option<String>) {
    match ev {
        NeedEvidence::Tool { name, detail } => {
            let local = join_detail(&name, detail.as_deref());
            // Allowlisted: the tool name is a structured field, never its arguments.
            (local_evidence(&local), remote_evidence(&name))
        }
        NeedEvidence::Approval { kind, detail } => {
            let local = join_detail(&kind, detail.as_deref());
            // Allowlisted: the approval kind label only, never the command/args.
            (local_evidence(&local), remote_evidence(&kind))
        }
        // Error output and free prose are never allowlisted for the wire, so remote
        // evidence is absent and a relayed card degrades to "details on source".
        NeedEvidence::Error { text } => (text.as_deref().and_then(local_evidence), None),
        NeedEvidence::Prompt { text } => (local_evidence(&text), None),
        NeedEvidence::None => (None, None),
    }
}

/// `"<head>: <detail>"`, or just `head` when there is no detail.
fn join_detail(head: &str, detail: Option<&str>) -> String {
    match detail {
        Some(d) if !d.trim().is_empty() => format!("{head}: {d}"),
        _ => head.to_string(),
    }
}

/// Sanitize + bound a source-faithful local excerpt; `None` when nothing survives.
fn local_evidence(s: &str) -> Option<String> {
    nonempty(bound(&sanitize(s)))
}

/// Bound an allowlisted structured field for the wire; `None` when empty. Already
/// structured, so only bounding (and a defensive sanitize) applies.
fn remote_evidence(s: &str) -> Option<String> {
    nonempty(bound(&sanitize(s)))
}

fn nonempty(s: String) -> Option<String> {
    (!s.trim().is_empty()).then_some(s)
}

/// Clamp to [`EVIDENCE_MAX_CHARS`] characters, ellipsizing an overrun.
fn bound(s: &str) -> String {
    if s.chars().count() <= EVIDENCE_MAX_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(EVIDENCE_MAX_CHARS - 1).collect();
    out.push('…');
    out
}

/// Redact recognized sensitive values from a display excerpt so secrets on the
/// owner's own board are not echoed back at a glance (ADR 0010, User Story 17).
/// A best-effort safety net, not an authorization boundary: it redacts obvious
/// secret tokens (`sk-…`, provider-prefixed keys, long opaque strings), the value
/// of a `key=value`/`key: value` pair whose key names a secret, and the token that
/// follows a `Bearer`/`Authorization` marker. Original whitespace is preserved so
/// the excerpt stays source-faithful.
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut redact_next = false;
    for piece in s.split_inclusive(char::is_whitespace) {
        let core_len = piece.trim_end_matches(char::is_whitespace).len();
        let (word, ws) = piece.split_at(core_len);
        if word.is_empty() {
            out.push_str(ws);
            continue;
        }
        if introduces_secret(word) {
            // e.g. `Authorization:` / `Bearer` — a marker whose following token is
            // the secret. Markers chain (`Authorization: Bearer <token>`), so a
            // marker is never itself consumed as the redaction target.
            out.push_str(word);
            redact_next = true;
        } else if redact_next {
            out.push_str(&redact_keeping_wrap(word));
            redact_next = false;
        } else {
            out.push_str(&redact_word(word));
        }
        out.push_str(ws);
    }
    out
}

/// Redact a word's token body while preserving any surrounding punctuation (so a
/// quoted `'…secret'` keeps its quotes). Used for the token that follows a marker.
fn redact_keeping_wrap(word: &str) -> String {
    let is_body =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '+' | '.');
    match (word.find(is_body), word.rfind(is_body)) {
        (Some(start), Some(last)) => {
            let end = last + 1; // body chars are ASCII, so one byte wide.
            format!("{}{REDACTED}{}", &word[..start], &word[end..])
        }
        _ => word.to_string(),
    }
}

/// Whether a word is a marker whose following token is a secret to redact.
fn introduces_secret(word: &str) -> bool {
    let w = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    w.eq_ignore_ascii_case("bearer") || w.eq_ignore_ascii_case("authorization")
}

/// Redact one whitespace-bounded word if it is (or contains) a secret.
fn redact_word(word: &str) -> Cow<'_, str> {
    // `key=value` / `key: value` with a sensitive key → redact the value.
    for sep in ['=', ':'] {
        if let Some(i) = word.find(sep) {
            let key = &word[..i];
            let val = &word[i + sep.len_utf8()..];
            if !val.trim().is_empty() && is_sensitive_key(key) {
                return Cow::Owned(format!("{key}{sep}{REDACTED}"));
            }
        }
    }
    if looks_secret(word) {
        return Cow::Owned(REDACTED.to_string());
    }
    Cow::Borrowed(word)
}

/// Whether a `key=value` key names a secret (case- and punctuation-insensitive).
fn is_sensitive_key(key: &str) -> bool {
    let key: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    const SENSITIVE: [&str; 11] = [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "authorization",
        "auth",
        "credential",
        "credentials",
        "privatekey",
        "accesskey",
    ];
    SENSITIVE.iter().any(|s| key.contains(s))
}

/// Whether a bare word looks like an opaque secret token.
fn looks_secret(word: &str) -> bool {
    // Trim surrounding quotes/brackets/punctuation, keeping secret-ish body chars.
    let w = word.trim_matches(|c: char| {
        !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '/' | '+')
    });
    if w.len() < 16 {
        return false;
    }
    const PREFIXES: [&str; 6] = ["sk-", "sk_", "ghp_", "gho_", "xox", "aws_"];
    if PREFIXES
        .iter()
        .any(|p| w.to_ascii_lowercase().starts_with(p))
    {
        return true;
    }
    // A long, unbroken run of opaque token characters with no natural-language
    // punctuation and at least one digit reads as a key/hash, not prose.
    w.len() >= 24
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '+'))
        && w.chars().any(|c| c.is_ascii_digit())
        && w.chars().any(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn need(key: &str, cause: AttentionCause) -> Observation {
        Observation::Need {
            key: key.into(),
            cause,
            evidence: NeedEvidence::None,
            at: Some(ts("2026-07-19T10:00:00Z")),
        }
    }

    #[test]
    fn a_different_need_replaces_and_resets_since() {
        let mut r = AttentionReducer::default();
        r.apply(need("k1", AttentionCause::Input));
        r.apply(Observation::Need {
            key: "k2".into(),
            cause: AttentionCause::Approval,
            evidence: NeedEvidence::None,
            at: Some(ts("2026-07-19T11:00:00Z")),
        });
        let cur = r.current().unwrap();
        assert_eq!(cur.cause, AttentionCause::Approval);
        assert_eq!(cur.since, Some(ts("2026-07-19T11:00:00Z")));
    }

    #[test]
    fn restating_the_same_need_keeps_since_stable() {
        let mut r = AttentionReducer::default();
        r.apply(need("k1", AttentionCause::Input));
        r.apply(Observation::Need {
            key: "k1".into(),
            cause: AttentionCause::Input,
            evidence: NeedEvidence::None,
            at: Some(ts("2026-07-19T12:00:00Z")),
        });
        assert_eq!(
            r.current().unwrap().since,
            Some(ts("2026-07-19T10:00:00Z"))
        );
    }

    #[test]
    fn resolution_clears_only_the_matching_need() {
        let mut r = AttentionReducer::default();
        r.apply(need("k1", AttentionCause::Input));
        r.apply(Observation::Resolved { key: "other".into() });
        assert!(r.current().is_some());
        r.apply(Observation::Resolved { key: "k1".into() });
        assert!(r.current().is_none());
    }

    #[test]
    fn superseded_clears_any_need() {
        let mut r = AttentionReducer::default();
        r.apply(need("k1", AttentionCause::Error));
        r.apply(Observation::Superseded);
        assert!(r.current().is_none());
    }

    #[test]
    fn tool_evidence_keeps_name_on_the_wire_but_not_args() {
        let (local, remote) = render_evidence(NeedEvidence::Tool {
            name: "Bash".into(),
            detail: Some("rm -rf /tmp/secret".into()),
        });
        assert_eq!(local.as_deref(), Some("Bash: rm -rf /tmp/secret"));
        assert_eq!(remote.as_deref(), Some("Bash")); // no args cross the wire
    }

    #[test]
    fn error_and_prompt_have_no_remote_evidence() {
        let (local, remote) = render_evidence(NeedEvidence::Error {
            text: Some("panic: index out of bounds".into()),
        });
        assert_eq!(local.as_deref(), Some("panic: index out of bounds"));
        assert_eq!(remote, None);

        let (local, remote) = render_evidence(NeedEvidence::Prompt {
            text: "Which database should I migrate?".into(),
        });
        assert_eq!(local.as_deref(), Some("Which database should I migrate?"));
        assert_eq!(remote, None);
    }

    #[test]
    fn sanitize_redacts_recognized_secrets() {
        assert_eq!(
            sanitize("export OPENAI_API_KEY=sk-abcd1234efgh5678ijkl"),
            "export OPENAI_API_KEY=‹redacted›"
        );
        assert_eq!(
            sanitize("curl -H 'Authorization: Bearer sk-livetoken1234567890'"),
            "curl -H 'Authorization: Bearer ‹redacted›'"
        );
        assert_eq!(
            sanitize("password=hunter2 and go"),
            "password=‹redacted› and go"
        );
        // A long opaque hex/base64 token is redacted on its own.
        assert_eq!(
            sanitize("token 0a1b2c3d4e5f60718293a4b5c6d7e8f9"),
            "token ‹redacted›"
        );
    }

    #[test]
    fn sanitize_leaves_ordinary_prose_intact() {
        let s = "Which database should I migrate first?";
        assert_eq!(sanitize(s), s);
    }

    #[test]
    fn evidence_is_bounded_before_retention() {
        let long = "x".repeat(1000);
        let (local, _) = render_evidence(NeedEvidence::Prompt { text: long });
        assert_eq!(local.unwrap().chars().count(), EVIDENCE_MAX_CHARS);
    }
}
