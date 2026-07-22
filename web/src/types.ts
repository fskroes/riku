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
