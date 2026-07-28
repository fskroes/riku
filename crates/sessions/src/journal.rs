//! The Project Journal (ADR 0013): decoding and resolution.
//!
//! An agent appends one record per session at stop; the user answers with
//! correction records of their own. This module owns the pure half — JSONL text
//! in, a resolved reading out — so every consumer (the `riku journal` CLI, the
//! board's recap endpoint, the day board) reads the same journal the same way.
//! File access lives in [`crate::journal_store`].
//!
//! Everything here treats the journal as **untrusted input**: it is written by
//! processes outside Riku, hand-edited by the user, and truncated by crashes. A
//! line that does not decode is skipped, never fatal — one bad append must not
//! cost the recap. Fields that identify an entry (`v`, `project`, `session`,
//! `at`, `who`, `handoff`) are required, because an entry that cannot be dated
//! or attributed cannot take part in a conversation; the prose fields (`done`,
//! `next`, `resume.instruction`) default to empty, because a half-written
//! sentence is worth less than the entry around it.
//!
//! Two consequences of that stance are worth stating outright, because they
//! decide what the board shows:
//!
//! - **Append order decides recency, not `at`.** `at` is agent-supplied and
//!   nothing enforces it; append order is enforced by the filesystem. Keying
//!   latest-wins on `at` would let one entry stamped in the year 3000 outrank
//!   every user correction that follows it, which is exactly the correction path
//!   ADR 0013 depends on. `at` is display metadata: it dates the work and groups
//!   the days, and it never decides who had the last word.
//! - **A record's `project` is a claim, not a fact.** Entries are read against
//!   the slug of the file they were found in, and an entry claiming a different
//!   project is skipped rather than rendered on the wrong board.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tracing::debug;

/// The record format this module understands. Records tagged with any other `v`
/// are skipped — the tag is ADR 0013's evolution path, and a reader that guesses
/// at an unknown shape is worse than one that stays quiet.
pub const JOURNAL_VERSION: u64 = 1;

/// The Handoff Status: the agent's parting assessment, at session stop, of where
/// the effort stands. Deliberately *not* Attention — Attention is a live,
/// source-evidence-only status of a running Agent Session, while this is a
/// judgment recorded as the session ends (CONTEXT.md, ADR 0013).
///
/// Declaration order is card order: needs-you → needs-review → on-track, so
/// sorting by `Ord` puts the effort that wants a human first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Handoff {
    /// The user must decide or provide something before the work moves.
    NeedsYou,
    /// The work is done and waiting on the user's review.
    NeedsReview,
    /// Nothing is wanted from the user.
    OnTrack,
}

impl Handoff {
    /// Every Handoff Status, in card order — the one list the CLI's `--handoff`
    /// flag and its usage line are both built from.
    pub const ALL: [Handoff; 3] = [Handoff::NeedsYou, Handoff::NeedsReview, Handoff::OnTrack];

    /// Where a user's note leaves a card when they do not say — a correction is
    /// usually the user asking for something. Named once, because both surfaces
    /// that offer a note (`riku journal note` and the card's correction box)
    /// default to it and a board that disagreed with the CLI about where an
    /// unqualified answer lands would be two features, not one.
    pub const NOTE_DEFAULT: Handoff = Handoff::NeedsYou;

    /// How a Handoff Status is spelled, on disk and on the command line. The two are
    /// deliberately the same word: what the user types into `--handoff` is what
    /// lands in the record, so there is nothing to translate and nothing to
    /// drift.
    pub fn as_str(self) -> &'static str {
        match self {
            Handoff::NeedsYou => "needs-you",
            Handoff::NeedsReview => "needs-review",
            Handoff::OnTrack => "on-track",
        }
    }
}

impl std::str::FromStr for Handoff {
    type Err = String;

    fn from_str(text: &str) -> Result<Handoff, String> {
        Handoff::ALL
            .into_iter()
            .find(|handoff| handoff.as_str() == text)
            .ok_or_else(|| {
                let known: Vec<&str> = Handoff::ALL.iter().map(|h| h.as_str()).collect();
                format!(
                    "'{text}' is not a Handoff Status; expected {}",
                    known.join(", ")
                )
            })
    }
}

