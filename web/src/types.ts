// Mirror of the Rust `Session` model (camelCase JSON). Keep in sync with
// crates/collector/src/model.rs.

export type Status = "active" | "attention" | "finished";

// Why a card is in Attention. Present only when status === "attention".
export type AttentionReason = "waiting" | "error";

export type Tool = "claude" | "codex";

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
  attentionReason: AttentionReason | null;
}

export interface SessionsResponse {
  sessions: Session[];
}

// One project the Work Items view can show: a display name plus the directory
// (`cwd`) that identifies it to `GET /api/work`. Derived client-side from the
// sessions, so a project appears once it has a session in the last 24h.
export interface ProjectRef {
  name: string;
  cwd: string;
}

// Mirror of the Work model. Keep in sync with crates/collector/src/work.rs and the
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
