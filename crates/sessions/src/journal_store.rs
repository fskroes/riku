//! Where the Project Journal lives on disk (ADR 0013).
//!
//! Deliberately thin: it resolves the data directory and hands the bytes to
//! [`Journal::parse`], which owns every decision about them.
//!
//! The one thing Riku writes here is the **user's own voice** — [`append_note`]
//! backs `riku journal note` and the card's correction box, where Riku is
//! acting as the user's pen on an explicit user action. Riku never writes a
//! journal entry on its own behalf; the agent's entries come from its stop hook.
//! Every write is an append that creates the file `0600` and never rewrites a
//! line anyone else wrote, because append-only is what makes a correction a
//! reply rather than an edit.
//!
//! An absent journal is the normal case, not an error: the feature is opt-in,
//! and a project whose agent is not wired with the hook simply has no file.
//! [`read_journal`] answers that with an empty [`Journal`].

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use tracing::{debug, warn};

use crate::journal::{project_slug, Handoff, Journal, JournalEntry};

/// How large one project's journal may grow before it is rotated — roughly a
/// couple of thousand entries, or years of an ordinary project's handoffs.
///
/// Rotation keeps exactly one previous generation (`<project>.jsonl.1`), so a
/// journal costs at most twice this on disk however long it runs, and the parse
/// cost of a board refresh stays bounded at twice it (ADR 0013).
///
/// Both generations are read, so rotation is a write-side concern only: what a
/// reader sees is one conversation across the pair, and history leaves the board
/// only when the *rotated* file is itself replaced.
pub const JOURNAL_SIZE_CAP: u64 = 1 << 20;

/// What a project's journal is called, and what its one rotated generation is
/// called. Every path in this module is built from these, so the name the stop
/// hook writes, the name Riku reads, the name rotation produces, and the names
/// `--purge` recognizes cannot drift apart.
const JOURNAL_EXTENSION: &str = ".jsonl";
const ROTATED_SUFFIX: &str = ".jsonl.1";

/// The stop hook's local log of sessions that ended without writing an entry,
/// named in `hooks/claude-code/riku_journal_stop_hook.py` — the two must not
/// drift. Named here because `--purge` clears it along with the prose it
/// belongs to.
const MISSED_LOG: &str = "journal-missed.log";

/// The journal file for a project slug (see [`crate::journal::project_slug`]) —
/// `$XDG_DATA_HOME/riku/journal/<project>.jsonl`, else
/// `~/.local/share/riku/journal/<project>.jsonl`. The stop hook writes to
/// exactly this path. `None` only when neither directory is known.
pub fn journal_path(project: &str) -> Option<PathBuf> {
    Some(journal_file(&journal_dir()?, project))
}

/// A project's live journal inside a known directory.
fn journal_file(dir: &Path, project: &str) -> PathBuf {
    dir.join(format!("{project}{JOURNAL_EXTENSION}"))
}

/// The one generation kept beside it once it rotates.
fn rotated_file(dir: &Path, project: &str) -> PathBuf {
    dir.join(format!("{project}{ROTATED_SUFFIX}"))
}

/// Read a project's journal by slug. An unknown data directory, an absent file,
/// and an unreadable one all read as empty — a missing journal costs the prose,
/// never the board.
pub fn read_journal(project: &str) -> Journal {
    match journal_dir() {
        Some(dir) => read_journal_in(&dir, project),
        None => {
            debug!(project, "no data directory; journal unavailable");
            Journal::default()
        }
    }
}

