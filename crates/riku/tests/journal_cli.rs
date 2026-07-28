//! `riku journal` end to end: the real binary, an isolated `$XDG_DATA_HOME`,
//! and the file it leaves behind (ADR 0013).
//!
//! The unit suites cover the grammar (`cli.rs`) and the write itself
//! (`sessions::journal_store`); what only shows up here is the wiring between
//! them — the toggle actually gating the command, a typed `<project>` becoming
//! the file the stop hook writes, and a note surviving as a record the reader
//! accepts.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

/// The riku binary, pointed at a throwaway home so a test never touches the
/// developer's own journal.
fn riku(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riku"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .args(args)
        .output()
        .expect("riku should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The journal directory the binary writes under `home`.
fn journal_dir(home: &Path) -> std::path::PathBuf {
    home.join("data").join("riku").join("journal")
}

/// The single journal file, failing loudly if there is not exactly one.
fn only_journal(home: &Path) -> std::path::PathBuf {
    let mut files: Vec<_> = std::fs::read_dir(journal_dir(home))
        .expect("journal directory")
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "expected one journal file, got {files:?}");
    files.pop().unwrap()
}

#[test]
fn a_note_is_written_read_back_and_purged() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let project = home.join("dev").join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let project = project.to_str().unwrap();

    // Off by default: the write surface is inert, the refusal names the switch,
    // and nothing is left on disk.
    let refused = riku(home, &["journal", "note", project, "answer me"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("riku config set journal.enabled true"),
        "unexpected error: {}",
        stderr(&refused)
    );
    assert!(!journal_dir(home).exists(), "an off journal wrote to disk");

    assert!(riku(home, &["config", "set", "journal.enabled", "true"])
        .status
        .success());

    // On: the note lands in the file named for the project directory.
    let noted = riku(home, &["journal", "note", project, "I also need Kelvin"]);
    assert!(noted.status.success(), "{}", stderr(&noted));
    let path = only_journal(home);
    assert!(
        stdout(&noted).contains(path.to_str().unwrap()),
        "the user is told where their words went: {}",
        stdout(&noted)
    );

    // It is a record the reader accepts, in the user's voice.
    let first = std::fs::read_to_string(&path).unwrap();
    let record: serde_json::Value = serde_json::from_str(first.trim()).expect("one JSON record");
    assert_eq!(record["v"], 1);
    assert_eq!(record["who"], "user");
    assert_eq!(record["handoff"], "needs-you");
    assert_eq!(record["next"], "I also need Kelvin");
    assert_eq!(record["done"].as_array().unwrap().len(), 0);
    assert_eq!(record["resume"]["instruction"], "");
    assert_eq!(
        record["project"],
        path.file_stem().unwrap().to_str().unwrap()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the journal is the user's alone");
    }

    // Nothing had spoken yet, so there was no thread to answer and the output
    // says so rather than naming an empty session.
    assert!(
        stdout(&noted).contains("first entry"),
        "unexpected output: {}",
        stdout(&noted)
    );

    // An agent session ends and writes its own entry, as the stop hook does.
    let agent = format!(
        "{{\"v\":1,\"project\":\"{}\",\"session\":\"a-real-session\",\"at\":\"2026-07-27T09:00:00Z\",\"who\":\"agent\",\"handoff\":\"needs-review\",\"done\":[\"Added Kelvin\"],\"next\":\"Review it\",\"resume\":{{\"instruction\":\"pick it up\"}}}}\n",
        path.file_stem().unwrap().to_str().unwrap()
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(agent.as_bytes())
        .unwrap();

    // A second note appends; the first is still there, word for word. The user
    // never named a thread, so the output says which one their words landed on
    // rather than leaving them to guess between the sessions they have running.
    let second = riku(home, &["journal", "note", project, "and Rankine"]);
    assert!(second.status.success(), "{}", stderr(&second));
    let all = std::fs::read_to_string(&path).unwrap();
    assert!(all.starts_with(&first), "an append rewrote history");
    assert_eq!(all.lines().count(), 3);
    assert!(
        stdout(&second).contains("a-real-session"),
        "the thread answered is not named: {}",
        stdout(&second)
    );

    // The user can lower a card as well as raise it: "carry on" must not pin the
    // card to the front of the board until an agent session happens to run.
    let carry_on = riku(
        home,
        &[
            "journal",
            "note",
            project,
            "that's fine, carry on",
            "--handoff",
            "on-track",
        ],
    );
    assert!(carry_on.status.success(), "{}", stderr(&carry_on));
    let last = std::fs::read_to_string(&path).unwrap();
    let last: serde_json::Value = serde_json::from_str(last.lines().last().unwrap()).unwrap();
    assert_eq!(last["handoff"], "on-track");
    assert_eq!(last["who"], "user");

    // A Handoff Status that does not exist is refused, and nothing is written.
    let bad = riku(
        home,
        &["journal", "note", project, "hm", "--handoff", "blocked"],
    );
    assert!(!bad.status.success());
    assert!(
        stderr(&bad).contains("needs-you"),
        "unexpected error: {}",
        stderr(&bad)
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().lines().count(),
        4,
        "a refused Handoff Status still wrote a note"
    );

    // A project that names nothing is refused rather than quietly filed away.
    let typo = riku(home, &["journal", "note", "proj-that-isnt", "hello"]);
    assert!(!typo.status.success());
    assert!(
        stderr(&typo).contains("pass the project's directory"),
        "unexpected error: {}",
        stderr(&typo)
    );
    only_journal(home);

    // Purge takes the prose off the disk in one command, and says so.
    let purged = riku(home, &["journal", "--purge"]);
    assert!(purged.status.success(), "{}", stderr(&purged));
    assert!(stdout(&purged).contains("removed 1 journal file"));
    assert!(!path.exists());

    // With the journal off again, purge still works — it is the user's control
    // over what exists, not a feature of the feature.
    assert!(riku(home, &["config", "set", "journal.enabled", "false"])
        .status
        .success());
    let empty = riku(home, &["journal", "--purge"]);
    assert!(empty.status.success());
    assert!(stdout(&empty).contains("no journal files to remove"));
}