/// Who wrote an entry. Both voices are equal for resolution — the journal is a
/// conversation, and the last word wins whoever spoke it (ADR 0013).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Voice {
    /// Written by the coding agent from its stop hook.
    Agent,
    /// Written by the user, via the card or `riku journal note`.
    User,
}

/// How to pick the work back up. A sentence for a fresh session, never a command
/// string: `session` identifies the run, and Riku alone renders the runnable form
/// via `DeepLink` — so an entry pointing at a session that no longer exists
/// yields no command instead of a dead one (ADR 0013).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    #[serde(default)]
    pub instruction: String,
}

/// One decoded journal record. Serializes back to exactly the on-disk record
/// shape, so a reader and a writer cannot drift apart on the format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalEntry {
    /// The record format tag, always [`JOURNAL_VERSION`] — carried rather than
    /// implied so an entry round-trips back to a valid line.
    pub v: u64,
    /// The project slug the entry was filed under (see [`project_slug`]),
    /// verified against the file it was read from.
    pub project: String,
    /// The Agent Session that wrote, or is being corrected by, this entry.
    pub session: String,
    /// When the author says the work happened. Untrusted: it dates the entry and
    /// groups the days, but it never decides recency.
    pub at: DateTime<Utc>,
    pub who: Voice,
    pub handoff: Handoff,
    /// What was finished, in the author's own prose — one sentence per piece.
    pub done: Vec<String>,
    /// The single next best step.
    pub next: String,
    pub resume: Resume,
}

impl JournalEntry {
    /// The record the user's own voice writes: `riku journal note` and, later,
    /// the card's correction box. Both go through this one constructor so the
    /// two surfaces cannot disagree about the shape of a correction.
    ///
    /// A note finishes nothing, so `done` is empty and `resume` carries no
    /// instruction; the user's text is the `next` step, which is the field
    /// latest-wins resolution reads.
    ///
    /// `handoff` is the user's own assessment, defaulting to
    /// [`Handoff::NeedsYou`] at the surfaces that offer no picker — a correction
    /// is usually the user asking for something. It is a parameter rather than a
    /// constant because the last word on a card belongs to whoever spoke it: a
    /// user who says "that's fine, carry on" must be able to *lower* the
    /// Handoff Status,
    /// or the card stays pinned to the front of the board until an agent session
    /// happens to run again.
    ///
    /// `session` is the thread the note answers, so the correction lands on
    /// that card rather than floating free; it is empty when there is no entry
    /// to answer yet.
    pub fn user_note(
        project: &str,
        session: &str,
        at: DateTime<Utc>,
        text: &str,
        handoff: Handoff,
    ) -> JournalEntry {
        JournalEntry {
            v: JOURNAL_VERSION,
            project: project.to_string(),
            session: session.to_string(),
            at,
            who: Voice::User,
            handoff,
            done: Vec::new(),
            next: text.to_string(),
            resume: Resume::default(),
        }
    }
}

/// A project's journal: every decoded entry, in the order it was appended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Journal {
    entries: Vec<JournalEntry>,
}

/// One day's worth of finished work, as the day board's **Done so far** column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalDay {
    /// The local date the work was reported on. Local, not UTC: "what did I do
    /// this morning, what did I do yesterday" is a question about the reader's
    /// own day, and evening work must not land on tomorrow's board.
    pub date: NaiveDate,
    /// Every `done` line reported that day, in the order it was written.
    pub done: Vec<String>,
}

/// The resolved reading of a journal: the entry that had the last word, plus
/// `done` grouped by day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalReading {
    /// The last entry appended in range, whoever wrote it — the card's Handoff
    /// Status, next step, and resume instruction all read off this one record
    /// rather than a re-flattened copy of it. A `who` of [`Voice::User`] means
    /// the last word is a correction the agent has not answered yet.
    pub latest: JournalEntry,
    /// Finished work grouped by day, newest day first.
    pub days: Vec<JournalDay>,
}