/// Both generations of a project's journal in a known directory, oldest first.
///
/// The rotated file is read *before* the live one because that is the order they
/// were written in, and append order is the only recency the journal enforces.
/// Reading only the live file would be cheaper by half, and would quietly drop
/// every day before the last rotation off the recap while it still sat on disk.
///
/// Public because the directory is a caller's choice as often as it is this
/// module's: the board's recap holds its journal directory in state rather than
/// resolving it per read, which is what lets a fixture journal be served from a
/// temporary directory without the process-wide `$XDG_DATA_HOME` that an
/// in-process test cannot isolate. [`read_journal`] is the same read against the
/// one directory Riku owns.
pub fn read_journal_in(dir: &Path, project: &str) -> Journal {
    read_journal_file(project, &rotated_file(dir, project))
        .followed_by(read_journal_file(project, &journal_file(dir, project)))
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

/// Every project with a journal on disk, as its slug and the moment that
/// journal was last written. Unordered; the caller decides what order means.
///
/// This is the read that lets the recap see past the session store's discovery
/// window: prose outlives the transcript it was written beside, and a project
/// whose last session has aged out still has something to say. An unknown data
/// directory reads as nothing at all.
pub fn list_journals() -> Vec<(String, DateTime<Utc>)> {
    match journal_dir() {
        Some(dir) => list_journals_in(&dir),
        None => {
            debug!("no data directory; no journals to list");
            Vec::new()
        }
    }
}

/// [`list_journals`] against a known directory, for the same reason
/// [`read_journal_in`] is public: the board holds its journal directory in
/// state rather than resolving it per read.
///
/// Both generations count, because [`read_journal_in`] reads both — a project
/// whose live file was rotated away is still a project with prose. The pair is
/// one entry under one slug, stamped with the newer of the two.
///
/// The timestamp is the file's, not any entry's. `at` is agent-supplied and
/// nothing enforces it, so ordering a listing by it would let one entry stamped
/// in the year 3000 hold the top of the list forever; mtime is the filesystem's
/// word and cannot be written by the prose.
pub fn list_journals_in(dir: &Path) -> Vec<(String, DateTime<Utc>)> {
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        // A directory nobody has written to yet is the ordinary opt-out.
        Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            warn!(dir = %dir.display(), %error, "journal directory unreadable; ignoring it");
            return Vec::new();
        }
    };
    let mut newest: std::collections::HashMap<String, DateTime<Utc>> =
        std::collections::HashMap::new();
    for entry in listing.flatten() {
        // `file_type` does not follow symlinks, so a link planted here names no
        // project — the same rule `--purge` applies before deleting anything.
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(slug) = journal_slug(&name) else {
            continue;
        };
        let Some(written) = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(DateTime::<Utc>::from)
        else {
            continue;
        };
        newest
            .entry(slug)
            .and_modify(|at| *at = (*at).max(written))
            .or_insert(written);
    }
    newest.into_iter().collect()
}

/// The project a journal filename belongs to, or `None` when the file is not
/// one — the hook's miss log is telemetry rather than prose, and anything else
/// the user left in the directory is theirs.
fn journal_slug(name: &str) -> Option<String> {
    if name == MISSED_LOG {
        return None;
    }
    let slug = name
        .strip_suffix(ROTATED_SUFFIX)
        .or_else(|| name.strip_suffix(JOURNAL_EXTENSION))?;
    (!slug.is_empty()).then(|| slug.to_string())
}

/// Where a note landed: the file it was appended to, and the thread it answered.
///
/// The session is reported rather than kept private because the caller chose it
/// by implication — a note answers whatever spoke last, and with two threads
/// active that may not be the one the user had in mind. Naming it is what turns
/// a silent guess into something the user can see and correct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Noted {
    pub path: PathBuf,
    /// The Agent Session the note answers, empty when the journal had no entry
    /// to answer yet.
    pub session: String,
}

/// Append the user's note to a project's journal, answering whatever had the
/// last word, and return where it landed.
///
/// This is the user's pen: an explicit user action, never Riku narrating state
/// of its own.
pub fn append_note(project: &str, text: &str, handoff: Handoff) -> Result<Noted, String> {
    append_note_in(&require_journal_dir()?, project, text, handoff, Utc::now())
}

/// Delete every journal file, returning what was removed. Deliberately not
/// gated on `journal.enabled`: turning the feature off is a decision about what
/// gets written from now on, while purging is the user's control over the prose
/// already on disk, and that must always be available (ADR 0013).
pub fn purge_journals() -> Result<Vec<PathBuf>, String> {
    purge_journals_in(&require_journal_dir()?)
}

