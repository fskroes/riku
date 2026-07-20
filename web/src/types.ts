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
