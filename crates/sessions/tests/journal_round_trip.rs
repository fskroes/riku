//! The journal API as the board sees it: through `sessions::`, from outside the
//! crate, with the directory and the clock both held still.
//!
//! `board/src/recap.rs` calls `read_journal` and resolves what comes back;
//! nothing inside `sessions` proves that surface holds across the crate
//! boundary. This does, against the `_in` variants that take an explicit
//! directory and an explicit clock precisely so a test can hold both still.

use chrono::{Duration, TimeZone, Utc};
use sessions::{append_note_in, read_journal_in, Handoff, Voice};

#[test]
fn a_note_written_to_a_journal_reads_back_through_the_public_surface() {
    let dir = tempfile::tempdir().unwrap();
    let at = Utc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap();

    // A note is a correction asking for something, so NeedsYou is the honest
    // Handoff Status — it is also the default both note surfaces use.
    let noted = append_note_in(
        dir.path(),
        "riku",
        "the base was wrong",
        Handoff::NeedsYou,
        at,
    )
    .unwrap();
    assert!(
        noted.path.starts_with(dir.path()),
        "the note landed outside the directory it was given: {}",
        noted.path.display()
    );
    assert_eq!(
        noted.session, "",
        "an empty journal has no thread to answer yet"
    );

    let journal = read_journal_in(dir.path(), "riku");
    assert!(!journal.is_empty());
    let entries = journal.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].next, "the base was wrong");
    assert_eq!(entries[0].who, Voice::User);
    assert_eq!(entries[0].handoff, Handoff::NeedsYou);
    assert_eq!(entries[0].at, at);

    let reading = journal.resolve(Some(at)).unwrap();
    assert_eq!(reading.latest.next, "the base was wrong");
    assert_eq!(
        reading.age(at + Duration::minutes(90)),
        Duration::minutes(90),
        "age against a later clock is exactly the difference passed in"
    );
}

#[test]
fn a_second_note_answers_the_same_session_as_the_first() {
    let dir = tempfile::tempdir().unwrap();
    // The record the stop hook writes: an agent session that had the last word.
    // Written as raw bytes because that on-disk line, not a Rust value, is what
    // the hook actually produces.
    std::fs::write(
        dir.path().join("riku.jsonl"),
        concat!(
            r#"{"v":1,"project":"riku","session":"s1","at":"2026-08-04T09:00:00Z","#,
            r#""who":"agent","handoff":"on-track","done":["Wrote the reader"],"#,
            r#""next":"Review it","resume":{"instruction":"pick it up"}}"#,
            "\n"
        ),
    )
    .unwrap();

    let at = Utc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap();
    let first = append_note_in(
        dir.path(),
        "riku",
        "not done - the base was wrong",
        Handoff::NeedsYou,
        at,
    )
    .unwrap();
    let second = append_note_in(
        dir.path(),
        "riku",
        "and say which base you used",
        Handoff::NeedsYou,
        at + Duration::minutes(5),
    )
    .unwrap();

    assert_eq!(first.session, "s1", "a note answers the thread that spoke last");
    assert_eq!(
        second.session, "s1",
        "the second note answers the same session as the first"
    );

    let journal = read_journal_in(dir.path(), "riku");
    let entries = journal.entries();
    assert_eq!(entries.len(), 3, "the agent's entry and both notes survive");
    assert_eq!(entries[2].next, "and say which base you used");
    assert_eq!(entries[2].session, "s1");
}