/// The journal slug for a `<project>` argument as a person would type it: an
/// existing directory (`.`, `~/dev/riku`), or the slug of a project that
/// already has a journal.
///
/// Anything else is an error rather than a new file. A note is the user
/// correcting the board, and the one outcome worse than refusing a typo is
/// accepting it: `riku journal note rikku "…"` would report success while
/// filing the correction where no card will ever read it.
pub fn resolve_journal_project(argument: &str) -> Result<String, String> {
    resolve_journal_project_in(&require_journal_dir()?, argument)
}

fn resolve_journal_project_in(dir: &Path, argument: &str) -> Result<String, String> {
    let path = Path::new(argument);
    if path.is_dir() {
        // The hook slugs the agent's process cwd, which is already a real path;
        // resolving symlinks here is what makes `riku journal note .` land in
        // the same file the agent wrote.
        return Ok(project_slug(
            &path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        ));
    }
    let slug = project_slug(path);
    match !slug.is_empty() && journal_file(dir, &slug).exists() {
        true => Ok(slug),
        false => Err(format!(
            "'{argument}' is not a project directory, and no journal is filed under it; pass the project's directory (use '.' for this one)"
        )),
    }
}

/// [`append_note`] against a known directory: read the journal to find the
/// thread that spoke last, then append the user's answer to it.
///
/// The lookup reads what the board reads, both generations included: the thread
/// a note answers has to be the thread whose card the user was looking at, and a
/// live file left empty or truncated by a crash must not turn a reply into a
/// free-floating entry while the conversation it belongs to sits in the rotated
/// generation.
///
/// Public for the reason [`read_journal_in`] is: the board holds its journal
/// directory in state rather than resolving it per call, which is what lets the
/// card's correction box be exercised against a temporary directory instead of
/// the developer's own journal. The clock is the caller's for the same reason
/// the recap's is — a stamp the module reached for itself could not be held
/// still by a test.
pub fn append_note_in(
    dir: &Path,
    project: &str,
    text: &str,
    handoff: Handoff,
    at: DateTime<Utc>,
) -> Result<Noted, String> {
    let session = read_journal_in(dir, project)
        .entries()
        .last()
        .map(|entry| entry.session.clone())
        .unwrap_or_default();
    let path = append_entry_in(
        dir,
        project,
        &JournalEntry::user_note(project, &session, at, text, handoff),
    )?;
    Ok(Noted { path, session })
}

/// The one write path: serialize to a single line, rotate if this append would
/// pass the cap, then append with the file created `0600`.
///
/// The two ways a write could land somewhere nothing will ever read it are
/// refused up front, because the reader answers both by silently skipping the
/// line: a project that names no file, and an entry filed under a project other
/// than the one whose file it is going into.
fn append_entry_in(dir: &Path, project: &str, entry: &JournalEntry) -> Result<PathBuf, String> {
    if project.is_empty() {
        return Err("a journal entry needs a project; none could be resolved".to_string());
    }
    if entry.project != project {
        return Err(format!(
            "refusing to file a '{}' entry under '{project}'",
            entry.project
        ));
    }
    let path = journal_file(dir, project);
    // serde_json escapes newlines, so a pasted paragraph stays one record and
    // cannot forge a second line in the file.
    let mut line = serde_json::to_string(entry)
        .map_err(|error| format!("could not encode the journal entry: {error}"))?;
    line.push('\n');

    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    rotate_if_full(dir, project, line.len() as u64)?;

    let mut options = OpenOptions::new();
    // Create-or-append, never truncate: the agent's entries and the user's are
    // the same conversation, and neither voice overwrites the other.
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Honored on creation only — an existing file keeps the permissions its
        // owner gave it, whether that was the stop hook or the user.
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    file.write_all(line.as_bytes())
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync {}: {error}", path.display()))?;
    Ok(path)
}

/// Rotate the journal when `incoming` more bytes would carry it past
/// [`JOURNAL_SIZE_CAP`]. The live file becomes the rotated generation,
/// replacing any older one — a single rename, so a concurrent reader sees
/// either the old file or the new one and never a half-copied journal.
fn rotate_if_full(dir: &Path, project: &str, incoming: u64) -> Result<(), String> {
    let path = journal_file(dir, project);
    let size = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not measure {}: {error}", path.display())),
    };
    if size + incoming <= JOURNAL_SIZE_CAP {
        return Ok(());
    }
    let rotated = rotated_file(dir, project);
    std::fs::rename(&path, &rotated)
        .map_err(|error| format!("could not rotate {}: {error}", path.display()))?;
    debug!(from = %path.display(), to = %rotated.display(), "journal rotated");
    Ok(())
}

