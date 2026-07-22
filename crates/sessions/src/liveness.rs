//! Process liveness: ground truth for Running vs Finished (ADR pending).
//!
//! The mtime heuristic lies in two directions: a Ctrl-C'd session keeps a fresh
//! transcript for up to 15 minutes ("looks live but is dead"), and an alive agent
//! whose user is thinking goes quiet past the window ("looks finished but is
//! running"). This module observes the actual agent processes and reports the set
//! of working directories that currently host one, so the store can make status
//! ground truth where a match exists and fall back to mtime where it doesn't.
//!
//! Technique (ported, with its hard-won details, from a prior JS implementation):
//! one `ps` pass to find agent pids, then a **single batched** `lsof` call for all
//! pids with a 2-second timeout — per-pid calls with short timeouts fail under
//! load. Processes whose cwd is under `.claude/worktrees/agent-*` are subagent
//! duplicates and are skipped. The upstream TTY filter is deliberately **not**
//! ported: Conductor runs its agents without a controlling TTY (verified
//! empirically — they show `??`), so filtering on TTY would declare every
//! Conductor session dead. Instead, matching on the executable basename
//! (`claude`/`codex`) plus cwd matching bounds the search.
//!
//! macOS-only by design, like the rest of the project (BSD `ps`/`lsof` flags).

use std::collections::HashSet;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tracing::warn;

/// The per-session process verdict the status rule consumes.
///
/// `Unknown` covers every case without trustworthy data: no `cwd`, another
/// session in the same directory owns the liveness credit, or the probe failed —
/// the mtime heuristic then applies unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessLiveness {
    /// An agent process is running in the session's directory.
    Alive,
    /// No agent process for two consecutive probes (debounced).
    Dead,
    /// No liveness data — fall back to the mtime heuristic.
    #[default]
    Unknown,
}

/// Executable basenames that count as an agent process. Exact match, so
/// bystanders like Claude Desktop's "Claude Helper" never register.
const AGENT_BINARIES: [&str; 2] = ["claude", "codex"];

/// Budget for the single batched `lsof` call (and the `ps` pass before it).
/// Per-pid calls with short individual timeouts fail under load; one batched
/// call with one generous budget does not.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// A subagent's worktree — a duplicate of its parent session's process, never a
/// session of its own.
const SUBAGENT_WORKTREE_MARKER: &str = "/.claude/worktrees/agent-";

/// The working directories that currently host a live agent process.
///
/// `None` means the probe itself failed (timeout, missing tool) — the caller
/// must treat that as "no data this tick", never as "everything died". An empty
/// set is a real observation: no agent is running anywhere.
pub fn probe_alive_cwds() -> Option<HashSet<String>> {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let pids = agent_pids(deadline)?;
    if pids.is_empty() {
        return Some(HashSet::new());
    }
    let joined = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    // One batched call for every pid: `-a` ANDs the pid filter with `-d cwd`,
    // `-Fn` emits machine-parseable `p<pid>` / `n<path>` records.
    let out = run_with_deadline("lsof", &["-a", "-p", &joined, "-d", "cwd", "-Fn"], deadline)?;
    Some(parse_lsof_cwds(&out))
}

/// Pids whose executable basename is an agent binary, via one `ps` pass.
fn agent_pids(deadline: Instant) -> Option<Vec<u32>> {
    let out = run_with_deadline("ps", &["-axo", "pid=,comm="], deadline)?;
    let mut pids = Vec::new();
    for line in out.lines() {
        let mut parts = line.trim().splitn(2, ' ');
        let (Some(pid), Some(comm)) = (parts.next(), parts.next()) else {
            continue;
        };
        let basename = comm.trim().rsplit('/').next().unwrap_or_default();
        if AGENT_BINARIES.contains(&basename) {
            if let Ok(pid) = pid.parse() {
                pids.push(pid);
            }
        }
    }
    Some(pids)
}

/// The cwd paths from `lsof -Fn` output, minus subagent worktrees.
fn parse_lsof_cwds(out: &str) -> HashSet<String> {
    out.lines()
        .filter_map(|line| line.strip_prefix('n'))
        .filter(|path| !path.contains(SUBAGENT_WORKTREE_MARKER))
        .map(|path| path.trim_end_matches('/').to_string())
        // A cwd of `/` trims to empty — never a session directory, drop it.
        .filter(|path| !path.is_empty())
        .collect()
}

/// Run a command, killing it and returning `None` if it outlives `deadline`.
/// stdout is drained on a helper thread so a chatty child can never deadlock on
/// a full pipe while we wait.
fn run_with_deadline(program: &str, args: &[&str], deadline: Instant) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| warn!(program, error = %e, "liveness probe spawn failed"))
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let buf = reader.join().ok()?;
                if !status.success() {
                    // `lsof` exits non-zero when some pids vanished mid-call while
                    // still printing the rest — partial output is fine. A totally
                    // empty failure is a real probe failure.
                    if buf.is_empty() {
                        return None;
                    }
                }
                return String::from_utf8(buf).ok();
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    warn!(program, "liveness probe timed out; skipping this tick");
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                warn!(program, error = %e, "liveness probe wait failed");
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_lsof_cwds;

    #[test]
    fn parses_batched_lsof_records() {
        let out = "p123\nfcwd\nn/Users/x/repos/foo\np456\nfcwd\nn/Users/x/conductor/workspaces/riku/surat\n";
        let cwds = parse_lsof_cwds(out);
        assert!(cwds.contains("/Users/x/repos/foo"));
        assert!(cwds.contains("/Users/x/conductor/workspaces/riku/surat"));
        assert_eq!(cwds.len(), 2);
    }

    #[test]
    fn skips_subagent_worktrees() {
        let out = "p1\nfcwd\nn/Users/x/repos/foo/.claude/worktrees/agent-abc\np2\nfcwd\nn/Users/x/repos/foo\n";
        let cwds = parse_lsof_cwds(out);
        assert_eq!(cwds.len(), 1);
        assert!(cwds.contains("/Users/x/repos/foo"));
    }

    /// Live end-to-end probe against the real ps/lsof. Ignored by default (it
    /// depends on the host machine); run explicitly with
    /// `cargo test -p sessions probe_runs -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn probe_runs_on_this_machine() {
        let cwds = super::probe_alive_cwds().expect("probe failed");
        eprintln!("live agent cwds: {cwds:?}");
    }
}
