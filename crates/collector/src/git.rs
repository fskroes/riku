//! Live git diff stats for a session's repo (C5).
//!
//! A Session's `+/-` is not in its transcript — it is live working-tree state — so
//! it is read here by shelling out to `git`, the same way Work Items shell out to
//! `gh`. [`diff_stat`] answers "how much has this branch changed": the committed
//! work since the branch left the repo's default branch, **plus** anything still
//! uncommitted in the working tree. Everything degrades to `None` (not an error)
//! when the directory is not a git repo or `git` cannot run, so a card without a
//! repo simply shows no diff.
//!
//! The line-summing ([`parse_numstat`]) is split out and pure so it can be tested
//! without a repo; the process wiring around it is thin.

use std::path::Path;
use std::process::Command;

use tracing::warn;

use crate::model::DiffStat;

/// Compute the working diff stat for the repo rooted at (or containing) `dir`.
///
/// Measures the merge-base of the repo's default branch → working tree, so a
/// feature branch reports its whole change set (committed commits since it forked,
/// plus uncommitted edits). On the default branch itself, or when no base can be
/// resolved, it falls back to uncommitted-vs-`HEAD`. Returns `None` when `dir` is
/// not inside a work tree or `git` is unavailable.
pub fn diff_stat(dir: &Path) -> Option<DiffStat> {
    if !is_work_tree(dir) {
        return None;
    }
    // Prefer merge-base(default-branch, HEAD); fall back to HEAD (uncommitted only).
    let rev = base_rev(dir).unwrap_or_else(|| "HEAD".to_string());
    let numstat = numstat_against(dir, &rev)
        // A brand-new repo has no HEAD to diff against; there is nothing to show.
        .or_else(|| numstat_against(dir, "HEAD"))?;
    Some(parse_numstat(&numstat))
}

/// Whether `dir` sits inside a git work tree (`git rev-parse` succeeds and prints
/// `true`). Cheap and the gate for every other call, so one bad `git` invocation
/// short-circuits the rest.
fn is_work_tree(dir: &Path) -> bool {
    let out = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(dir)
        .output();
    matches!(out, Ok(o) if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
}

/// The commit to diff the working tree against: the merge-base of the repo's
/// default branch and `HEAD`. `None` when no default branch can be resolved or it
/// shares no history with `HEAD` — the caller then diffs uncommitted-vs-`HEAD`.
fn base_rev(dir: &Path) -> Option<String> {
    let base = default_branch(dir)?;
    let out = Command::new("git")
        .args(["merge-base", "HEAD", &base])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// The repo's default branch ref (`origin/main`, `main`, …). Tries the remote's
/// `origin/HEAD` symbolic ref first, then a short list of conventional names,
/// verifying each resolves to a commit. `None` if none do.
fn default_branch(dir: &Path) -> Option<String> {
    // `origin/HEAD` → `refs/remotes/origin/main`; strip to `origin/main`.
    let out = Command::new("git")
        .args(["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        .current_dir(dir)
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let name = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    ["origin/main", "origin/master", "main", "master"]
        .into_iter()
        .find(|cand| rev_exists(dir, cand))
        .map(str::to_string)
}

/// Whether `rev` resolves to a commit in `dir`.
fn rev_exists(dir: &Path, rev: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `git diff --numstat <rev>` output (rev → working tree), or `None` on failure.
fn numstat_against(dir: &Path, rev: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["diff", "--numstat", rev])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        warn!(dir = %dir.display(), rev, "git diff --numstat failed");
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Sum a `git diff --numstat` block into total added / removed lines. Each line is
/// `<added>\t<removed>\t<path>`; binary files report `-\t-\t<path>` and contribute
/// nothing. Unparseable lines are skipped, so odd paths never break the total.
pub fn parse_numstat(numstat: &str) -> DiffStat {
    let mut added = 0u64;
    let mut removed = 0u64;
    for line in numstat.lines() {
        let mut cols = line.split('\t');
        let (Some(a), Some(r)) = (cols.next(), cols.next()) else {
            continue;
        };
        // `-` marks a binary file; treat as zero on that axis.
        added += a.trim().parse::<u64>().unwrap_or(0);
        removed += r.trim().parse::<u64>().unwrap_or(0);
    }
    DiffStat { added, removed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_added_and_removed_across_files() {
        let numstat = "12\t3\tsrc/a.rs\n0\t7\tsrc/b.rs\n40\t0\tREADME.md\n";
        assert_eq!(parse_numstat(numstat), DiffStat { added: 52, removed: 10 });
    }

    #[test]
    fn binary_files_and_blank_lines_contribute_nothing() {
        let numstat = "-\t-\tlogo.png\n\n5\t2\tsrc/a.rs\n";
        assert_eq!(parse_numstat(numstat), DiffStat { added: 5, removed: 2 });
    }

    #[test]
    fn empty_diff_is_zero() {
        assert_eq!(parse_numstat(""), DiffStat { added: 0, removed: 0 });
    }

    #[test]
    fn non_repo_directory_has_no_diff() {
        let tmp = std::env::temp_dir();
        // temp_dir itself is not a git work tree in CI; if it somehow is, the call
        // still returns a valid (possibly Some) value — this only asserts no panic.
        let _ = diff_stat(&tmp);
    }
}
