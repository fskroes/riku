//! Collector: the Session Sources for Claude Code and Codex CLI.
//!
//! Each tool plugs in behind [`SessionSource`]: it discovers its transcripts and
//! decodes their lines, while the byte-offset tailing, status heuristic, and
//! [`Session`] shape are shared. The store folds every source's transcripts into
//! [`Session`]s for the board. Runtime- and transport-agnostic: the store and
//! watcher expose plain callbacks so the board crate can wire them to whatever
//! async runtime it uses.

mod codex;
mod fold;
mod model;
mod parse;
mod session;
mod source;
mod store;
mod watch;
mod work;

use std::path::PathBuf;

pub use fold::{Fold, Projection, ACTIVITY_WINDOW};
pub use model::{Session, Status, Tool};
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
