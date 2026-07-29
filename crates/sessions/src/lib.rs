//! Sessions: the Session Sources for Claude Code and Codex CLI.
//!
//! Each tool plugs in behind [`SessionSource`]: it discovers its transcripts and
//! decodes their lines, while the byte-offset tailing, status heuristic, and
//! [`Session`] shape are shared. The store folds every source's transcripts into
//! [`Session`]s for the board. Runtime- and transport-agnostic: the store and
//! watcher expose plain callbacks so the board crate can wire them to whatever
//! async runtime it uses.

mod attention;
mod codex;
mod deeplink;
mod diff;
mod fold;
mod git;
mod journal;
mod journal_store;
mod liveness;
mod model;
mod parse;
mod pricing;
mod session;
mod source;
mod store;
mod watch;
mod work;

use std::path::PathBuf;

pub use attention::{AttentionReducer, NeedEvidence, Observation, PendingAttention};
pub use deeplink::DeepLink;
pub use diff::DiffCache;
pub use fold::{Fold, Folded, Projection, SubAgentProjection, ACTIVITY_WINDOW};
pub use git::diff_stat;
pub use journal::{
    project_slug, Handoff, Journal, JournalDay, JournalEntry, JournalReading, Resume, Voice,
    JOURNAL_VERSION,
};
pub use journal_store::{
    append_note, append_note_in, journal_path, list_journals, list_journals_in, purge_journals,
    read_journal, read_journal_in, resolve_journal_project, Noted,
};
pub use liveness::{probe_alive_cwds, ProcessLiveness};
pub use model::{Attention, AttentionCause, DiffStat, Session, Status, SubAgents, Tool};
pub use session::{Accumulator, FileState};
pub use source::{ClaudeSource, CodexSource, SessionSource};
pub use store::{Event, SessionStore, DISCOVERY_WINDOW};
pub use watch::{watch, Change, WatchGuard};
pub use work::{read_work_map, WorkItem, WorkMap, WorkSourceKind, WorkStatus};

/// The default Claude Code projects root: `~/.claude/projects`.
pub fn default_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// The default Codex CLI sessions root, honoring `CODEX_HOME` (default
/// `~/.codex`): `<CODEX_HOME>/sessions`.
pub fn codex_default_root() -> Option<PathBuf> {
    match std::env::var_os("CODEX_HOME") {
        Some(home) => Some(PathBuf::from(home).join("sessions")),
        None => dirs::home_dir().map(|h| h.join(".codex").join("sessions")),
    }
}
