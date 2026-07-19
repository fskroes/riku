// Mirror of the Rust `Session` model (camelCase JSON). Keep in sync with
// crates/collector/src/model.rs.

export type Status = "active" | "attention" | "finished";

export interface Session {
  id: string;
  project: string;
  model: string | null;
  branch: string | null;
  cwd: string | null;
  tokensIn: number;
  tokensOut: number;
  activity: string | null;
  lastEventAt: string; // ISO 8601
  status: Status;
}

export interface SessionsResponse {
  sessions: Session[];
}
