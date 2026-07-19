//! End-to-end tests: a temp-dir projects root -> discovery -> HTTP snapshot and
//! SSE `session` / `removed` events on file append / delete.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use board::{http, runtime, Started};
use futures::StreamExt;

/// Build one Claude Code assistant transcript line.
fn assistant_line(id: &str, activity: &str, tin: u64, tout: u64) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": id,
        "timestamp": "2026-07-19T10:00:00Z",
        "cwd": "/Users/x/repos/foo",
        "gitBranch": "main",
        "isSidechain": false,
        "message": {
            "model": "claude-opus-4-8",
            "usage": { "input_tokens": tin, "output_tokens": tout },
            "content": [{ "type": "text", "text": activity }]
        }
    })
    .to_string()
}

/// The lines of a minimal Codex rollout: session_meta + turn_context + token_count
/// + one assistant message.
fn codex_rollout(id: &str, activity: &str, tin: u64, tout: u64) -> Vec<String> {
    vec![
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": id,
                "cwd": "/Users/x/repos/bar",
                "thread_source": "user",
                "git": { "branch": "feat" }
            }
        })
        .to_string(),
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:01Z",
            "type": "turn_context",
            "payload": { "model": "gpt-5.6-sol", "approval_policy": "never" }
        })
        .to_string(),
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:02Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": { "total_token_usage": { "input_tokens": tin, "output_tokens": tout } }
            }
        })
        .to_string(),
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": activity }]
            }
        })
        .to_string(),
    ]
}

/// Write a Codex rollout under a `YYYY/MM/DD` date dir.
fn write_codex_rollout(codex_root: &Path, file: &str, lines: &[String]) -> PathBuf {
    write_transcript(codex_root, "2026/07/19", file, lines)
}

fn write_transcript(root: &Path, project_dir: &str, file: &str, lines: &[String]) -> PathBuf {
    let dir = root.join(project_dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file);
    let mut body = lines.join("\n");
    body.push('\n');
    fs::write(&path, body).unwrap();
    path
}

fn append_line(path: &Path, line: &str) {
    let mut f = OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(line.as_bytes()).unwrap();
    f.write_all(b"\n").unwrap();
}

/// Start the board HTTP server in-process on an ephemeral port. The returned
/// [`Started`] must be kept alive so the filesystem watcher keeps running.
async fn spawn_server(root: PathBuf) -> (SocketAddr, Started) {
    spawn_server_with(root, None).await
}

/// As [`spawn_server`], but also wires a Codex sessions root.
async fn spawn_server_with(
    claude_root: PathBuf,
    codex_root: Option<PathBuf>,
) -> (SocketAddr, Started) {
    let started = runtime::init(claude_root, codex_root, PathBuf::from("does-not-exist"));
    let app = http::router(started.state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, started)
}

/// Read from an SSE response until `needle` appears (or time out).
async fn read_until(resp: reqwest::Response, needle: &str, dur: Duration) -> String {
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let fut = async {
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.expect("stream chunk");
            buf.push_str(&String::from_utf8_lossy(&bytes));
            if buf.contains(needle) {
                return buf.clone();
            }
        }
        buf.clone()
    };
    tokio::time::timeout(dur, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for '{needle}' in SSE stream; got: {buf:?}"))
}

#[tokio::test]
async fn discovery_exposes_sessions_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    write_transcript(
        tmp.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "hello", 100, 10)],
    );

    let (addr, _started) = spawn_server(tmp.path().to_path_buf()).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], "sess-1");
    assert_eq!(sessions[0]["project"], "foo");
    assert_eq!(sessions[0]["tokensIn"], 100);
    assert_eq!(sessions[0]["tokensOut"], 10);
    assert_eq!(sessions[0]["status"], "active");
}

#[tokio::test]
async fn append_emits_session_event() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_transcript(
        tmp.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "hello", 100, 10)],
    );

    let (addr, _started) = spawn_server(tmp.path().to_path_buf()).await;

    // Awaiting send() guarantees the handler has subscribed before we mutate.
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/events"))
        .send()
        .await
        .unwrap();

    append_line(&path, &assistant_line("sess-1", "APPENDED_MARKER", 50, 5));

    let buf = read_until(resp, "APPENDED_MARKER", Duration::from_secs(15)).await;
    assert!(buf.contains("event: session"), "expected a session event: {buf:?}");
    // Tokens accumulated across the appended line.
    assert!(buf.contains("\"tokensIn\":150"), "expected summed tokens: {buf:?}");
}

