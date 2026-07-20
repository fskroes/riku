//! The Deep Link: how to re-enter an Agent Session on the machine it runs on.
//!
//! The board's one action on a session is to deep-link into the local session
//! (ADR 0002) — never to control it remotely. This module owns the *domain* half
//! of that: given a Session's tool, id, and working directory, what command
//! resumes it, and in which directory. Turning this into an actual terminal
//! window is the board's job (an OS mechanism, not domain), kept out of here so
//! the collector stays runtime- and platform-agnostic.

use std::path::{Path, PathBuf};

use crate::model::Tool;

/// A resolved way to re-open one Agent Session locally: the resume program and
/// its arguments, the directory to run them in (the session's `cwd`), and the
/// transcript file (a reveal-in-Finder fallback for a launcher that cannot run
/// the resume command).
///
/// The resume invocations mirror each CLI's documented resume-by-id form —
/// `claude --resume <id>` and `codex resume <id>`. If an installed CLI names its
/// resume flag differently, the launcher still opens a terminal in `dir`, so the
/// human lands in the right workspace regardless (a safe degradation, in the
/// spirit of the source adapters' honesty about unverified upstream shapes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepLink {
    /// The resume program to run (`claude` / `codex`).
    pub program: &'static str,
    /// Arguments that resume this specific session.
    pub args: Vec<String>,
    /// The working directory to resume in — the session's latest `cwd`.
    pub dir: PathBuf,
    /// The session's transcript file, for a reveal-only fallback.
    pub transcript: PathBuf,
}

impl DeepLink {
    /// Resolve the deep link for a session. Returns `None` when the session has no
    /// known `cwd`: without a directory there is nothing to resume *into*, and a
    /// resume command run in the wrong place would land in the wrong repo.
    pub fn resume(
        tool: Tool,
        session_id: &str,
        cwd: Option<&str>,
        transcript: &Path,
    ) -> Option<DeepLink> {
        let dir = PathBuf::from(cwd?);
        let (program, args) = match tool {
            Tool::Claude => (
                "claude",
                vec!["--resume".to_string(), session_id.to_string()],
            ),
            Tool::Codex => ("codex", vec!["resume".to_string(), session_id.to_string()]),
        };
        Some(DeepLink {
            program,
            args,
            dir,
            transcript: transcript.to_path_buf(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_resumes_by_session_id() {
        let link = DeepLink::resume(
            Tool::Claude,
            "sess-1",
            Some("/Users/x/repos/foo"),
            Path::new("/Users/x/.claude/projects/-foo/sess-1.jsonl"),
        )
        .unwrap();
        assert_eq!(link.program, "claude");
        assert_eq!(link.args, ["--resume", "sess-1"]);
        assert_eq!(link.dir, PathBuf::from("/Users/x/repos/foo"));
    }

    #[test]
    fn codex_resumes_by_session_id() {
        let link = DeepLink::resume(
            Tool::Codex,
            "rollout-1",
            Some("/Users/x/repos/bar"),
            Path::new("/Users/x/.codex/sessions/2026/07/19/rollout-1.jsonl"),
        )
        .unwrap();
        assert_eq!(link.program, "codex");
        assert_eq!(link.args, ["resume", "rollout-1"]);
        assert_eq!(link.dir, PathBuf::from("/Users/x/repos/bar"));
    }

    #[test]
    fn no_cwd_yields_no_deep_link() {
        assert!(DeepLink::resume(Tool::Claude, "sess-1", None, Path::new("/t.jsonl")).is_none());
    }
}
