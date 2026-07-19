//! End-to-end tests: a temp-dir projects root -> discovery -> HTTP snapshot and
//! SSE `session` / `removed` events on file append / delete.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use board::{http, runtime, Started};
use futures::StreamExt;

/// Build one assistant transcript line.
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
    let started = runtime::init(root, PathBuf::from("does-not-exist"));
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
