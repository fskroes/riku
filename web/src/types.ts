// Mirror of the Rust `Session` model (camelCase JSON). Keep in sync with
// crates/sessions/src/model.rs.

export type Status = "active" | "attention" | "finished";

// The typed kind of human response a session needs (ADR 0010). A closed set; the
// text carries the meaning, so all causes share one visual priority on the board.
export type AttentionCause = "approval" | "answer" | "review" | "error" | "input";

// One current, structured Attention on a session. Present only when
// status === "attention".
export interface Attention {
  cause: AttentionCause;
  // When the current need began (Attention Since) — drives waiting duration and
  // the oldest-waiting-first order.
  since: string; // ISO 8601
  // A bounded, source-faithful evidence excerpt, or null when none is safe. For a
  // relayed card this is the privacy-safe remote rendering baked in at the source.
  evidence: string | null;
  // A relayed card whose allowlisted fields could not explain the need: the UI
  // points at the source machine rather than showing a guess.
  detailsOnSource: boolean;
}

export type Tool = "claude" | "codex";

// Lines added / removed in a session's repo — the card's `+/-` stat (C5). `null`
// when the session's cwd is not a git repo.
export interface DiffStat {
  added: number;
  removed: number;
}

// The Sub-agents a session is currently fanning work out to — the card's badge
// (issue #23). A Sub-agent is never its own card. `active` is the badge count;
// `descriptions` are the active ones' short descriptions, for the tooltip. Empty
// for a session not fanning out and for Codex (no Sub-agent concept).
export interface SubAgents {
  active: number;
  descriptions: string[];
}

export interface Session {
  id: string;
  tool: Tool;
  project: string;
  model: string | null;
  branch: string | null;
  cwd: string | null;
  tokensIn: number;
  tokensOut: number;
  activity: string | null;
  lastEventAt: string; // ISO 8601
  status: Status;
  // The current structured need, when status === "attention"; null otherwise.
  attention: Attention | null;
  // Estimated USD cost (tokens × the model's public list price); `null` for an
  // unpriced model. A labelled estimate — hidden when the cost toggle is off.
  costUsd: number | null;
  // Live git `+/-` for the session's repo, or `null` when there is none.
  diff: DiffStat | null;
  // The Sub-agents this session is currently fanning out to (issue #23). Empty when
  // it is not fanning out or the source has no Sub-agent concept (Codex) — the card
  // then omits the badge. Always present (defaults to an empty set on the wire).
  subAgents: SubAgents;
  // The machine this session runs on — the host's name (C7). Stamped by the board
  // (or a Collector) so every card shows which machine it is on; `null` only for a
  // pre-C7 session that was never stamped.
  machine: string | null;
}

export interface SessionsResponse {
  sessions: Session[];
}

// The board's Relay-subscription state (C7), for the topbar pill. `configured` is
// whether a Relay was set up at all (else zero-setup solo mode); `connected` is
// whether the board's subscription is currently live (else reconnecting).
export interface RelayStatus {
  configured: boolean;
  connected: boolean;
}

// One project the Work Items view can show: a display name plus the directory
// (`cwd`) that identifies it to `GET /api/work`. Derived client-side from the
// sessions, so a project appears once it has a session in the last 24h.
export interface ProjectRef {
  name: string;
  cwd: string;
}

// Mirror of the Work model. Keep in sync with crates/sessions/src/work.rs and the
// `/api/work` response shaped in crates/board/src/http.rs.

export type WorkStatus = "todo" | "doing" | "done";

// Which source a project's Work Items came from (WORK.md wins over GitHub).
export type WorkSource = "workMd" | "github";

// The Agent Session carrying a Work Item — the Work Link, made visible. `id`
// cross-links to the same session's card on the Board.
export interface LinkedSession {
  id: string;
  project: string;
  tool: Tool;
  model: string | null;
  branch: string | null;
  status: Status;
  // The machine the linked session is on (C7), shown on the Work Link chip.
  machine: string | null;
}

export interface WorkItem {
  id: string;
  title: string;
  status: WorkStatus;
  effort: string | null;
  blockedBy: string[];
  session: LinkedSession | null;
}

export interface WorkResponse {
  project: string;
  source: WorkSource;
  items: WorkItem[];
}

// Mirror of the journal-derived recap (ADR 0013). Keep in sync with
// crates/board/src/recap.rs and crates/sessions/src/journal.rs.

// The agent's parting assessment of where an effort stands, written at session
// stop. Not Attention: Attention is a live status of a running session. Card
// order is this order — needs-you → needs-review → on-track.
export type Handoff = "needs-you" | "needs-review" | "on-track";

// Who wrote a journal entry. Both voices are equal; the latest entry wins.
export type Voice = "agent" | "user";

// One local day's finished work, as the authors reported it.
export interface JournalDay {
  date: string; // local YYYY-MM-DD
  done: string[];
}

// How a card offers to pick the work back up. `command` is built by Riku from
// the session the store resolved — display-only text for the user to copy, never
// something the board runs (ADR 0002 / 0013).
export interface CardResume {
  instruction: string;
  command: string | null;
  dir: string | null;
  // The entry names a session this machine cannot get back into, so the
  // instruction stands alone. Distinct from an entry that named no session.
  sessionGone: boolean;
}

// What the journal says about one project: the entry that had the last word,
// plus the days it finished work on. All prose here is untrusted input — it is
// rendered as text and never as markup or a command.
export interface CardJournal {
  handoff: Handoff;
  next: string;
  days: JournalDay[];
  session: string;
  who: Voice;
  at: string; // ISO 8601
  // How old the latest entry is — the card's "latest 2h ago" label.
  ageSeconds: number;
  resume: CardResume;
}

// One thread of effort. `journal === null` is the derived-timeline fallback: the
// project has sessions but no prose (no wired stop hook, or nothing written yet).
export interface RecapCard {
  project: string;
  cwd: string;
  journal: CardJournal | null;
}

export interface OlderResume {
  instruction: string;
  sessionGone: boolean;
}

// A project whose journal outlived the sessions that wrote it: a line, not a
// card, because the slug it is filed under cannot be turned back into a path —
// there is nowhere to deep-link and nothing to resume into.
export interface OlderJournal {
  slug: string;
  handoff: Handoff;
  next: string;
  who: Voice;
  at: string; // ISO 8601
  ageSeconds: number;
  resume: OlderResume;
}

// The user's answer to a card, as `POST /api/recap/note` takes it. Riku appends
// it as a `who:"user"` entry — acting as the user's pen on an explicit user
// action — and the latest entry wins whoever wrote it (ADR 0013). `cwd` is the
// card's own directory: the endpoint only writes for a project it is showing.
export interface Correction {
  cwd: string;
  text: string;
  handoff: Handoff;
}

export interface RecapResponse {
  // The `journal.enabled` toggle. False means untouched, not merely unrendered —
  // "you have not turned this on" and "nothing written yet" are different states.
  enabled: boolean;
  // Ordered by the server: Handoff Status first, then newest.
  cards: RecapCard[];
  older: OlderJournal[];
  // The true count behind a capped `older`, so the view can say "5 of 12".
  olderTotal: number;
}