#[tokio::test]
async fn delete_emits_removed_event() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_transcript(
        tmp.path(),
        "-Users-x-repos-foo",
        "bbbb.jsonl",
        &[assistant_line("sess-del", "hello", 100, 10)],
    );

    let (addr, _started) = spawn_server(tmp.path().to_path_buf()).await;

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/events"))
        .send()
        .await
        .unwrap();

    fs::remove_file(&path).unwrap();

    let buf = read_until(resp, "sess-del", Duration::from_secs(15)).await;
    assert!(buf.contains("event: removed"), "expected a removed event: {buf:?}");
}

#[tokio::test]
async fn claude_and_codex_render_side_by_side() {
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("claude-1", "hi from claude", 100, 10)],
    );
    write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-00-codex-1.jsonl",
        &codex_rollout("codex-1", "hi from codex", 1000, 200),
    );

    let (addr, _started) =
        spawn_server_with(claude.path().to_path_buf(), Some(codex.path().to_path_buf())).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2, "both tools should render: {sessions:?}");

    let claude_card = sessions.iter().find(|s| s["id"] == "claude-1").unwrap();
    assert_eq!(claude_card["tool"], "claude");
    assert_eq!(claude_card["project"], "foo");

    let codex_card = sessions.iter().find(|s| s["id"] == "codex-1").unwrap();
    assert_eq!(codex_card["tool"], "codex");
    assert_eq!(codex_card["project"], "bar");
    assert_eq!(codex_card["model"], "gpt-5.6-sol");
    assert_eq!(codex_card["branch"], "feat");
    assert_eq!(codex_card["tokensIn"], 1000);
    assert_eq!(codex_card["tokensOut"], 200);
}

#[tokio::test]
async fn codex_append_emits_session_event() {
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let path = write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-00-codex-2.jsonl",
        &codex_rollout("codex-2", "starting", 500, 50),
    );

    let (addr, _started) =
        spawn_server_with(claude.path().to_path_buf(), Some(codex.path().to_path_buf())).await;

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/events"))
        .send()
        .await
        .unwrap();

    // A later cumulative token_count updates the same card.
    append_line(
        &path,
        &serde_json::json!({
            "timestamp": "2026-07-19T10:05:00Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": { "total_token_usage": { "input_tokens": 1400, "output_tokens": 130 } }
            }
        })
        .to_string(),
    );

    let buf = read_until(resp, "\"tokensIn\":1400", Duration::from_secs(15)).await;
    assert!(buf.contains("event: session"), "expected a session event: {buf:?}");
    // Cumulative, not summed (500+1400 would be 1900).
    assert!(buf.contains("\"tokensOut\":130"), "expected cumulative tokens: {buf:?}");
}

#[tokio::test]
async fn codex_subagent_rollout_is_not_a_card() {
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let mut lines = codex_rollout("sub-1", "subagent work", 900, 90);
    // Flip the session_meta to a subagent rollout.
    lines[0] = serde_json::json!({
        "timestamp": "2026-07-19T10:00:00Z",
        "type": "session_meta",
        "payload": { "id": "sub-1", "cwd": "/Users/x/repos/bar", "thread_source": "subagent" }
    })
    .to_string();
    write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-00-sub-1.jsonl",
        &lines,
    );

    let (addr, _started) =
        spawn_server_with(claude.path().to_path_buf(), Some(codex.path().to_path_buf())).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let sessions = body["sessions"].as_array().unwrap();
    assert!(sessions.is_empty(), "subagent rollout must not render: {sessions:?}");
}

#[tokio::test]
async fn missing_codex_root_degrades_gracefully() {
    // Claude root exists; the Codex root points at a path that does not exist.
    let claude = tempfile::tempdir().unwrap();
    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("claude-only", "still here", 10, 1)],
    );
    let missing_codex = claude.path().join("no-such-codex-dir");

    let (addr, _started) =
        spawn_server_with(claude.path().to_path_buf(), Some(missing_codex)).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], "claude-only");
    assert_eq!(sessions[0]["tool"], "claude");
}