/// [`purge_journals`] against a known directory. A directory that was never
/// created is already purged, not an error.
fn purge_journals_in(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("could not read {}: {error}", dir.display())),
    };
    let mut removed = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|error| format!("could not read {}: {error}", dir.display()))?;
        // Only regular files Riku put here. `file_type` does not follow
        // symlinks, so a link planted in the directory is skipped rather than
        // used to delete something elsewhere.
        let is_file = entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false);
        if !is_file || !is_journal_file(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let path = entry.path();
        std::fs::remove_file(&path)
            .map_err(|error| format!("could not remove {}: {error}", path.display()))?;
        removed.push(path);
    }
    removed.sort();
    Ok(removed)
}

/// Whether a file in the journal directory is Riku's to delete: a journal, a
/// rotated generation of one, or the hook's miss log. Anything else the user
/// happened to leave here is theirs.
fn is_journal_file(name: &str) -> bool {
    name.ends_with(".jsonl") || name.contains(".jsonl.") || name == MISSED_LOG
}

/// The journal directory, as an error rather than an absence: a write that
/// cannot name its file has to say so, where a read can simply come back empty.
fn require_journal_dir() -> Result<PathBuf, String> {
    journal_dir().ok_or_else(|| "could not determine the user data directory".to_string())
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
    use crate::journal::Voice;

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
    fn listing_names_every_project_with_prose_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("users-x-repos-riku.jsonl"), "").unwrap();
        std::fs::write(dir.path().join("users-x-dotfiles.jsonl"), "").unwrap();
        // A project whose live file has rotated away is still a project with
        // prose, because `read_journal_in` reads both generations.
        std::fs::write(dir.path().join("users-x-old.jsonl.1"), "").unwrap();
        // Not prose: the hook's own telemetry, and whatever else the user left.
        std::fs::write(dir.path().join(MISSED_LOG), "").unwrap();
        std::fs::write(dir.path().join("notes.md"), "").unwrap();

        let mut slugs: Vec<String> = list_journals_in(dir.path())
            .into_iter()
            .map(|(slug, _)| slug)
            .collect();
        slugs.sort();
        assert_eq!(
            slugs,
            vec!["users-x-dotfiles", "users-x-old", "users-x-repos-riku"]
        );
    }

    #[test]
    fn both_generations_of_one_project_are_one_entry_stamped_with_the_newer() {
        let dir = tempfile::tempdir().unwrap();
        let rotated = dir.path().join("riku.jsonl.1");
        let live = dir.path().join("riku.jsonl");
        std::fs::write(&rotated, "").unwrap();
        std::fs::write(&live, "").unwrap();
        // Make the rotated generation unambiguously older than the live one,
        // rather than relying on the two writes above landing on different
        // filesystem ticks.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&rotated)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let listed = list_journals_in(dir.path());
        assert_eq!(listed.len(), 1, "one project, not two: {listed:?}");
        assert_eq!(listed[0].0, "riku");
        assert!(
            listed[0].1 > DateTime::<Utc>::from(old),
            "the pair is stamped with the newer file, not the rotated one"
        );
    }

    #[test]
    fn a_journal_directory_that_does_not_exist_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_journals_in(&dir.path().join("never-created")).is_empty());
    }

    #[test]
    fn an_empty_journal_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("riku.jsonl");
        std::fs::write(&path, "").unwrap();
        assert!(read_journal_file("riku", &path).is_empty());
    }

    /// The bytes of one agent entry, sized so a handful of them cross a small cap.
    fn agent_line(session: &str, done: &str) -> String {
        format!(
            "{{\"v\":1,\"project\":\"riku\",\"session\":\"{session}\",\"at\":\"2026-07-27T09:00:00Z\",\"who\":\"agent\",\"handoff\":\"on-track\",\"done\":[\"{done}\"],\"next\":\"x\",\"resume\":{{\"instruction\":\"y\"}}}}\n"
        )
    }

    /// The ordinary note: this project, now, asking for something.
    fn note(dir: &Path, text: &str) -> Noted {
        append_note_in(dir, "riku", text, Handoff::NeedsYou, Utc::now()).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_note_creates_a_private_journal_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let noted = note(dir.path(), "answer me");
        assert_eq!(
            std::fs::metadata(&noted.path).unwrap().permissions().mode() & 0o777,
            0o600,
            "the journal is prose about the user's work; it is theirs alone"
        );
    }

    #[test]
    fn a_note_appends_and_answers_the_last_word() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("riku.jsonl");
        std::fs::write(&path, agent_line("s1", "Finished temps.py")).unwrap();

        let noted = note(dir.path(), "Not done - I need Kelvin");

        // The agent's line survives verbatim: an append never rewrites history.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(&agent_line("s1", "Finished temps.py")));

        let journal = read_journal_file("riku", &path);
        assert_eq!(journal.entries().len(), 2);
        let reading = journal.resolve(None).unwrap();
        assert_eq!(reading.latest.who, Voice::User);
        assert_eq!(reading.latest.next, "Not done - I need Kelvin");
        assert_eq!(
            reading.latest.session, "s1",
            "the note answers the thread that spoke last"
        );
        assert_eq!(
            noted.session, "s1",
            "and the caller is told which thread that was"
        );
    }

    #[test]
    fn a_note_can_carry_the_users_own_assessment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("riku.jsonl"), agent_line("s1", "Work")).unwrap();

        let noted = append_note_in(
            dir.path(),
            "riku",
            "That's fine, carry on",
            Handoff::OnTrack,
            Utc::now(),
        )
        .unwrap();

        let reading = read_journal_file("riku", &noted.path)
            .resolve(None)
            .unwrap();
        assert_eq!(reading.latest.handoff, Handoff::OnTrack);
        assert_eq!(reading.latest.who, Voice::User);
    }

    #[test]
    fn a_note_on_an_empty_journal_answers_no_thread() {
        let dir = tempfile::tempdir().unwrap();
        let noted = note(dir.path(), "start here");
        assert_eq!(noted.session, "");
        let journal = read_journal_file("riku", &noted.path);
        let entry = &journal.entries()[0];
        assert_eq!(entry.session, "");
        assert_eq!(entry.next, "start here");
    }

    #[test]
    fn a_write_nothing_could_read_back_is_refused() {
        let dir = tempfile::tempdir().unwrap();

        // The reader verifies an entry's project against the file it was found
        // in, so a misfiled entry would be written and then skipped forever.
        let theirs = JournalEntry::user_note(
            "someone-elses-repo",
            "s1",
            Utc::now(),
            "hi",
            Handoff::NeedsYou,
        );
        let error = append_entry_in(dir.path(), "riku", &theirs).unwrap_err();
        assert!(error.contains("refusing to file"), "unexpected: {error}");

        // An unresolvable project would name the file `.jsonl` and belong to
        // nothing.
        let error =
            append_note_in(dir.path(), "", "hi", Handoff::NeedsYou, Utc::now()).unwrap_err();
        assert!(error.contains("needs a project"), "unexpected: {error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_journal_rotates_once_it_would_pass_the_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("riku.jsonl");
        let rotated = dir.path().join("riku.jsonl.1");

        // A file already at the cap: the next append starts a fresh one, and the
        // full generation is kept beside it rather than deleted.
        let one = agent_line("s1", "Old work");
        let filled = one.repeat(JOURNAL_SIZE_CAP as usize / one.len() + 1);
        std::fs::write(&path, &filled).unwrap();

        note(dir.path(), "after rotation");

        assert_eq!(std::fs::read_to_string(&rotated).unwrap(), filled);
        let entries = read_journal_file("riku", &path);
        assert_eq!(entries.entries().len(), 1, "the live file starts fresh");
        assert_eq!(entries.entries()[0].next, "after rotation");

        // Rotating again keeps exactly one generation, so the journal is bounded
        // rather than growing a new file per rotation.
        std::fs::write(&path, &filled).unwrap();
        note(dir.path(), "after the second rotation");
        assert_eq!(std::fs::read_to_string(&rotated).unwrap(), filled);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            2,
            "one live journal and one previous generation"
        );
    }

    #[test]
    fn an_append_below_the_cap_does_not_rotate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("riku.jsonl"), agent_line("s1", "Work")).unwrap();
        note(dir.path(), "note");
        assert!(!dir.path().join("riku.jsonl.1").exists());
    }

    #[test]
    fn a_read_spans_the_rotated_generation() {
        // Rotation bounds the write; it must not retire days from the recap
        // while they are still sitting on disk.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("riku.jsonl.1"),
            agent_line("s1", "Retired work"),
        )
        .unwrap();
        std::fs::write(dir.path().join("riku.jsonl"), agent_line("s2", "Live work")).unwrap();

        let journal = read_journal_in(dir.path(), "riku");
        assert_eq!(journal.entries().len(), 2);
        assert_eq!(journal.entries()[0].done, vec!["Retired work"]);
        assert_eq!(
            journal.entries()[1].session,
            "s2",
            "the live file still has the last word"
        );

        // A project with nothing rotated yet is the ordinary case, not an error.
        let fresh = tempfile::tempdir().unwrap();
        std::fs::write(fresh.path().join("riku.jsonl"), agent_line("s1", "Work")).unwrap();
        assert_eq!(read_journal_in(fresh.path(), "riku").entries().len(), 1);
        assert!(read_journal_in(fresh.path(), "unknown-project").is_empty());
    }

    #[test]
    fn a_note_answers_the_thread_the_board_shows_it() {
        // The note's session comes from the same read the board does. A crash
        // between rotating and writing the entry that caused it leaves the live
        // file empty, and the reply would otherwise float free of the
        // conversation it is answering.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("riku.jsonl.1"),
            agent_line("s1", "Retired work"),
        )
        .unwrap();
        std::fs::write(dir.path().join("riku.jsonl"), "").unwrap();

        assert_eq!(note(dir.path(), "answer me").session, "s1");
    }

    #[test]
    fn purge_removes_every_journal_file_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "riku.jsonl",
            "users-dev-other.jsonl",
            "riku.jsonl.1",
            "journal-missed.log",
        ] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        // Not Riku's to delete.
        std::fs::write(dir.path().join("notes.md"), "mine").unwrap();

        let removed = purge_journals_in(dir.path()).unwrap();
        assert_eq!(removed.len(), 4, "removed: {removed:?}");
        assert!(dir.path().join("notes.md").exists());
        assert!(!dir.path().join("riku.jsonl").exists());
        assert!(!dir.path().join("riku.jsonl.1").exists());
        assert!(!dir.path().join("journal-missed.log").exists());
    }

    #[test]
    fn purging_when_there_is_nothing_to_purge_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(purge_journals_in(&dir.path().join("never-created"))
            .unwrap()
            .is_empty());
        assert!(purge_journals_in(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_project_argument_may_be_a_directory_or_a_slug_that_exists() {
        let journal = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let project = workspace.path().join("dev").join("My App.v2");
        std::fs::create_dir_all(&project).unwrap();

        // A path the user can actually type is slugged the way the hook slugs cwd.
        assert_eq!(
            resolve_journal_project_in(journal.path(), project.to_str().unwrap()).unwrap(),
            crate::journal::project_slug(&project.canonicalize().unwrap())
        );

        // A slug that already has a journal is taken as-is, so a project can be
        // answered from anywhere.
        std::fs::write(journal.path().join("users-dev-riku.jsonl"), "").unwrap();
        assert_eq!(
            resolve_journal_project_in(journal.path(), "users-dev-riku").unwrap(),
            "users-dev-riku"
        );

        // A typo names neither, and must not quietly become a new journal.
        for typo in ["users-dev-rikku", "~/not/here", ""] {
            let error = resolve_journal_project_in(journal.path(), typo).unwrap_err();
            assert!(
                error.contains("pass the project's directory"),
                "unexpected error for {typo:?}: {error}"
            );
        }
        assert_eq!(std::fs::read_dir(journal.path()).unwrap().count(), 1);
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
