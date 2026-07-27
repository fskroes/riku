//! Where the Project Journal lives on disk (ADR 0013).
//!
//! Deliberately thin: it resolves the data directory and hands the bytes to
//! [`Journal::parse`], which owns every decision about them. Nothing here
//! writes — the agent's stop hook appends, and the user's own corrections go
//! through the `riku journal` CLI; Riku never writes the journal on its own
//! behalf.
//!
//! An absent journal is the normal case, not an error: the feature is opt-in,
//! and a project whose agent is not wired with the hook simply has no file.
//! [`read_journal`] answers that with an empty [`Journal`].

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::journal::Journal;

/// The journal file for a project slug (see [`crate::journal::project_slug`]) —
/// `$XDG_DATA_HOME/riku/journal/<project>.jsonl`, else
/// `~/.local/share/riku/journal/<project>.jsonl`. The stop hook writes to
/// exactly this path. `None` only when neither directory is known.
pub fn journal_path(project: &str) -> Option<PathBuf> {
    Some(journal_dir()?.join(format!("{project}.jsonl")))
}

/// Read a project's journal by slug. An unknown data directory, an absent file,
/// and an unreadable one all read as empty — a missing journal costs the prose,
/// never the board.
pub fn read_journal(project: &str) -> Journal {
    match journal_path(project) {
        Some(path) => read_journal_file(project, &path),
        None => {
            debug!(project, "no data directory; journal unavailable");
            Journal::default()
        }
    }
}

/// Read one journal file, decoding it against `project`. Split out from
/// [`read_journal`] so the file path can be exercised against a fixture without
/// an `$XDG_DATA_HOME`.
fn read_journal_file(project: &str, path: &Path) -> Journal {
    match std::fs::read_to_string(path) {
        Ok(text) => Journal::parse(project, &text),
        // No file is the ordinary opt-out; anything else is worth saying out
        // loud, because the prose silently vanishing from a board that had it
        // is confusing in a way an absent journal is not.
        Err(e) if e.kind() == ErrorKind::NotFound => {
            debug!(path = %path.display(), "no journal for this project yet");
            Journal::default()
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "journal unreadable; ignoring it");
            Journal::default()
        }
    }
}

/// The journal directory. Kept private: callers name a project, not a path.
fn journal_dir() -> Option<PathBuf> {
    dir_from(
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        dirs::home_dir(),
    )
}

/// The directory rule, taking its inputs as arguments so it is testable without
/// mutating the process environment.
fn dir_from(data_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = match data_home.filter(|p| !p.as_os_str().is_empty()) {
        Some(data_home) => data_home,
        None => home?.join(".local").join("share"),
    };
    Some(base.join("riku").join("journal"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_data_home_wins_over_the_home_fallback() {
        let dir = dir_from(
            Some(PathBuf::from("/data")),
            Some(PathBuf::from("/home/dev")),
        )
        .unwrap();
        assert_eq!(dir, PathBuf::from("/data/riku/journal"));
    }

    #[test]
    fn falls_back_to_local_share_under_home() {
        let dir = dir_from(None, Some(PathBuf::from("/home/dev"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/dev/.local/share/riku/journal"));

        // An empty XDG_DATA_HOME is unset, per the XDG spec.
        let dir = dir_from(Some(PathBuf::new()), Some(PathBuf::from("/home/dev"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/dev/.local/share/riku/journal"));
    }

    #[test]
    fn no_home_and_no_xdg_means_no_journal_dir() {
        assert!(dir_from(None, None).is_none());
    }

    #[test]
    fn an_absent_journal_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let journal = read_journal_file("riku", &dir.path().join("nothing-here.jsonl"));
        assert!(journal.is_empty());
        assert!(journal.resolve(None).is_none());
    }

    #[test]
    fn an_empty_journal_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("riku.jsonl");
        std::fs::write(&path, "").unwrap();
        assert!(read_journal_file("riku", &path).is_empty());
    }

    #[test]
    fn reads_a_journal_file_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("users-dev-riku.jsonl");
        std::fs::write(
            &path,
            "\
{\"v\":1,\"project\":\"users-dev-riku\",\"session\":\"s1\",\"at\":\"2026-07-27T09:00:00Z\",\"who\":\"agent\",\"handoff\":\"needs-review\",\"done\":[\"Wrote the reader\"],\"next\":\"Review it\",\"resume\":{\"instruction\":\"pick it up\"}}
truncated half a line - a crash mid-append
",
        )
        .unwrap();

        let reading = read_journal_file("users-dev-riku", &path)
            .resolve(None)
            .unwrap();
        assert_eq!(reading.latest.project, "users-dev-riku");
        assert_eq!(reading.latest.next, "Review it");
        assert_eq!(reading.days[0].done, vec!["Wrote the reader"]);
    }
}
