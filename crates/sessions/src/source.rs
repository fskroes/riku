//! The Session Source abstraction: one implementation per agent tool. A source
//! knows where its transcripts live ([`roots`](SessionSource::roots)), which paths
//! it owns ([`owns`](SessionSource::owns)), and how to decode one — it hands back a
//! fresh [`Fold`] per file. Discovery + line decoding are the only per-source
//! concerns; tailing and status are shared (see `fold.rs` / `session.rs`).

use std::path::{Path, PathBuf};

use crate::codex::CodexFold;
use crate::fold::Fold;
use crate::session::{Accumulator, ClaudeSubAgentFold};

/// An adapter that discovers and reads Agent Sessions for one agent tool.
pub trait SessionSource: Send + Sync {
    /// Directories to scan on startup and watch for changes. Each is unique to the
    /// source, so a path under one root belongs unambiguously to that source.
    fn roots(&self) -> Vec<PathBuf>;
    /// Whether `path` is a transcript this source should ingest.
    fn owns(&self, path: &Path) -> bool;
    /// A fresh fold for the transcript at `path`.
    ///
    /// The source decides here which of the two things it is about to read — an
    /// Agent Session transcript or a Sub-agent transcript — and hands back the fold
    /// that produces the matching projection. It is given the path (rather than
    /// deciding line by line) so that decision, and any sibling file the fold needs
    /// pre-loaded, stay outside line handling: a [`Fold`] is pure over lines.
    fn new_fold(&self, path: &Path) -> Box<dyn Fold>;
}

/// Claude Code: flat `<root>/<project-dir>/<uuid>.jsonl` transcripts under
/// `~/.claude/projects`, with each Sub-agent written to its own file at
/// `<project-dir>/<root-uuid>/subagents/agent-<agentId>.jsonl`.
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

    /// Claude states which of the two a file is in the path itself, so the fold is
    /// chosen before a single line is read.
    fn new_fold(&self, path: &Path) -> Box<dyn Fold> {
        match claude_sub_agent_fold(path) {
            Some(fold) => Box::new(fold),
            None => Box::new(Accumulator::default()),
        }
    }
}

/// The Sub-agent fold a Claude path calls for, or `None` for an Agent Session
/// transcript.
///
/// Claude writes each Sub-agent to `<project>/<root-uuid>/subagents/agent-<agentId>.jsonl`,
/// in a flat directory (a depth-2 child sits beside its depth-1 spawner), so the
/// path alone both classifies the file and names both ids. The containing directory
/// is the root's id, and it is authoritative: a child entry names the root too, but
/// letting a file's *contents* decide which card its row lands on is the one input
/// to the cross-file join that a file could steer.
///
/// Beside the transcript sits `<stem>.meta.json`, the sidecar Claude writes at
/// spawn. Its **path** is handed to the fold here — the sibling file the
/// [`new_fold`](SessionSource::new_fold) seam exists to let a source resolve — and
/// the fold reads it immediately. It re-reads only while the sidecar has still told
/// it nothing, since the watcher may sight the transcript first and a Sub-agent that
/// never learns its Errand also never joins its parent's spawn record.
///
/// The **directory** is what classifies: anything Claude writes under `subagents/`
/// is a Sub-agent's, whatever it comes to name the file. The `agent-` prefix only
/// names the id, so a file without it keeps its whole stem as the id rather than
/// falling through to the Agent Session fold — which would be the one outcome that
/// puts a Sub-agent on the board as a card.
fn claude_sub_agent_fold(path: &Path) -> Option<ClaudeSubAgentFold> {
    let stem = path.file_stem()?.to_str()?;
    let dir = path.parent()?;
    if dir.file_name()?.to_str()? != "subagents" {
        return None;
    }
    let root_session_id = dir.parent()?.file_name()?.to_str()?.to_string();
    let agent_id = stem.strip_prefix("agent-").unwrap_or(stem).to_string();
    let meta_path = dir.join(format!("{stem}.meta.json"));
    Some(ClaudeSubAgentFold::new(
        agent_id,
        root_session_id,
        meta_path,
    ))
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

    /// Codex's subagent rollouts are not path-distinguishable — same date-nested
    /// directory, same `rollout-` name as an Agent Session's — so the path says
    /// nothing here and the classification comes from the rollout's own
    /// `session_meta` (`thread_source`). The fold states the outcome all the same.
    fn new_fold(&self, _path: &Path) -> Box<dyn Fold> {
        Box::new(CodexFold::default())
    }
}