impl JournalReading {
    /// How old the latest entry is at `now` — what the card's "latest 2h ago"
    /// label measures. Zero rather than negative for an entry stamped in the
    /// future: clock skew is not a reason to render a negative age.
    pub fn age(&self, now: DateTime<Utc>) -> Duration {
        (now - self.latest.at).max(Duration::zero())
    }
}

impl Journal {
    /// Decode one project's journal text — one JSON record per line, read against
    /// the slug of the file it came from. Blank lines, malformed JSON, unknown
    /// `v`, wrong types, missing identifying fields, and entries claiming another
    /// project are all skipped. Entries keep their append order, which is the
    /// only recency signal the file actually enforces.
    pub fn parse(project: &str, text: &str) -> Journal {
        let entries = text
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .filter_map(|(n, line)| match serde_json::from_str::<RawEntry>(line) {
                Ok(raw) => raw.decode(project).or_else(|| {
                    debug!(line = n + 1, project, "journal entry not usable; skipped");
                    None
                }),
                Err(e) => {
                    debug!(line = n + 1, error = %e, "journal line is not a record; skipped");
                    None
                }
            })
            .collect();
        Journal { entries }
    }

    /// Every decoded entry, in append order.
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Read `later`'s entries as the continuation of this journal — how a
    /// rotated generation and the live file become one conversation.
    ///
    /// Order is the whole point: rotation renames the live file mid-history, and
    /// only the caller knows which side is older. Joining decoded entries rather
    /// than the two files' text also means a generation truncated by a crash
    /// costs its own last line and never swallows the next file's first one.
    pub fn followed_by(mut self, later: Journal) -> Journal {
        self.entries.extend(later.entries);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resolve the journal into what a card shows. `as_of` bounds the reading to
    /// entries not dated after that instant — the day board asks for a past day
    /// and gets that day's answer, not today's. `None` reads the whole journal.
    ///
    /// Returns `None` when nothing is in range: an absent journal and a journal
    /// whose entries are all newer than `as_of` are the same "no prose yet"
    /// case, and the card falls back to the derived timeline.
    pub fn resolve(&self, as_of: Option<DateTime<Utc>>) -> Option<JournalReading> {
        let in_range: Vec<&JournalEntry> = match as_of {
            Some(cutoff) => self.entries.iter().filter(|e| e.at <= cutoff).collect(),
            None => self.entries.iter().collect(),
        };
        let latest = *in_range.last()?;

        // Keyed by local date, so a day's work is one row however the entries
        // were appended. A day nobody reported work on is not a row at all.
        let mut by_day: BTreeMap<NaiveDate, Vec<String>> = BTreeMap::new();
        for entry in in_range {
            if entry.done.is_empty() {
                continue;
            }
            by_day
                .entry(entry.at.with_timezone(&Local).date_naive())
                .or_default()
                .extend(entry.done.iter().cloned());
        }

        Some(JournalReading {
            latest: latest.clone(),
            days: by_day
                .into_iter()
                .rev()
                .map(|(date, done)| JournalDay { date, done })
                .collect(),
        })
    }
}

/// The stable slug of a project's directory path: lowercased, with every run of
/// non-alphanumerics collapsed to `-` and no leading or trailing `-`.
///
/// This is the single definition of a project's journal filename. The stop hook
/// (`hooks/claude-code/riku_journal_stop_hook.py`) derives the same slug in
/// Python from the session's `cwd` — the hook writes `<slug>.jsonl` and Riku
/// reads it, so the two must not drift.
pub fn project_slug(project_dir: &Path) -> String {
    let raw = project_dir.to_string_lossy();
    let mut slug = String::with_capacity(raw.len());
    for ch in raw.trim_matches('/').chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

/// A raw journal line. Every field is optional so a partial record still decodes
/// far enough to be judged, rather than failing the whole line at serde.
#[derive(Debug, Deserialize)]
struct RawEntry {
    v: Option<u64>,
    project: Option<String>,
    session: Option<String>,
    at: Option<DateTime<Utc>>,
    who: Option<Voice>,
    handoff: Option<Handoff>,
    #[serde(default)]
    done: Vec<String>,
    #[serde(default)]
    next: String,
    resume: Option<Resume>,
}

impl RawEntry {
    /// `None` unless the record is this format, belongs to `project`, and carries
    /// everything needed to date and attribute it; prose may be missing.
    fn decode(self, project: &str) -> Option<JournalEntry> {
        if self.v? != JOURNAL_VERSION || self.project? != project {
            return None;
        }
        Some(JournalEntry {
            v: JOURNAL_VERSION,
            project: project.to_string(),
            session: self.session?,
            at: self.at?,
            who: self.who?,
            handoff: self.handoff?,
            // Blank bullets would render as empty rows on the card.
            done: self
                .done
                .into_iter()
                .filter(|d| !d.trim().is_empty())
                .collect(),
            next: self.next,
            resume: self.resume.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const PROJECT: &str = "riku";

    /// A UTC instant for a *local* wall-clock time, so day-grouping assertions
    /// mean the same thing in whatever timezone the suite runs in.
    fn local_at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Local
            .with_ymd_and_hms(y, m, d, h, 0, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn local_date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// One well-formed record, parameterized enough for the tests below. An
    /// empty `done` gives `[]`, the shape a user correction actually has.
    fn line(
        session: &str,
        at: DateTime<Utc>,
        who: &str,
        handoff: &str,
        done: &str,
        next: &str,
    ) -> String {
        let done = if done.is_empty() {
            String::from("[]")
        } else {
            format!(r#"["{done}"]"#)
        };
        let at = at.to_rfc3339();
        format!(
            r#"{{"v":1,"project":"riku","session":"{session}","at":"{at}","who":"{who}","handoff":"{handoff}","done":{done},"next":"{next}","resume":{{"instruction":"pick it up"}}}}"#
        )
    }

    #[test]
    fn parses_a_full_agent_record() {
        let at = local_at(2026, 7, 27, 9);
        let journal = Journal::parse(
            PROJECT,
            &line(
                "s1",
                at,
                "agent",
                "needs-review",
                "Wrote the parser",
                "Review it",
            ),
        );
        assert_eq!(journal.entries().len(), 1);
        let e = &journal.entries()[0];
        assert_eq!(e.v, JOURNAL_VERSION);
        assert_eq!(e.project, PROJECT);
        assert_eq!(e.session, "s1");
        assert_eq!(e.at, at);
        assert_eq!(e.who, Voice::Agent);
        assert_eq!(e.handoff, Handoff::NeedsReview);
        assert_eq!(e.done, vec!["Wrote the parser"]);
        assert_eq!(e.next, "Review it");
        assert_eq!(e.resume.instruction, "pick it up");
    }

    #[test]
    fn an_entry_serializes_back_to_the_record_it_came_from() {
        let text = line(
            "s1",
            local_at(2026, 7, 27, 9),
            "agent",
            "on-track",
            "Work",
            "Next",
        );
        let journal = Journal::parse(PROJECT, &text);
        let entry = &journal.entries()[0];

        // The writer's shape, not a reader-only projection: `resume` stays a
        // nested object and `v` survives, so a round-trip is lossless.
        let round_tripped = serde_json::to_string(entry).unwrap();
        assert!(
            round_tripped.contains(r#""resume":{"instruction":"pick it up"}"#),
            "expected the on-disk resume shape, got {round_tripped}"
        );
        assert_eq!(&Journal::parse(PROJECT, &round_tripped).entries()[0], entry);
    }

    #[test]
    fn the_last_appended_entry_wins_across_voices() {
        let text = [
            line(
                "s1",
                local_at(2026, 7, 27, 9),
                "agent",
                "needs-review",
                "Added Celsius",
                "Ship it",
            ),
            line(
                "s1",
                local_at(2026, 7, 27, 10),
                "user",
                "needs-you",
                "",
                "I also need Kelvin",
            ),
            line(
                "s2",
                local_at(2026, 7, 27, 11),
                "agent",
                "needs-review",
                "Added Kelvin per the journal note",
                "Review both",
            ),
        ]
        .join("\n");

        let reading = Journal::parse(PROJECT, &text).resolve(None).unwrap();
        assert_eq!(reading.latest.handoff, Handoff::NeedsReview);
        assert_eq!(reading.latest.next, "Review both");
        assert_eq!(reading.latest.who, Voice::Agent);
        assert_eq!(reading.latest.session, "s2");
    }

    #[test]
    fn a_user_correction_supersedes_the_agent() {
        let text = [
            line(
                "s1",
                local_at(2026, 7, 27, 9),
                "agent",
                "on-track",
                "Finished temps.py",
                "Nothing pending",
            ),
            line(
                "s1",
                local_at(2026, 7, 27, 10),
                "user",
                "needs-you",
                "",
                "temps.py is NOT done - I also need Kelvin",
            ),
        ]
        .join("\n");

        let reading = Journal::parse(PROJECT, &text).resolve(None).unwrap();
        assert_eq!(reading.latest.handoff, Handoff::NeedsYou);
        assert_eq!(reading.latest.who, Voice::User);
        assert_eq!(
            reading.latest.next,
            "temps.py is NOT done - I also need Kelvin"
        );
    }

    #[test]
    fn a_user_note_is_a_correction_in_the_shared_shape() {
        let at = local_at(2026, 7, 27, 10);
        let note =
            JournalEntry::user_note(PROJECT, "s1", at, "I also need Kelvin", Handoff::NeedsYou);
        assert_eq!(note.who, Voice::User);
        assert_eq!(note.handoff, Handoff::NeedsYou);
        assert_eq!(note.session, "s1", "a note answers a thread");
        assert!(note.done.is_empty(), "a note finishes nothing");
        assert_eq!(note.next, "I also need Kelvin");
        assert_eq!(note.resume.instruction, "");

        // What the writer emits is what the reader accepts: one line, decoding
        // back to the same entry.
        let line = serde_json::to_string(&note).unwrap();
        assert_eq!(&Journal::parse(PROJECT, &line).entries()[0], &note);
    }

    #[test]
    fn a_note_written_here_wins_over_the_agents_entry() {
        let agent = line(
            "s1",
            local_at(2026, 7, 27, 9),
            "agent",
            "on-track",
            "Finished temps.py",
            "Nothing pending",
        );
        let note = serde_json::to_string(&JournalEntry::user_note(
            PROJECT,
            "s1",
            local_at(2026, 7, 27, 10),
            "temps.py is NOT done - I also need Kelvin",
            Handoff::NeedsYou,
        ))
        .unwrap();

        let reading = Journal::parse(PROJECT, &[agent, note].join("\n"))
            .resolve(None)
            .unwrap();
        assert_eq!(reading.latest.who, Voice::User);
        assert_eq!(reading.latest.handoff, Handoff::NeedsYou);
        assert_eq!(
            reading.latest.next,
            "temps.py is NOT done - I also need Kelvin"
        );
        // The correction says nothing was finished, and must not erase the day.
        assert_eq!(reading.days[0].done, vec!["Finished temps.py"]);
    }

    #[test]
    fn a_note_spanning_lines_is_still_one_record() {
        // JSONL's invariant is one record per line; a shell heredoc or a pasted
        // paragraph must not become several half-records.
        let note = JournalEntry::user_note(
            PROJECT,
            "",
            local_at(2026, 7, 27, 10),
            "first thing\nsecond thing",
            Handoff::NeedsYou,
        );
        let line = serde_json::to_string(&note).unwrap();
        assert!(
            !line.contains('\n'),
            "note broke the JSONL invariant: {line}"
        );
        assert_eq!(
            Journal::parse(PROJECT, &line).entries()[0].next,
            "first thing\nsecond thing"
        );
    }

    #[test]
    fn append_order_decides_recency_not_the_agent_supplied_timestamp() {
        // An agent stamps its entry far in the future; the user then corrects it.
        // Sorting on `at` would let that entry outrank every later correction and
        // silently break the ADR's correction path, so append order wins.
        let text = [
            line(
                "s1",
                local_at(3000, 1, 1, 9),
                "agent",
                "on-track",
                "Claimed done",
                "Nothing pending",
            ),
            line(
                "s1",
                local_at(2026, 7, 27, 10),
                "user",
                "needs-you",
                "",
                "Not done - see the failing test",
            ),
        ]
        .join("\n");

        let reading = Journal::parse(PROJECT, &text).resolve(None).unwrap();
        assert_eq!(reading.latest.who, Voice::User);
        assert_eq!(reading.latest.handoff, Handoff::NeedsYou);
    }

    #[test]
    fn an_entry_claiming_another_project_is_skipped() {
        let text = [
            line(
                "s1",
                local_at(2026, 7, 27, 9),
                "agent",
                "on-track",
                "Mine",
                "x",
            ),
            r#"{"v":1,"project":"someone-elses-repo","session":"s2","at":"2026-07-27T10:00:00Z","who":"agent","handoff":"needs-you","done":["Theirs"],"next":"y"}"#.to_string(),
        ]
        .join("\n");

        let journal = Journal::parse(PROJECT, &text);
        assert_eq!(journal.entries().len(), 1);
        assert_eq!(journal.entries()[0].done, vec!["Mine"]);
    }

    #[test]
    fn hostile_and_corrupt_lines_are_skipped_not_fatal() {
        let good = line(
            "s1",
            local_at(2026, 7, 27, 9),
            "agent",
            "on-track",
            "Real work",
            "next",
        );
        let text = [
            "",
            "   ",
            "not json at all",
            "{",
            "[1,2,3]",
            r#""just a string""#,
            r#"{"v":1,"project":"riku","session":"s0","at":"not a timestamp","who":"agent","handoff":"on-track","done":[],"next":""}"#,
            r#"{"v":1,"project":"riku","session":"s0","at":"2026-07-27T08:00:00Z","who":"robot","handoff":"on-track","done":[],"next":""}"#,
            r#"{"v":1,"project":"riku","session":"s0","at":"2026-07-27T08:00:00Z","who":"agent","handoff":"blocked","done":[],"next":""}"#,
            r#"{"v":1,"project":42,"session":"s0","at":"2026-07-27T08:00:00Z","who":"agent","handoff":"on-track","done":[],"next":""}"#,
            r#"{"v":1,"project":"riku","session":"s0","at":"2026-07-27T08:00:00Z","who":"agent","handoff":"on-track","done":"a string not a list","next":""}"#,
            &good,
        ]
        .join("\n");

        let journal = Journal::parse(PROJECT, &text);
        assert_eq!(journal.entries().len(), 1);
        assert_eq!(journal.entries()[0].session, "s1");
    }

    #[test]
    fn unknown_version_is_skipped() {
        let text = [
            r#"{"v":2,"project":"riku","session":"s9","at":"2026-07-27T09:00:00Z","who":"agent","handoff":"on-track","done":["future"],"next":"x","resume":{"instruction":"y"}}"#,
            r#"{"project":"riku","session":"s8","at":"2026-07-27T09:00:00Z","who":"agent","handoff":"on-track","done":["no version"],"next":"x"}"#,
        ]
        .join("\n");
        assert!(Journal::parse(PROJECT, &text).is_empty());
    }

    #[test]
    fn identifying_fields_are_required_but_prose_may_be_missing() {
        // Missing `handoff` — cannot be placed on the board, so it is skipped.
        let missing_handoff = r#"{"v":1,"project":"riku","session":"s1","at":"2026-07-27T09:00:00Z","who":"agent","done":["x"],"next":"y"}"#;
        assert!(Journal::parse(PROJECT, missing_handoff).is_empty());

        // Missing prose — the entry survives with empty prose.
        let bare = r#"{"v":1,"project":"riku","session":"s1","at":"2026-07-27T09:00:00Z","who":"agent","handoff":"needs-you"}"#;
        let journal = Journal::parse(PROJECT, bare);
        assert_eq!(journal.entries().len(), 1);
        assert!(journal.entries()[0].done.is_empty());
        assert_eq!(journal.entries()[0].next, "");
        assert_eq!(journal.entries()[0].resume.instruction, "");
    }

    #[test]
    fn blank_done_bullets_are_dropped() {
        let text = r#"{"v":1,"project":"riku","session":"s1","at":"2026-07-27T09:00:00Z","who":"agent","handoff":"on-track","done":["","  ","Real work"],"next":"y"}"#;
        assert_eq!(
            Journal::parse(PROJECT, text).entries()[0].done,
            vec!["Real work"]
        );
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let text = r#"{"v":1,"project":"riku","session":"s1","at":"2026-07-27T09:00:00Z","who":"agent","handoff":"on-track","done":["x"],"next":"y","resume":{"instruction":"z","command":"rm -rf /"},"mood":"chipper"}"#;
        let journal = Journal::parse(PROJECT, text);
        assert_eq!(journal.entries().len(), 1);
        // ADR 0013: the record carries the instruction only — Riku builds the
        // runnable form itself, so a smuggled `command` is dropped on the floor.
        assert_eq!(journal.entries()[0].resume.instruction, "z");
    }

    #[test]
    fn done_is_grouped_by_local_day_newest_first() {
        let text = [
            line(
                "s1",
                local_at(2026, 7, 25, 9),
                "agent",
                "on-track",
                "Sat a",
                "x",
            ),
            // Late evening: a UTC-keyed grouping would push this onto Sunday for
            // anyone east of Greenwich.
            line(
                "s2",
                local_at(2026, 7, 25, 23),
                "agent",
                "on-track",
                "Sat b",
                "x",
            ),
            line(
                "s3",
                local_at(2026, 7, 27, 9),
                "agent",
                "on-track",
                "Mon a",
                "x",
            ),
        ]
        .join("\n");

        let days = Journal::parse(PROJECT, &text).resolve(None).unwrap().days;
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].date, local_date(2026, 7, 27));
        assert_eq!(days[0].done, vec!["Mon a"]);
        assert_eq!(days[1].date, local_date(2026, 7, 25));
        assert_eq!(days[1].done, vec!["Sat a", "Sat b"]);
    }

    #[test]
    fn days_with_nothing_done_are_left_out() {
        let text = [
            line(
                "s1",
                local_at(2026, 7, 25, 9),
                "agent",
                "on-track",
                "Sat a",
                "x",
            ),
            line(
                "s1",
                local_at(2026, 7, 26, 9),
                "user",
                "needs-you",
                "",
                "answer me",
            ),
        ]
        .join("\n");

        let days = Journal::parse(PROJECT, &text).resolve(None).unwrap().days;
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].done, vec!["Sat a"]);
    }

    #[test]
    fn as_of_reads_the_journal_as_it_stood_that_day() {
        let text = [
            line(
                "s1",
                local_at(2026, 7, 25, 9),
                "agent",
                "needs-you",
                "Sat a",
                "ask",
            ),
            line(
                "s2",
                local_at(2026, 7, 27, 9),
                "agent",
                "on-track",
                "Mon a",
                "done",
            ),
        ]
        .join("\n");
        let journal = Journal::parse(PROJECT, &text);

        let saturday = journal.resolve(Some(local_at(2026, 7, 25, 23))).unwrap();
        assert_eq!(saturday.latest.handoff, Handoff::NeedsYou);
        assert_eq!(saturday.latest.next, "ask");
        assert_eq!(saturday.days.len(), 1);

        // Nothing had been written yet before the first entry.
        assert!(journal.resolve(Some(local_at(2026, 7, 24, 9))).is_none());
    }

    #[test]
    fn an_empty_journal_resolves_to_nothing() {
        let journal = Journal::parse(PROJECT, "");
        assert!(journal.is_empty());
        assert!(journal.resolve(None).is_none());
    }

    #[test]
    fn entry_age_is_measured_from_the_latest_entry() {
        let at = local_at(2026, 7, 27, 9);
        let text = line("s1", at, "agent", "on-track", "x", "y");
        let reading = Journal::parse(PROJECT, &text).resolve(None).unwrap();
        assert_eq!(
            reading.age(at + Duration::hours(2)).num_hours(),
            2,
            "the card's `latest 2h ago` label"
        );
        // A clock-skewed future entry reads as brand new, never negative.
        assert_eq!(reading.age(at - Duration::hours(1)), Duration::zero());
    }

    #[test]
    fn a_note_can_lower_the_handoff_status_as_well_as_raise_it() {
        // "That's fine, carry on" is a correction too, and the card must be able
        // to leave the front of the board without waiting for an agent session.
        let agent = line(
            "s1",
            local_at(2026, 7, 27, 9),
            "agent",
            "needs-you",
            "Wrote the parser",
            "Which format do you want?",
        );
        let note = serde_json::to_string(&JournalEntry::user_note(
            PROJECT,
            "s1",
            local_at(2026, 7, 27, 10),
            "Either is fine - keep going",
            Handoff::OnTrack,
        ))
        .unwrap();

        let reading = Journal::parse(PROJECT, &[agent, note].join("\n"))
            .resolve(None)
            .unwrap();
        assert_eq!(reading.latest.who, Voice::User);
        assert_eq!(reading.latest.handoff, Handoff::OnTrack);
    }

    #[test]
    fn a_handoff_status_is_spelled_the_same_on_disk_and_on_the_command_line() {
        for handoff in Handoff::ALL {
            // What `--handoff` accepts is exactly what serde writes into the
            // record, so the flag can never name a Handoff Status the reader
            // rejects.
            assert_eq!(
                serde_json::to_string(&handoff).unwrap(),
                format!("\"{}\"", handoff.as_str())
            );
            assert_eq!(handoff.as_str().parse::<Handoff>(), Ok(handoff));
        }

        // Anything else names the ones that exist rather than guessing.
        let error = "blocked".parse::<Handoff>().unwrap_err();
        assert!(error.contains("needs-you"), "unexpected error: {error}");
        assert!(error.contains("on-track"), "unexpected error: {error}");
        assert!(
            "Needs-You".parse::<Handoff>().is_err(),
            "case is the spelling"
        );
    }

    #[test]
    fn a_rotated_generation_reads_as_the_earlier_half_of_one_conversation() {
        // What the reader does with `<project>.jsonl.1` and the live file: the
        // older generation's entries come first, so the live file still holds
        // the last word and the retired days are still on the board.
        let earlier = Journal::parse(
            PROJECT,
            &[
                line(
                    "s1",
                    local_at(2026, 7, 25, 9),
                    "agent",
                    "on-track",
                    "Sat a",
                    "x",
                ),
                // A crash mid-append leaves a partial line; it must cost that
                // line only, not the first entry of the file that follows.
                r#"{"v":1,"project":"riku","session":"s2","at":"2026-"#.to_string(),
            ]
            .join("\n"),
        );
        let live = Journal::parse(
            PROJECT,
            &line(
                "s3",
                local_at(2026, 7, 27, 9),
                "agent",
                "needs-review",
                "Mon a",
                "Review it",
            ),
        );

        let reading = earlier.followed_by(live).resolve(None).unwrap();
        assert_eq!(
            reading.latest.session, "s3",
            "the live file has the last word"
        );
        assert_eq!(reading.latest.next, "Review it");
        assert_eq!(
            reading.days.len(),
            2,
            "the rotated day is still on the board: {:?}",
            reading.days
        );
        assert_eq!(reading.days[1].done, vec!["Sat a"]);
    }

    #[test]
    fn handoff_sorts_needs_you_first() {
        let mut all = vec![Handoff::OnTrack, Handoff::NeedsReview, Handoff::NeedsYou];
        all.sort();
        assert_eq!(
            all,
            vec![Handoff::NeedsYou, Handoff::NeedsReview, Handoff::OnTrack]
        );
    }

    #[test]
    fn slug_matches_the_stop_hooks_definition() {
        // The hook's Python: re.sub(r"[^a-zA-Z0-9]+", "-", cwd.strip("/")).strip("-").lower()
        assert_eq!(
            project_slug(Path::new("/Users/fskroes/dev/riku")),
            "users-fskroes-dev-riku"
        );
        assert_eq!(
            project_slug(Path::new("/Users/fskroes/dev/my_app.v2/")),
            "users-fskroes-dev-my-app-v2"
        );
        assert_eq!(project_slug(Path::new("/tmp/Spike Proj")), "tmp-spike-proj");
        assert_eq!(project_slug(Path::new("/")), "");
        assert_eq!(project_slug(Path::new("")), "");
    }
}
