//! Collector: the Session Source for Claude Code.
//!
//! Discovers Claude Code transcripts under `~/.claude/projects`, tails them
//! incrementally, and folds each into a [`Session`] for the board. Runtime- and
//! transport-agnostic: the store and watcher expose plain callbacks so the board
//! crate can wire them to whatever async runtime it uses.

mod model;
mod parse;
mod session;
mod store;
mod watch;

use std::path::PathBuf;

pub use model::{Session, Status};
pub use session::{Accumulator, FileState, ACTIVITY_WINDOW};
pub use store::{Event, SessionStore, DISCOVERY_WINDOW};
pub use watch::{watch, Change, WatchGuard};

/// The default projects root: `~/.claude/projects`.
pub fn default_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}
