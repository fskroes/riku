//! The Session Source abstraction: one implementation per agent tool. A source
//! knows where its transcripts live ([`roots`](SessionSource::roots)), which paths
//! it owns ([`owns`](SessionSource::owns)), and how to decode one — it hands back a
//! fresh [`Fold`] per file. Discovery + line decoding are the only per-source
//! concerns; tailing and status are shared (see `fold.rs` / `session.rs`).

use std::path::{Path, PathBuf};

use crate::codex::CodexFold;
use crate::fold::Fold;
use crate::session::Accumulator;

/// An adapter that discovers and reads Agent Sessions for one agent tool.
pub trait SessionSource: Send + Sync {
    /// Directories to scan on startup and watch for changes. Each is unique to the
    /// source, so a path under one root belongs unambiguously to that source.
    fn roots(&self) -> Vec<PathBuf>;
    /// Whether `path` is a transcript this source should ingest.
    fn owns(&self, path: &Path) -> bool;
    /// A fresh fold for one of this source's transcripts.
    fn new_fold(&self) -> Box<dyn Fold>;
}

/// Claude Code: flat `<root>/<project-dir>/<uuid>.jsonl` transcripts under
/// `~/.claude/projects`.
pub struct ClaudeSource {
    root: PathBuf,
}

impl ClaudeSource {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl SessionSource for ClaudeSource {
    fn roots(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }

    fn owns(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some("jsonl")
    }

    fn new_fold(&self) -> Box<dyn Fold> {
        Box::new(Accumulator::default())
    }
}

/// Codex CLI: date-nested `<root>/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl` rollouts
/// under `~/.codex/sessions` (honoring `CODEX_HOME`).
pub struct CodexSource {
    root: PathBuf,
}

impl CodexSource {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl SessionSource for CodexSource {
    fn roots(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }

    fn owns(&self, path: &Path) -> bool {
        matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some(name) if name.starts_with("rollout-") && name.ends_with(".jsonl")
        )
    }

    fn new_fold(&self) -> Box<dyn Fold> {
        Box::new(CodexFold::default())
    }
}
