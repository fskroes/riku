//! End-to-end tests: a temp-dir projects root -> discovery -> HTTP snapshot and
//! SSE `session` / `removed` events on file append / delete.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use board::{http, runtime, Launcher, Started};
use futures::StreamExt;
use sessions::DeepLink;

/// A [`Launcher`] that records the deep links it is asked to open instead of
/// spawning Terminal, so the open endpoint can be tested end-to-end.
#[derive(Default, Clone)]
struct RecordingLauncher {
    opened: Arc<Mutex<Vec<DeepLink>>>,
}

impl Launcher for RecordingLauncher {
    fn open(&self, link: &DeepLink) -> Result<(), String> {
        self.opened.lock().unwrap().push(link.clone());
        Ok(())
    }
}

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

/// A Claude assistant line with an explicit `cwd` and `gitBranch` — used to point a
/// session at a real project directory (so it can carry a `WORK.md`) and give it a
/// branch a Work Item id can be inferred from.
fn assistant_line_in(id: &str, cwd: &str, branch: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": id,
        "timestamp": "2026-07-19T10:00:00Z",
        "cwd": cwd,
        "gitBranch": branch,
        "message": {
            "model": "claude-opus-4-8",
            "usage": { "input_tokens": 10, "output_tokens": 1 },
            "content": [{ "type": "text", "text": "working" }]
        }
    })
    .to_string()
}

/// A Claude assistant turn that spawns a Sub-agent, in the shape Claude Code writes
/// today: an `Agent` tool-use whose id the Sub-agent's sidecar joins back on, and
/// whose `description` is the Errand.
fn claude_agent_spawn(id: &str, cwd: &str, tuid: &str, errand: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": id,
        "timestamp": "2026-07-19T10:00:00Z",
        "cwd": cwd,
        "gitBranch": "main",
        "message": {
            "model": "claude-opus-4-8",
            "stop_reason": "tool_use",
            "content": [{
                "type": "tool_use", "id": tuid, "name": "Agent",
                "input": { "description": errand, "subagent_type": "Explore" }
            }]
        }
    })
    .to_string()
}

/// The `tool_result` answering an `Agent` spawn: a launch acknowledgement, not a
/// completion.
fn claude_launch_ack(id: &str, tuid: &str) -> String {
    serde_json::json!({
        "type": "user",
        "sessionId": id,
        "cwd": "/Users/x/repos/foo",
        "message": { "content": [{
            "type": "tool_result", "tool_use_id": tuid,
            "content": "Async agent launched successfully. You will be notified automatically when it completes.",
        }] }
    })
    .to_string()
}

/// The record that says a Sub-agent has actually finished: a user turn whose whole
/// prompt is a `<task-notification>` block, arriving up to 20 minutes after the
/// launch acknowledgement. Its `<tool-use-id>` joins back to the spawn; its
/// `<status>` is the verbatim outcome word.
fn claude_task_notification(id: &str, tuid: &str, task_id: &str, status: &str) -> String {
    serde_json::json!({
        "type": "user",
        "sessionId": id,
        "cwd": "/Users/x/repos/foo",
        "timestamp": "2026-07-19T10:20:00Z",
        "message": { "role": "user", "content": notification_body(tuid, task_id, status) }
    })
    .to_string()
}

/// The `<task-notification>` block itself: the structured tags that carry the join key
/// and the outcome word. Every record form that can carry a notification carries this
/// same body.
fn notification_body(tuid: &str, task_id: &str, status: &str) -> String {
    format!(
        "<task-notification>\n<task-id>{task_id}</task-id>\n<tool-use-id>{tuid}</tool-use-id>\n<status>{status}</status>\n<summary>Agent finished</summary>\n</task-notification>"
    )
}

/// The same notification as [`claude_task_notification`], in the two forms written when
/// the child ends while its parent is **mid-turn** — a `queue-operation` and the
/// queued-command `attachment`. In that case no user turn is written at all, so these
/// are the whole record of the ending (issue #85).
fn claude_task_notification_queued(
    id: &str,
    tuid: &str,
    task_id: &str,
    status: &str,
) -> [String; 2] {
    let body = notification_body(tuid, task_id, status);
    [
        serde_json::json!({
            "type": "queue-operation", "operation": "enqueue",
            "timestamp": "2026-07-19T10:20:00Z", "sessionId": id, "content": body,
        })
        .to_string(),
        serde_json::json!({
            "type": "attachment", "sessionId": id, "isSidechain": false,
            "timestamp": "2026-07-19T10:20:00Z",
            "attachment": { "type": "queued_command", "prompt": body,
                "commandMode": "task-notification" },
        })
        .to_string(),
    ]
}

/// Write a Claude Sub-agent's own transcript and the metadata sidecar Claude writes
/// beside it at spawn, at
/// `<project>/<root-uuid>/subagents/agent-<agentId>.{jsonl,meta.json}`.
///
/// The directory is flat: a depth-2 child sits beside its depth-1 spawner, and every
/// entry in either carries the **root** session's id.
#[allow(clippy::too_many_arguments)]
fn write_sub_agent(
    claude_root: &Path,
    project_dir: &str,
    root_id: &str,
    agent_id: &str,
    tuid: &str,
    errand: &str,
    depth: u32,
    cwd: &str,
    model: &str,
    tin: u64,
    tout: u64,
) -> PathBuf {
    let dir = claude_root
        .join(project_dir)
        .join(root_id)
        .join("subagents");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("agent-{agent_id}.meta.json")),
        serde_json::json!({
            "agentType": "Explore", "description": errand,
            "toolUseId": tuid, "spawnDepth": depth,
        })
        .to_string(),
    )
    .unwrap();
    let path = dir.join(format!("agent-{agent_id}.jsonl"));
    let line = serde_json::json!({
        "type": "assistant", "isSidechain": true, "agentId": agent_id,
        "sessionId": root_id, "cwd": cwd, "gitBranch": "main",
        "timestamp": "2026-07-19T10:00:10Z",
        "message": {
            "model": model,
            "usage": { "input_tokens": tin, "output_tokens": tout },
            "content": [{ "type": "text", "text": "sub work" }]
        }
    });
    fs::write(&path, format!("{line}\n")).unwrap();
    path
}

/// A Claude assistant turn that ended to call a tool (waiting on the human).
fn claude_waiting_line(id: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": id,
        "timestamp": "2026-07-19T10:00:00Z",
        "cwd": "/Users/x/repos/foo",
        "gitBranch": "main",
        "message": {
            "model": "claude-opus-4-8",
            "stop_reason": "tool_use",
            "content": [{ "type": "tool_use", "id": "toolu_1", "name": "Bash" }]
        }
    })
    .to_string()
}

/// A user turn answering a `tool_use` — clears the wait.
fn claude_tool_result_line(id: &str) -> String {
    serde_json::json!({
        "type": "user",
        "sessionId": id,
        "cwd": "/Users/x/repos/foo",
        "message": { "content": [{ "type": "tool_result", "tool_use_id": "toolu_1", "content": "ok" }] }
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
    append_lines(path, &[line.to_string()]);
}

/// Append several records in **one** write, the way Claude Code actually flushes a
/// spawn and the acknowledgement answering it: both landed inside a single 100ms
/// sample when this was measured against a live fan-out, so a watcher never sees a
/// transcript that holds the spawn and not yet the acknowledgement.
fn append_lines(path: &Path, lines: &[String]) {
    let mut f = OpenOptions::new().append(true).open(path).unwrap();
    let mut body = lines.join("\n");
    body.push('\n');
    f.write_all(body.as_bytes()).unwrap();
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
    let started = runtime::init(claude_root, codex_root, None, None, false);
    let app = http::router(started.state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, started)
}

/// Start the server with a [`RecordingLauncher`] wired in, returning the recorder
/// so a test can assert which deep link `POST …/open` resolved.
async fn spawn_server_recording(root: PathBuf) -> (SocketAddr, Started, Arc<Mutex<Vec<DeepLink>>>) {
    let mut started = runtime::init(root, None, None, None, false);
    let recorder = RecordingLauncher::default();
    let opened = recorder.opened.clone();
    started.state.launcher = Arc::new(recorder);
    let app = http::router(started.state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, started, opened)
}

#[tokio::test]
async fn embedded_ui_is_served_without_a_web_dist_directory() {
    let temp = tempfile::tempdir().unwrap();
    let (addr, _started) = spawn_server(temp.path().to_path_buf()).await;

    let index = reqwest::get(format!("http://{addr}/")).await.unwrap();
    assert_eq!(index.status(), reqwest::StatusCode::OK);
    let body = index.text().await.unwrap();
    assert!(body.contains("<div id=\"root\"></div>"));

    let asset = body
        .split("src=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("index.html should reference its hashed JavaScript asset");
    let asset = reqwest::get(format!("http://{addr}{asset}")).await.unwrap();
    assert_eq!(asset.status(), reqwest::StatusCode::OK);
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

/// Poll the board until `done` accepts what it returns, or give up. Used where a
/// test appends to a transcript and waits for the watcher to catch up, rather than
/// asserting against whichever tick it happened to land in.
async fn wait_for<F, Fut, D>(fetch: &F, done: D) -> serde_json::Value
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = serde_json::Value>,
    D: Fn(&serde_json::Value) -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let value = fetch().await;
        if done(&value) {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the board to catch up; last saw {value:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
    // C7: a session served through the board path is stamped with this machine's
    // name, so every card is labelled (no unlabelled "local" special case).
    assert_eq!(sessions[0]["machine"], board::runtime::local_hostname());
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
    assert!(
        buf.contains("event: session"),
        "expected a session event: {buf:?}"
    );
    // Tokens accumulated across the appended line.
    assert!(
        buf.contains("\"tokensIn\":150"),
        "expected summed tokens: {buf:?}"
    );
    // C7: the streamed event is stamped with this machine's name, like the snapshot.
    let stamp = format!("\"machine\":\"{}\"", board::runtime::local_hostname());
    assert!(
        buf.contains(&stamp),
        "expected machine stamp {stamp:?} in: {buf:?}"
    );
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
    assert!(
        buf.contains("event: removed"),
        "expected a removed event: {buf:?}"
    );
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

    let (addr, _started) = spawn_server_with(
        claude.path().to_path_buf(),
        Some(codex.path().to_path_buf()),
    )
    .await;

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

    let (addr, _started) = spawn_server_with(
        claude.path().to_path_buf(),
        Some(codex.path().to_path_buf()),
    )
    .await;

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
    assert!(
        buf.contains("event: session"),
        "expected a session event: {buf:?}"
    );
    // Cumulative, not summed (500+1400 would be 1900).
    assert!(
        buf.contains("\"tokensOut\":130"),
        "expected cumulative tokens: {buf:?}"
    );
}

/// Turn a rollout into a subagent rollout spawned by `parent`, in the shape the real
/// corpus writes: the spawner at the top level, the depth inside the spawn block, and
/// a nickname that names nothing about the work. `cwd` is the parent's — a Sub-agent
/// shares it verbatim, which is the whole reason Work Link and Process Liveness must
/// never see one.
fn as_codex_subagent(lines: &mut [String], id: &str, parent: &str, depth: u32, cwd: &str) {
    lines[0] = serde_json::json!({
        "timestamp": "2026-07-19T10:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": id, "session_id": parent, "cwd": cwd,
            "thread_source": "subagent", "parent_thread_id": parent,
            "agent_nickname": "Dirac", "agent_path": "/root/spec_review",
            "source": { "subagent": { "thread_spawn": {
                "parent_thread_id": parent, "depth": depth, "agent_role": null
            }}}
        }
    })
    .to_string();
}

/// A Codex rollout rooted in a chosen working directory and branch — the two fields a
/// Work Link reads.
fn codex_rollout_in(id: &str, cwd: &str, branch: &str) -> Vec<String> {
    let mut lines = codex_rollout(id, "carrying the work", 100, 10);
    lines[0] = serde_json::json!({
        "timestamp": "2026-07-19T10:00:00Z",
        "type": "session_meta",
        "payload": { "id": id, "cwd": cwd, "thread_source": "user", "git": { "branch": branch } }
    })
    .to_string();
    lines
}

/// A Codex lifecycle `event_msg` — `task_started`, `task_complete`, `turn_aborted`.
fn codex_event(payload_type: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-07-19T10:00:04Z",
        "type": "event_msg",
        "payload": { "type": payload_type, "turn_id": "t1" }
    })
    .to_string()
}

#[tokio::test]
async fn a_codex_subagent_rollout_whose_parent_is_undiscovered_is_no_card_at_all() {
    // Held out of every roster rather than attached to a guess — and still never a
    // card, which is the half that was always true and for the wrong reason.
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let mut lines = codex_rollout("sub-1", "subagent work", 900, 90);
    as_codex_subagent(&mut lines, "sub-1", "root-missing", 1, "/Users/x/repos/bar");
    write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-00-sub-1.jsonl",
        &lines,
    );

    let (addr, _started) = spawn_server_with(
        claude.path().to_path_buf(),
        Some(codex.path().to_path_buf()),
    )
    .await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let sessions = body["sessions"].as_array().unwrap();
    assert!(
        sessions.is_empty(),
        "subagent rollout must not render: {sessions:?}"
    );
}

#[tokio::test]
async fn a_codex_fan_out_shows_its_sub_agents_on_the_parents_card() {
    // The Codex side end to end, in the mirror image of the Claude one: nothing but
    // the children's own rollouts says a Sub-agent exists, and the walk up each
    // `parent_thread_id` is what puts every row on the one node that is a card.
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();

    let parent = write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-00-codex-fanout.jsonl",
        &codex_rollout("codex-fanout", "orchestrating", 1_000, 100),
    );

    // A finished Sub-agent: it reached its terminal event, so it carries the one
    // outcome word Codex names.
    let mut done = codex_rollout("codex-sub-a", "spec review", 900_000, 900);
    as_codex_subagent(
        &mut done,
        "codex-sub-a",
        "codex-fanout",
        1,
        "/Users/x/repos/bar",
    );
    done.push(codex_event("task_started"));
    done.push(codex_event("task_complete"));
    write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-01-codex-sub-a.jsonl",
        &done,
    );

    // One still running, spawned by that Sub-agent rather than by the card: it lands
    // on the **root** all the same, after a walk through another Sub-agent.
    let mut nested = codex_rollout("codex-sub-b", "grinding", 500, 50);
    as_codex_subagent(
        &mut nested,
        "codex-sub-b",
        "codex-sub-a",
        2,
        "/Users/x/repos/bar",
    );
    nested.push(codex_event("task_started"));
    write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-02-codex-sub-b.jsonl",
        &nested,
    );

    // The parent's own rollout has been quiet for half an hour — twice the Staleness
    // window — while its Sub-agent grinds.
    age_file(&parent, 30);

    let (addr, _started) = spawn_server_with(
        claude.path().to_path_buf(),
        Some(codex.path().to_path_buf()),
    )
    .await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(
        sessions.iter().map(|s| &s["id"]).collect::<Vec<_>>(),
        vec!["codex-fanout"],
        "only the root is a card: {sessions:?}"
    );

    let card = &sessions[0];
    let roster = card["subAgentRoster"].as_array().unwrap();
    assert_eq!(roster.len(), 2, "roster was {roster:?}");
    let row = |id: &str| roster.iter().find(|r| r["id"] == id).unwrap();

    let finished = row("codex-sub-a");
    assert_eq!(finished["state"], "finished");
    assert_eq!(finished["outcome"], "completed");
    assert_eq!(finished["depth"], 1);
    assert_eq!(finished["tokensIn"], 900_000);
    // Unlabelled: no nickname, role, or path is presented as an Errand.
    assert_eq!(finished["errand"], serde_json::Value::Null);

    let running = row("codex-sub-b");
    assert_eq!(running["state"], "running");
    assert_eq!(running["outcome"], serde_json::Value::Null);
    assert_eq!(running["depth"], 2);

    // The spend that was being attributed to nothing now reaches the headline totals.
    assert_eq!(card["tokensIn"], 901_500);
    assert_eq!(card["tokensOut"], 1_050);

    // A Codex parent with a Running Sub-agent stays Running past the Staleness
    // window, and fanning out is never a human need.
    assert_eq!(card["status"], "active");
    assert_eq!(card["attention"], serde_json::Value::Null);

    // Never an id-lookup target: a Sub-agent cannot be opened or resumed, because
    // only the session that sent it can.
    let opened = reqwest::Client::new()
        .post(format!("http://{addr}/api/sessions/codex-sub-a/open"))
        .send()
        .await
        .unwrap();
    assert_eq!(opened.status(), 404);
}

#[tokio::test]
async fn a_codex_sub_agent_is_never_a_work_link_target() {
    // A Codex Sub-agent inherits its parent's working directory and its branch with
    // it, and is the more recently active of the two — so if it were ever a session,
    // `link_session`'s newest-wins pick would put the Work Item chip on a card that
    // does not exist. It cannot be: a Sub-agent never reaches the session list the
    // link is chosen from. Asserted rather than left to the type, because "it is
    // structurally impossible" is exactly the claim that rots unnoticed.
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let proj_cwd = proj.path().to_string_lossy().to_string();
    fs::write(
        proj.path().join("WORK.md"),
        "# Work Map\n\n- [ ] W-76 The work the parent is carrying\n",
    )
    .unwrap();

    write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-00-codex-parent.jsonl",
        &codex_rollout_in("codex-parent", &proj_cwd, "feat/W-76-codex-roster"),
    );
    let mut child = codex_rollout_in("codex-child", &proj_cwd, "feat/W-76-codex-roster");
    as_codex_subagent(&mut child, "codex-child", "codex-parent", 1, &proj_cwd);
    write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-01-codex-child.jsonl",
        &child,
    );

    let (addr, _started) = spawn_server_with(
        claude.path().to_path_buf(),
        Some(codex.path().to_path_buf()),
    )
    .await;

    let work: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/api/work"))
        .query(&[("cwd", proj_cwd.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let item = work["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "W-76")
        .unwrap();
    assert_eq!(
        item["session"]["id"], "codex-parent",
        "the chip stays on the parent, the only card there is: {item}"
    );
}

/// Backdate a file's modification time by `minutes`, so a test can put a transcript
/// past the 15-minute Staleness window without waiting for it.
fn age_file(path: &Path, minutes: u64) {
    let when = std::time::SystemTime::now() - Duration::from_secs(minutes * 60);
    let f = OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(fs::FileTimes::new().set_modified(when))
        .unwrap();
}

#[tokio::test]
async fn a_claude_fan_out_shows_its_sub_agents_on_the_parents_card() {
    // The whole feature at the seam a person can see: a parent transcript, two
    // Sub-agent files, and their spawn-time sidecars, written as real files —
    // through discovery, both folds, the cross-file join, the status refinement,
    // cost and token totals, and JSON serialization.
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let project = "-Users-x-repos-foo";
    let cwd = "/Users/x/repos/foo";

    // The parent: 1M in / 1M out on Opus, two spawns, and a launch acknowledgement
    // for the first — which must change nothing about its entry.
    let parent = write_transcript(
        claude.path(),
        project,
        "sess-fanout.jsonl",
        &[
            assistant_line("sess-fanout", "orchestrating", 1_000_000, 1_000_000),
            claude_agent_spawn("sess-fanout", cwd, "toolu_a", "map the parser"),
            claude_agent_spawn("sess-fanout", cwd, "toolu_b", "audit the tests"),
            claude_launch_ack("sess-fanout", "toolu_a"),
        ],
    );
    // A depth-1 Sub-agent on the cheaper Haiku: 1M in, 0 out → $0.80.
    write_sub_agent(
        claude.path(),
        project,
        "sess-fanout",
        "a1b2c3",
        "toolu_a",
        "map the parser",
        1,
        cwd,
        "claude-haiku-4-5",
        1_000_000,
        0,
    );
    // A Sub-agent spawned by that Sub-agent. Its spawn was recorded in a child
    // transcript, not the parent's — so only its own file speaks for it, and it must
    // still land on the **root's** roster.
    write_sub_agent(
        claude.path(),
        project,
        "sess-fanout",
        "a-deep",
        "toolu_nested",
        "research the API",
        2,
        cwd,
        "claude-haiku-4-5",
        500,
        50,
    );
    // A session that never fanned out: an empty roster, so no badge.
    write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-00-codex-plain.jsonl",
        &codex_rollout("codex-plain", "no fan-out here", 100, 10),
    );
    // The parent's own transcript has been quiet for half an hour — twice the
    // Staleness window, and past the 963s of observed fan-out silence.
    age_file(&parent, 30);

    let (addr, _started) = spawn_server_with(
        claude.path().to_path_buf(),
        Some(codex.path().to_path_buf()),
    )
    .await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = body["sessions"].as_array().unwrap();

    let fanout = sessions.iter().find(|s| s["id"] == "sess-fanout").unwrap();
    let roster = fanout["subAgentRoster"].as_array().unwrap();
    assert_eq!(roster.len(), 3, "roster was {roster:?}");

    // In spawn order, each carrying its Errand verbatim and what it has spent.
    let mapped = &roster[0];
    assert_eq!(mapped["errand"], "map the parser");
    assert_eq!(mapped["state"], "running");
    assert_eq!(mapped["tokensIn"], 1_000_000);
    assert_eq!(mapped["model"], "claude-haiku-4-5");
    assert_eq!(mapped["depth"], 1);
    // Outcome is absent while it runs — a word the source has not said.
    assert_eq!(mapped["outcome"], serde_json::Value::Null);

    // `toolu_b` spawned but its file has not appeared: the parent's own record is
    // enough for a row, and the row says what it was sent to do.
    let audited = roster
        .iter()
        .find(|r| r["errand"] == "audit the tests")
        .unwrap();
    assert_eq!(audited["tokensIn"], 0);

    // However deep it was spawned, a Sub-agent belongs to the root — the only node
    // that is a card. This one's spawn was never in the parent's transcript, so the
    // row exists on its own file's word, after the two the parent recorded.
    let nested = &roster[2];
    assert_eq!(nested["errand"], "research the API");
    assert_eq!(nested["depth"], 2);
    assert_eq!(nested["tokensIn"], 500);

    // Headline totals include every Sub-agent's usage, each priced at its own model:
    // Opus main (15 + 75 = 90) + Haiku children (0.80 + 0.0006), never Opus-priced.
    assert_eq!(fanout["tokensIn"], 2_000_500);
    assert_eq!(fanout["tokensOut"], 1_000_050);
    let cost = fanout["costUsd"].as_f64().unwrap();
    assert!((cost - 90.8006).abs() < 1e-6, "cost was {cost}");
    // The card's model stays the orchestrator's, never a Sub-agent's.
    assert_eq!(fanout["model"], "claude-opus-4-8");

    // A parent whose own transcript went quiet while its Sub-agents grind stays
    // Running rather than ageing into Finished — and fanning out is not a human wait.
    assert_eq!(fanout["status"], "active");
    assert_eq!(fanout["attention"], serde_json::Value::Null);
    // The legacy count-and-descriptions field is gone from the card entirely.
    assert_eq!(fanout["subAgents"], serde_json::Value::Null);

    let codex_card = sessions.iter().find(|s| s["id"] == "codex-plain").unwrap();
    assert_eq!(codex_card["subAgentRoster"], serde_json::json!([]));
}

#[tokio::test]
async fn a_completion_notification_finishes_one_roster_entry_and_leaves_the_rest() {
    // The ticket at the seam a person sees. Two Sub-agents out, one notification
    // back: a session that fanned out shows the one that has returned as Finished —
    // in the source's own word — and the one that has not as Running, while the
    // parent itself is still Running. The `failed` word does not become a human need,
    // and a notification for a backgrounded command touches nothing at all.
    let claude = tempfile::tempdir().unwrap();
    let project = "-Users-x-repos-foo";
    let cwd = "/Users/x/repos/foo";

    let parent = write_transcript(
        claude.path(),
        project,
        "sess-done.jsonl",
        &[
            assistant_line("sess-done", "orchestrating", 1_000, 100),
            claude_agent_spawn("sess-done", cwd, "toolu_a", "map the parser"),
            claude_agent_spawn("sess-done", cwd, "toolu_b", "audit the tests"),
            // The acknowledgement, ~2s after the spawn: it changes nothing.
            claude_launch_ack("sess-done", "toolu_a"),
            // A backgrounded shell command notifying under the same tag. 101 task-ids
            // appear against 59 spawns — this is why completions join by id.
            claude_task_notification("sess-done", "toolu_bash", "task-bg", "failed"),
            // The first Sub-agent's real ending, 20 minutes after its launch.
            claude_task_notification("sess-done", "toolu_a", "task-a", "completed"),
            // …and a later word about the same one, which wins.
            claude_task_notification("sess-done", "toolu_a", "task-a", "failed"),
        ],
    );
    write_sub_agent(
        claude.path(),
        project,
        "sess-done",
        "a1b2c3",
        "toolu_a",
        "map the parser",
        1,
        cwd,
        "claude-haiku-4-5",
        900,
        90,
    );
    // The parent's own transcript is long quiet; the Sub-agent still out keeps it
    // Running, so a completion is read against a live parent rather than a dead one.
    age_file(&parent, 30);

    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;
    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let card = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "sess-done")
        .unwrap()
        .clone();

    let roster = card["subAgentRoster"].as_array().unwrap();
    assert_eq!(
        roster.len(),
        2,
        "no row for a backgrounded command: {roster:?}"
    );
    assert_eq!(roster[0]["state"], "finished");
    assert_eq!(roster[0]["outcome"], "failed", "the latest word wins");
    assert_eq!(roster[0]["errand"], "map the parser");
    assert_eq!(
        roster[0]["tokensIn"], 900,
        "and it still says what it spent"
    );
    assert_eq!(roster[1]["state"], "running");
    assert_eq!(roster[1]["outcome"], serde_json::Value::Null);

    // A failed Sub-agent is reported to the agent, not to the person.
    assert_eq!(card["status"], "active");
    assert_eq!(card["attention"], serde_json::Value::Null);
}

#[tokio::test]
async fn a_roster_entry_moves_on_the_notification_and_not_on_the_acknowledgement() {
    // The lifecycle in motion rather than at rest: riku watching while a fan-out
    // happens. Spawn, then the acknowledgement ~2s later, then the notification
    // ~20 minutes after that — the order the transcript is actually written in. The
    // entry is Running across the first two and moves only on the third.
    let claude = tempfile::tempdir().unwrap();
    let project = "-Users-x-repos-foo";
    let cwd = "/Users/x/repos/foo";
    let parent = write_transcript(
        claude.path(),
        project,
        "sess-motion.jsonl",
        &[assistant_line("sess-motion", "orchestrating", 10, 1)],
    );

    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;
    let card = || async {
        let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        body["sessions"].as_array().unwrap()[0].clone()
    };
    assert_eq!(
        card().await["subAgentRoster"],
        serde_json::json!([]),
        "nothing sent out yet"
    );

    // The spawn.
    append_line(
        &parent,
        &claude_agent_spawn("sess-motion", cwd, "toolu_a", "map the parser"),
    );
    let c = wait_for(&card, |c| {
        !c["subAgentRoster"].as_array().unwrap().is_empty()
    })
    .await;
    assert_eq!(c["subAgentRoster"][0]["state"], "running");
    assert_eq!(c["subAgentRoster"][0]["errand"], "map the parser");

    // The acknowledgement, and a turn after it. Waiting for the *later* line to show
    // up is what proves the acknowledgement itself was read — a transcript is folded
    // in file order, so the entry's state at that point is its state after the
    // acknowledgement, not before the board had caught up.
    append_line(&parent, &claude_launch_ack("sess-motion", "toolu_a"));
    append_line(
        &parent,
        &assistant_line("sess-motion", "waiting on the child", 10, 1),
    );
    let c = wait_for(&card, |c| c["activity"] == "waiting on the child").await;
    assert_eq!(
        c["subAgentRoster"][0]["state"], "running",
        "an acknowledgement is not a completion: {c:?}"
    );

    // The notification.
    append_line(
        &parent,
        &claude_task_notification("sess-motion", "toolu_a", "task-a", "completed"),
    );
    let c = wait_for(&card, |c| c["subAgentRoster"][0]["state"] == "finished").await;
    assert_eq!(c["subAgentRoster"][0]["outcome"], "completed");
}

#[tokio::test]
async fn a_busy_parents_queued_notification_finishes_its_roster_entry_in_motion() {
    // The lifecycle in motion in the shape a live fan-out actually writes it, which is
    // not the shape the test above assumes (issue #85, found by running one):
    //
    //   * the spawn and its acknowledgement are flushed together, so no watcher ever
    //     sees the one without the other — the old retire-on-`tool_result` rule would
    //     not have zeroed the badge 2s after each spawn, the row would never have
    //     appeared at all;
    //   * the parent is still mid-turn when the child ends, so the notification is
    //     enqueued rather than delivered, and the user turn that carries it is never
    //     written. These two records are the whole ending.
    let claude = tempfile::tempdir().unwrap();
    let project = "-Users-x-repos-foo";
    let cwd = "/Users/x/repos/foo";
    let parent = write_transcript(
        claude.path(),
        project,
        "sess-busy.jsonl",
        &[assistant_line("sess-busy", "orchestrating", 10, 1)],
    );

    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;
    let card = || async {
        let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        body["sessions"].as_array().unwrap()[0].clone()
    };

    // Spawn and acknowledgement, in one write.
    append_lines(
        &parent,
        &[
            claude_agent_spawn("sess-busy", cwd, "toolu_a", "map the parser"),
            claude_launch_ack("sess-busy", "toolu_a"),
        ],
    );
    let c = wait_for(&card, |c| {
        !c["subAgentRoster"].as_array().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        c["subAgentRoster"][0]["state"], "running",
        "an acknowledgement flushed with its own spawn is still not a completion: {c:?}"
    );
    assert_eq!(c["subAgentRoster"][0]["errand"], "map the parser");

    // The child ends while the parent is busy: queued records only, no user turn.
    let [queued, attached] =
        claude_task_notification_queued("sess-busy", "toolu_a", "task-a", "completed");
    append_lines(&parent, &[queued, attached]);
    let c = wait_for(&card, |c| c["subAgentRoster"][0]["state"] == "finished").await;
    assert_eq!(c["subAgentRoster"][0]["outcome"], "completed");

    // Three records carried that one ending — `enqueue`, the attachment, and the
    // `remove` that dequeued it. One row, one outcome: the join is by tool-use id.
    append_line(
        &parent,
        &claude_task_notification_queued("sess-busy", "toolu_a", "task-a", "completed")[0]
            .replace("\"enqueue\"", "\"remove\""),
    );
    append_line(
        &parent,
        &assistant_line("sess-busy", "reading the child's report", 10, 1),
    );
    let c = wait_for(&card, |c| c["activity"] == "reading the child's report").await;
    assert_eq!(c["subAgentRoster"].as_array().unwrap().len(), 1);
    assert_eq!(c["subAgentRoster"][0]["outcome"], "completed");

    // The parent is now free to leave the Running band once its own transcript goes
    // quiet, which a row stuck Running would have prevented for as long as it lived.
    // That half is pinned at the fold seam, where the clock can be moved:
    // `sessions::session::tests::a_queued_notification_lets_a_quiet_parent_finish`.
    assert_eq!(c["status"], "active", "the transcript was just written to");
}

#[tokio::test]
async fn a_sub_agent_appearing_mid_flight_streams_its_roots_card() {
    // The live half: a Sub-agent's file and sidecar appear *while the board is
    // watching*, in the order Claude Code writes them — sidecar at spawn, transcript
    // filling as the Sub-agent works. Riku observes them appear and the parent's
    // roster fills, on the stream rather than only in a later snapshot.
    let claude = tempfile::tempdir().unwrap();
    let project = "-Users-x-repos-foo";
    let cwd = "/Users/x/repos/foo";
    write_transcript(
        claude.path(),
        project,
        "sess-live.jsonl",
        &[claude_agent_spawn(
            "sess-live",
            cwd,
            "toolu_a",
            "map the parser",
        )],
    );

    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;

    // Awaiting send() guarantees the handler has subscribed before we mutate.
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/events"))
        .send()
        .await
        .unwrap();

    // The Sub-agent starts: its sidecar and its first turn land together.
    write_sub_agent(
        claude.path(),
        project,
        "sess-live",
        "a1b2c3",
        "toolu_a",
        "map the parser",
        1,
        cwd,
        "claude-haiku-4-5",
        4_242,
        424,
    );

    let buf = read_until(resp, "\"tokensIn\":4242", Duration::from_secs(15)).await;

    // Parse the streamed cards rather than substring-matching the serialization —
    // an assertion that depends on two fields being adjacent silently stops being
    // able to fail the moment the struct is reordered.
    let cards: Vec<serde_json::Value> = buf
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect();
    assert!(
        !cards.is_empty(),
        "expected at least one streamed card: {buf:?}"
    );
    // Every streamed card is the **root's**; a Sub-agent never streams as one.
    for card in &cards {
        assert_eq!(
            card["id"], "sess-live",
            "a Sub-agent must never stream as a card: {card:?}"
        );
    }
    let roster = cards
        .iter()
        .rev()
        .find_map(|c| c["subAgentRoster"].as_array().filter(|r| !r.is_empty()))
        .expect("a filled roster reached the stream");
    assert_eq!(roster[0]["errand"], "map the parser");
    assert_eq!(roster[0]["state"], "running");
    assert_eq!(roster[0]["tokensIn"], 4242);
}

#[tokio::test]
async fn a_sub_agent_is_never_a_card_a_lookup_target_or_a_work_link() {
    // The three things that must keep working. A Sub-agent shares its parent's
    // branch and working directory verbatim and is more recently active than it, so
    // every one of these would go wrong if a Sub-agent were a session.
    let claude = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let proj_cwd = proj.path().to_string_lossy().to_string();
    fs::write(
        proj.path().join("WORK.md"),
        "# Work Map\n\n- [ ] W-30 The work the parent is carrying\n",
    )
    .unwrap();
    write_transcript(
        claude.path(),
        "-proj",
        "sess-parent.jsonl",
        &[assistant_line_in(
            "sess-parent",
            &proj_cwd,
            "feat/W-30-roster",
        )],
    );
    write_sub_agent(
        claude.path(),
        "-proj",
        "sess-parent",
        "a1b2c3",
        "toolu_a",
        "map the parser",
        1,
        &proj_cwd,
        "claude-haiku-4-5",
        900,
        90,
    );

    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;

    // Never a card of its own.
    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(
        sessions.iter().map(|s| &s["id"]).collect::<Vec<_>>(),
        vec!["sess-parent"],
        "a Sub-agent is not a card: {sessions:?}"
    );
    assert_eq!(sessions[0]["subAgentRoster"].as_array().unwrap().len(), 1);

    // Never returned by an id lookup — there is nothing a person could open at it.
    let missing = reqwest::Client::new()
        .post(format!("http://{addr}/api/sessions/a1b2c3/open"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    // Never a Work Link target: the chip stays on the parent, the only card there is.
    let work: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/api/work"))
        .query(&[("cwd", proj_cwd.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let w30 = work["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "W-30")
        .unwrap();
    assert_eq!(w30["session"]["id"], "sess-parent");
}

#[tokio::test]
async fn attention_cause_surfaces_for_both_tools() {
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    // Claude: an unanswered tool_use → the generic Input fallback.
    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[claude_waiting_line("claude-wait")],
    );
    // Codex: an aborted turn → error.
    let mut codex_lines = codex_rollout("codex-err", "started", 100, 10);
    codex_lines.push(
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:04Z",
            "type": "event_msg",
            "payload": { "type": "turn_aborted", "reason": "interrupted" }
        })
        .to_string(),
    );
    write_codex_rollout(
        codex.path(),
        "rollout-2026-07-19T10-00-00-codex-err.jsonl",
        &codex_lines,
    );

    let (addr, _started) = spawn_server_with(
        claude.path().to_path_buf(),
        Some(codex.path().to_path_buf()),
    )
    .await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = body["sessions"].as_array().unwrap();

    let waiting = sessions.iter().find(|s| s["id"] == "claude-wait").unwrap();
    assert_eq!(waiting["status"], "attention");
    assert_eq!(waiting["attention"]["cause"], "input");

    let errored = sessions.iter().find(|s| s["id"] == "codex-err").unwrap();
    assert_eq!(errored["status"], "attention");
    assert_eq!(errored["attention"]["cause"], "error");
}

#[tokio::test]
async fn answering_a_wait_drops_out_of_attention_over_sse() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_transcript(
        tmp.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[claude_waiting_line("sess-wait")],
    );

    let (addr, _started) = spawn_server(tmp.path().to_path_buf()).await;

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/events"))
        .send()
        .await
        .unwrap();

    // The human answers the tool_use; the card must leave Attention.
    append_line(&path, &claude_tool_result_line("sess-wait"));

    let buf = read_until(resp, "sess-wait", Duration::from_secs(15)).await;
    assert!(
        buf.contains("event: session"),
        "expected a session event: {buf:?}"
    );
    assert!(
        buf.contains("\"status\":\"active\""),
        "expected active after answer: {buf:?}"
    );
    assert!(
        buf.contains("\"attention\":null"),
        "attention should clear: {buf:?}"
    );
}

#[tokio::test]
async fn work_map_items_carry_their_work_link() {
    // A real project dir with a WORK.md, plus a Claude session whose cwd points at
    // it and whose branch names W-12.
    let claude = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let proj_cwd = proj.path().to_string_lossy().to_string();
    fs::write(
        proj.path().join("WORK.md"),
        "# Work Map\n\n\
         - [x] W-11 Onboarding polish\n\
         - [~] W-12 Release download flow (~2d)\n\
         - [ ] W-14 Auto-update banner (blocked by: W-12)\n",
    )
    .unwrap();
    write_transcript(
        claude.path(),
        "-proj",
        "aaaa.jsonl",
        &[assistant_line_in(
            "sess-dl",
            &proj_cwd,
            "fix/W-12-download-flow",
        )],
    );

    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;

    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/api/work"))
        .query(&[("cwd", proj_cwd.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["source"], "workMd");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);

    let w12 = items.iter().find(|i| i["id"] == "W-12").unwrap();
    assert_eq!(w12["status"], "doing");
    // W-12 is `[~]` *and* carries a live link, so `status` alone would pass for two
    // reasons. `sourceStatus` comes only from the marker, so it is what pins the
    // parse this test is about.
    assert_eq!(w12["sourceStatus"], "doing");
    assert_eq!(w12["effort"], "~2d");
    // The Work Link: W-12's branch names the item, so its session is attached.
    assert_eq!(w12["session"]["id"], "sess-dl");
    assert_eq!(w12["session"]["branch"], "fix/W-12-download-flow");

    let w14 = items.iter().find(|i| i["id"] == "W-14").unwrap();
    assert_eq!(w14["blockedBy"], serde_json::json!(["W-12"]));
    assert!(
        w14["session"].is_null(),
        "W-14 has no matching branch: {w14:?}"
    );
}

#[tokio::test]
async fn a_live_work_link_makes_an_unmarked_item_read_as_doing() {
    // The #66 scenario: W-20 is *unmarked* in WORK.md — nobody wrote `[~]` — but a
    // Claude session is live on `feat/W-20-...`, so the board pairs them. The chip is
    // the evidence that the work is happening; the question is whether the column agrees.
    let claude = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let proj_cwd = proj.path().to_string_lossy().to_string();
    fs::write(
        proj.path().join("WORK.md"),
        "# Work Map\n\n\
         - [ ] W-20 Unmarked, but an agent is on it\n\
         - [ ] W-21 Unmarked, and nobody is on it\n",
    )
    .unwrap();
    write_transcript(
        claude.path(),
        "-proj",
        "aaaa.jsonl",
        &[assistant_line_in(
            "sess-live",
            &proj_cwd,
            "feat/W-20-doing-it",
        )],
    );

    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;

    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/api/work"))
        .query(&[("cwd", proj_cwd.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let items = body["items"].as_array().unwrap();
    let w20 = items.iter().find(|i| i["id"] == "W-20").unwrap();

    // The premise: the Work Link exists and the session really is live.
    assert_eq!(w20["session"]["id"], "sess-live", "premise: {w20:?}");
    assert_eq!(w20["session"]["status"], "active", "premise: {w20:?}");

    // The symptom: the column the card lands in.
    assert_eq!(
        w20["status"], "doing",
        "#66: a live Work Link should make the item read as Doing, not sit in To do \
         while its own chip says an agent is active: {w20:?}"
    );
    // And the plan's own word survives, so the card can disclose the difference
    // rather than the board quietly overwriting WORK.md.
    assert_eq!(w20["sourceStatus"], "todo", "{w20:?}");

    // The control: an unmarked item with no session stays To do.
    let w21 = items.iter().find(|i| i["id"] == "W-21").unwrap();
    assert!(w21["session"].is_null(), "control: {w21:?}");
    assert_eq!(w21["status"], "todo", "control: {w21:?}");
}

#[tokio::test]
async fn a_work_link_whose_session_has_gone_quiet_does_not_claim_the_item() {
    // The other half of #66. A Work Link is inferred from the branch alone, so it
    // outlives the session's activity: an item can carry a chip for an agent that
    // stopped half an hour ago. That must not read as Doing. Backdate the transcript
    // past ACTIVITY_WINDOW (15 min) to age the session out to Finished.
    let claude = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let proj_cwd = proj.path().to_string_lossy().to_string();
    fs::write(
        proj.path().join("WORK.md"),
        "- [ ] W-30 Nobody is on this any more\n",
    )
    .unwrap();
    let transcript = write_transcript(
        claude.path(),
        "-proj",
        "aaaa.jsonl",
        &[assistant_line_in(
            "sess-gone",
            &proj_cwd,
            "feat/W-30-abandoned",
        )],
    );
    age_file(&transcript, 30);

    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;

    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/api/work"))
        .query(&[("cwd", proj_cwd.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let w30 = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "W-30")
        .unwrap();
    assert_eq!(w30["session"]["status"], "finished", "premise: {w30:?}");
    assert_eq!(
        w30["status"], "todo",
        "a Work Link that outlived its session must not claim the item: {w30:?}"
    );
    assert_eq!(w30["sourceStatus"], "todo", "{w30:?}");
}

#[tokio::test]
async fn a_live_session_does_not_un_complete_a_done_item() {
    // The third case of #66, and the one that bounds the derivation: W-40 is checked
    // off, but its branch is still busy — review fixes, a follow-up commit. Done is
    // the source asserting completion, and evidence of activity must not undo it.
    let claude = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let proj_cwd = proj.path().to_string_lossy().to_string();
    fs::write(
        proj.path().join("WORK.md"),
        "- [x] W-40 Shipped, but the branch is still busy\n",
    )
    .unwrap();
    write_transcript(
        claude.path(),
        "-proj",
        "aaaa.jsonl",
        &[assistant_line_in(
            "sess-live",
            &proj_cwd,
            "feat/W-40-review-fixes",
        )],
    );

    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;

    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/api/work"))
        .query(&[("cwd", proj_cwd.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let w40 = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "W-40")
        .unwrap();
    assert_eq!(w40["session"]["status"], "active", "premise: {w40:?}");
    assert_eq!(w40["status"], "done", "{w40:?}");
    assert_eq!(w40["sourceStatus"], "done", "{w40:?}");
}

/// Run a git command in `dir`, asserting it succeeds.
fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(status.status.success(), "git {args:?} failed: {status:?}");
}

/// A git repo with one committed file, then three uncommitted lines added to it —
/// a deterministic working-tree diff of `+3 / -0`.
fn repo_with_uncommitted_change() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let p = repo.path();
    git(p, &["-c", "init.defaultBranch=main", "init", "-q"]);
    git(p, &["config", "user.email", "test@example.com"]);
    git(p, &["config", "user.name", "Test"]);
    fs::write(p.join("base.txt"), "one\ntwo\nthree\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "base"]);
    // Append three lines to the tracked file — uncommitted, so it shows in the diff.
    fs::write(p.join("base.txt"), "one\ntwo\nthree\nfour\nfive\nsix\n").unwrap();
    repo
}

#[tokio::test]
async fn card_carries_cost_estimate_and_live_git_diff() {
    let claude = tempfile::tempdir().unwrap();
    let repo = repo_with_uncommitted_change();
    let repo_cwd = repo.path().to_string_lossy().to_string();
    // A Claude session whose cwd is the git repo above.
    write_transcript(
        claude.path(),
        "-repo",
        "aaaa.jsonl",
        &[assistant_line_in("sess-diff", &repo_cwd, "main")],
    );

    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let card = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "sess-diff")
        .unwrap();

    // Cost: assistant_line_in uses claude-opus-4-8 with 10 in / 1 out tokens.
    // 10/1e6*15 + 1/1e6*75 = 0.00015 + 0.000075 = 0.000225.
    let cost = card["costUsd"]
        .as_f64()
        .expect("costUsd present for a priced model");
    assert!((cost - 0.000225).abs() < 1e-9, "unexpected cost: {cost}");

    // Diff: three lines appended to the one tracked file.
    assert_eq!(card["diff"]["added"], 3, "card: {card:?}");
    assert_eq!(card["diff"]["removed"], 0, "card: {card:?}");
}

#[tokio::test]
async fn non_repo_cwd_reports_no_diff() {
    let claude = tempfile::tempdir().unwrap();
    // /Users/x/repos/foo does not exist, so it is not a git repo.
    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "hello", 100, 10)],
    );
    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let card = &body["sessions"].as_array().unwrap()[0];
    assert!(
        card["diff"].is_null(),
        "no diff for a non-repo cwd: {card:?}"
    );
    // Cost still shows — it needs only tokens + model, not a repo.
    assert!(card["costUsd"].as_f64().unwrap() > 0.0);
}

#[tokio::test]
async fn work_endpoint_404s_for_an_unknown_project() {
    let claude = tempfile::tempdir().unwrap();
    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "hello", 10, 1)],
    );
    let (addr, _started) = spawn_server(claude.path().to_path_buf()).await;

    let status = reqwest::Client::new()
        .get(format!("http://{addr}/api/work"))
        .query(&[("cwd", "/no/such/project")])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn open_deep_links_into_the_local_session() {
    let tmp = tempfile::tempdir().unwrap();
    write_transcript(
        tmp.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-open", "waiting on you", 10, 1)],
    );

    let (addr, _started, opened) = spawn_server_recording(tmp.path().to_path_buf()).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/sessions/sess-open/open"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["opened"], true);

    // The launcher was handed the session's own tool, id, and cwd — resolved from
    // the store, not the request.
    let links = opened.lock().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].program, "claude");
    assert_eq!(links[0].args, ["--resume", "sess-open"]);
    assert_eq!(links[0].dir, PathBuf::from("/Users/x/repos/foo"));
}

#[tokio::test]
async fn open_404s_for_an_unknown_session() {
    let tmp = tempfile::tempdir().unwrap();
    write_transcript(
        tmp.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-known", "hi", 10, 1)],
    );
    let (addr, _started, opened) = spawn_server_recording(tmp.path().to_path_buf()).await;

    let status = reqwest::Client::new()
        .post(format!("http://{addr}/api/sessions/sess-missing/open"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert!(
        opened.lock().unwrap().is_empty(),
        "nothing should have launched"
    );
}

/// Start a board with the Project Journal switched on and served out of
/// `journal_dir` — never `$XDG_DATA_HOME`, which this in-process server shares
/// with every other test in this binary and with the developer's own journal.
async fn spawn_server_with_journal(
    root: PathBuf,
    journal_dir: PathBuf,
    enabled: bool,
) -> (SocketAddr, Started) {
    let mut started = runtime::init(root, None, None, None, enabled);
    started.state.journal_dir = Some(journal_dir);
    let app = http::router(started.state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, started)
}

/// Write one project's journal into `dir`, the way the agent's stop hook does:
/// a file named for the slug of the project directory, one JSON record per line.
fn write_journal(dir: &Path, project_cwd: &str, lines: &[String]) {
    let slug = sessions::project_slug(Path::new(project_cwd));
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(format!("{slug}.jsonl")), lines.join("\n") + "\n").unwrap();
}

/// One agent journal record for the project rooted at `cwd`.
fn journal_entry(cwd: &str, session: &str, at: &str, handoff: &str, done: &str) -> String {
    journal_entry_next(cwd, session, at, handoff, done, "Review the recap endpoint")
}

/// The same, with the next step spelled out — for tests that read two projects
/// at once and have to prove which journal a field came from.
fn journal_entry_next(
    cwd: &str,
    session: &str,
    at: &str,
    handoff: &str,
    done: &str,
    next: &str,
) -> String {
    serde_json::json!({
        "v": 1,
        "project": sessions::project_slug(Path::new(cwd)),
        "session": session,
        "at": at,
        "who": "agent",
        "handoff": handoff,
        "done": [done],
        "next": next,
        "resume": { "instruction": "pick up where the endpoint left off" },
    })
    .to_string()
}

#[tokio::test]
async fn recap_serves_the_journal_with_a_resume_command_riku_built() {
    let claude = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let cwd = "/Users/x/repos/foo";

    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "working", 10, 1)],
    );
    write_journal(
        journal.path(),
        cwd,
        &[
            journal_entry(
                cwd,
                "sess-1",
                "2026-07-27T09:00:00Z",
                "on-track",
                "Read the journal",
            ),
            // The last word, and a different day's work: both have to survive
            // the trip through the endpoint.
            journal_entry(
                cwd,
                "sess-1",
                "2026-07-28T09:00:00Z",
                "needs-review",
                "Served the recap",
            ),
        ],
    );

    let (addr, _started) = spawn_server_with_journal(
        claude.path().to_path_buf(),
        journal.path().to_path_buf(),
        true,
    )
    .await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/recap"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["enabled"], true);
    let cards = body["cards"].as_array().expect("cards is a list");
    assert_eq!(cards.len(), 1, "{body}");
    let card = &cards[0];
    assert_eq!(card["project"], "foo");
    assert_eq!(card["cwd"], cwd);

    // Latest-wins across the two entries, and both days are on the board.
    let journal = &card["journal"];
    assert_eq!(journal["handoff"], "needs-review");
    assert_eq!(journal["next"], "Review the recap endpoint");
    assert_eq!(journal["days"].as_array().unwrap().len(), 2);

    // The record carried an instruction and no command; the command in the
    // payload was assembled here from the session the store resolved.
    let resume = &journal["resume"];
    assert_eq!(resume["instruction"], "pick up where the endpoint left off");
    assert_eq!(resume["command"], "claude --resume sess-1");
    assert_eq!(resume["dir"], cwd);
    assert_eq!(resume["sessionGone"], false);
}

#[tokio::test]
async fn recap_still_carries_a_journal_whose_sessions_the_store_has_forgotten() {
    // The store only discovers transcripts touched in the last 24h, so a
    // project whose last session has aged out is not a card. Its prose is still
    // on disk, and a question asked days ago is exactly what a recap must not
    // lose — it comes back as a line, keyed on the journal directory rather
    // than the store. Written with no transcript at all, which is what an
    // aged-out project looks like from in here.
    let claude = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let live = "/Users/x/repos/foo";
    let forgotten = "/Users/x/repos/attention-ledger";

    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "working", 10, 1)],
    );
    write_journal(
        journal.path(),
        live,
        &[journal_entry(
            live,
            "sess-1",
            "2026-07-28T09:00:00Z",
            "on-track",
            "Served the recap",
        )],
    );
    write_journal(
        journal.path(),
        forgotten,
        &[journal_entry_next(
            forgotten,
            "sess-old",
            "2026-07-25T09:00:00Z",
            "needs-you",
            "Drafted ADR 0012",
            // Distinct from the live project's, so the assertion below proves
            // which journal the line was read from.
            "SQLite or flat JSONL?",
        )],
    );

    let (addr, _started) = spawn_server_with_journal(
        claude.path().to_path_buf(),
        journal.path().to_path_buf(),
        true,
    )
    .await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/recap"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // The live project is a card and is not repeated as a line.
    let cards = body["cards"].as_array().expect("cards is a list");
    assert_eq!(cards.len(), 1, "{body}");
    assert_eq!(cards[0]["cwd"], live);

    let older = body["older"].as_array().expect("older is a list");
    assert_eq!(older.len(), 1, "{body}");
    assert_eq!(body["olderTotal"], 1);
    // The slug rides out whole: it is the entirety of what is known about this
    // project, and its last segment ("ledger") names nothing the user has.
    assert_eq!(older[0]["slug"], "users-x-repos-attention-ledger");
    assert_eq!(older[0]["handoff"], "needs-you");
    assert_eq!(older[0]["who"], "agent");
    // The live project's journal says "Review the recap endpoint", so this
    // proves the line was read from the forgotten project's file and not the
    // one next to it.
    assert_eq!(older[0]["next"], "SQLite or flat JSONL?");

    // The sentence survives and nothing runnable is offered: there is no
    // directory to run it in, which is why this is a line and not a card.
    assert_eq!(
        older[0]["resume"]["instruction"],
        "pick up where the endpoint left off"
    );
    assert_eq!(older[0]["resume"]["sessionGone"], true);
    // Pinned as whole key sets, because asserting one absent field passes
    // whether or not the field could ever have been there.
    let keys = |value: &serde_json::Value| {
        let mut keys: Vec<String> = value
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect();
        keys.sort_unstable();
        keys
    };
    assert_eq!(
        keys(&older[0]),
        vec![
            "ageSeconds",
            "at",
            "handoff",
            "next",
            "resume",
            "slug",
            "who"
        ],
        "{body}"
    );
    assert_eq!(
        keys(&older[0]["resume"]),
        vec!["instruction", "sessionGone"],
        "a line carries no command and no directory: {body}"
    );
}

#[tokio::test]
async fn recap_reads_nothing_while_the_journal_is_switched_off() {
    let claude = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let cwd = "/Users/x/repos/foo";

    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "working", 10, 1)],
    );
    write_journal(
        journal.path(),
        cwd,
        &[journal_entry(
            cwd,
            "sess-1",
            "2026-07-28T09:00:00Z",
            "needs-you",
            "Wrote prose nobody opted into",
        )],
    );

    let (addr, _started) = spawn_server_with_journal(
        claude.path().to_path_buf(),
        journal.path().to_path_buf(),
        false,
    )
    .await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/recap"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // The project is still on the board; its prose is not, and the payload says
    // which of the two reasons that is.
    assert_eq!(body["enabled"], false);
    let cards = body["cards"].as_array().expect("cards is a list");
    assert_eq!(cards.len(), 1, "{body}");
    assert_eq!(cards[0]["cwd"], cwd);
    assert!(
        cards[0]["journal"].is_null(),
        "prose leaked past the toggle: {body}"
    );
    // Off means untouched: the directory is not even enumerated, so a project
    // known only by its journal cannot leak out as a line either.
    let older = body["older"].as_array().expect("older is a list");
    assert!(
        older.is_empty(),
        "prose leaked past the toggle as an older line: {body}"
    );
    assert_eq!(body["olderTotal"], 0);
}

/// Answer a card the way the correction box does: the project's directory, the
/// user's words, and the Handoff Status they are leaving the card in.
async fn correct(
    addr: SocketAddr,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/api/recap/note"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}

/// The recap as the browser re-reads it after answering.
async fn read_recap(addr: SocketAddr) -> serde_json::Value {
    reqwest::get(format!("http://{addr}/api/recap"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_correction_from_a_card_appends_the_users_voice_and_the_recap_follows() {
    // The user disagrees with the agent's parting assessment and answers from
    // the card. Riku is the user's pen here: an explicit user action appends a
    // `who:"user"` record through the same path as `riku journal note`, and the
    // next read re-resolves latest-wins, so the pill and the next step change
    // without a restart (ADR 0013).
    let claude = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let cwd = "/Users/x/repos/foo";

    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "working", 10, 1)],
    );
    write_journal(
        journal.path(),
        cwd,
        &[journal_entry(
            cwd,
            "sess-1",
            "2026-07-28T09:00:00Z",
            "on-track",
            "Converted the temperatures",
        )],
    );

    let (addr, _started) = spawn_server_with_journal(
        claude.path().to_path_buf(),
        journal.path().to_path_buf(),
        true,
    )
    .await;

    let (status, body) = correct(
        addr,
        serde_json::json!({
            "cwd": cwd,
            "text": "Not done — I need Kelvin, not Celsius",
            "handoff": "needs-you",
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["noted"], true);
    assert_eq!(
        body["session"], "sess-1",
        "the note answers the thread that spoke last: {body}"
    );

    // The agent's line survives verbatim — a correction is a reply, never an
    // edit — and the user's answer is appended after it.
    let path = journal
        .path()
        .join(format!("{}.jsonl", sessions::project_slug(Path::new(cwd))));
    let text = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].contains("Converted the temperatures"));
    let appended: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(appended["who"], "user");
    assert_eq!(appended["handoff"], "needs-you");
    assert_eq!(appended["next"], "Not done — I need Kelvin, not Celsius");
    assert_eq!(appended["done"].as_array().unwrap().len(), 0);
    assert_eq!(appended["session"], "sess-1");

    // And the card the user was looking at now reads back their own words: the
    // pill is re-labelled and the next step is theirs, latest-wins.
    let recap = read_recap(addr).await;
    let card = &recap["cards"].as_array().unwrap()[0];
    assert_eq!(card["cwd"], cwd);
    assert_eq!(card["journal"]["handoff"], "needs-you");
    assert_eq!(card["journal"]["who"], "user");
    assert_eq!(
        card["journal"]["next"],
        "Not done — I need Kelvin, not Celsius"
    );
    // The agent's day survives beside it: an append adds a voice, it does not
    // replace the record of what was done.
    assert_eq!(
        card["journal"]["days"].as_array().unwrap()[0]["done"]
            .as_array()
            .unwrap()[0],
        "Converted the temperatures"
    );
}

#[tokio::test]
async fn a_first_correction_starts_a_journal_that_is_the_users_alone() {
    // A project whose agent was never wired with the stop hook has no journal at
    // all, and the user can still speak first from its card. The file the board
    // creates for them is the same file `riku journal note` would create: one
    // record, `0600`, answering no thread because there was none to answer.
    let claude = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let cwd = "/Users/x/repos/foo";

    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "working", 10, 1)],
    );

    let (addr, _started) = spawn_server_with_journal(
        claude.path().to_path_buf(),
        journal.path().to_path_buf(),
        true,
    )
    .await;

    let (status, body) = correct(
        addr,
        serde_json::json!({ "cwd": cwd, "text": "Start with the parser" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["session"], "", "there was no thread to answer: {body}");

    let path = journal
        .path()
        .join(format!("{}.jsonl", sessions::project_slug(Path::new(cwd))));
    let written: serde_json::Value =
        serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
    assert_eq!(written["who"], "user");
    // No Handoff Status was named, so it lands where `riku journal note` lands
    // one: a correction is usually the user asking for something.
    assert_eq!(written["handoff"], "needs-you");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "the journal is prose about the user's work; it is theirs alone"
        );
    }

    // And the card now reads back their words, with no resume offer to make:
    // a note finishes nothing and names no thread to re-enter.
    let recap = read_recap(addr).await;
    let card = &recap["cards"].as_array().unwrap()[0];
    assert_eq!(card["journal"]["next"], "Start with the parser");
    assert_eq!(card["journal"]["resume"]["instruction"], "");
    assert_eq!(card["journal"]["resume"]["sessionGone"], false);
}

#[tokio::test]
async fn a_correction_lowering_the_handoff_status_moves_the_card_down_the_board() {
    // The user's answer is not always a demand: "that's fine, carry on" has to
    // be able to *lower* a Handoff Status, or a card the agent left in needs-you
    // stays pinned to the top until an agent session happens to run again.
    let claude = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let asking = "/Users/x/repos/foo";
    let reviewing = "/Users/x/repos/bar";

    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "working", 10, 1)],
    );
    write_transcript(
        claude.path(),
        "-Users-x-repos-bar",
        "bbbb.jsonl",
        &[assistant_line_in("sess-2", reviewing, "main")],
    );
    write_journal(
        journal.path(),
        asking,
        &[journal_entry(
            asking,
            "sess-1",
            "2026-07-28T09:00:00Z",
            "needs-you",
            "Half of the parser",
        )],
    );
    write_journal(
        journal.path(),
        reviewing,
        &[journal_entry(
            reviewing,
            "sess-2",
            "2026-07-28T09:00:00Z",
            "needs-review",
            "All of the reader",
        )],
    );

    let (addr, _started) = spawn_server_with_journal(
        claude.path().to_path_buf(),
        journal.path().to_path_buf(),
        true,
    )
    .await;

    // The asking project leads to begin with, because needs-you comes first.
    let before = read_recap(addr).await;
    assert_eq!(before["cards"].as_array().unwrap()[0]["cwd"], asking);

    let (status, body) = correct(
        addr,
        serde_json::json!({ "cwd": asking, "text": "That's fine, carry on", "handoff": "on-track" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");

    // Answered, it drops below the project that is still waiting on a human.
    let after = read_recap(addr).await;
    let order: Vec<&str> = after["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|card| card["cwd"].as_str().unwrap())
        .collect();
    assert_eq!(order, vec![reviewing, asking], "{after}");
    assert_eq!(
        after["cards"].as_array().unwrap()[1]["journal"]["handoff"],
        "on-track"
    );
}

#[tokio::test]
async fn a_correction_is_refused_while_the_journal_is_switched_off() {
    // Off is off in both directions: the board reads nothing, and it writes
    // nothing either. A correction accepted here would create the very file the
    // user has not opted into (ADR 0013).
    let claude = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let cwd = "/Users/x/repos/foo";

    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "working", 10, 1)],
    );

    let (addr, _started) = spawn_server_with_journal(
        claude.path().to_path_buf(),
        journal.path().to_path_buf(),
        false,
    )
    .await;

    let (status, body) = correct(
        addr,
        serde_json::json!({ "cwd": cwd, "text": "Answer me", "handoff": "needs-you" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("journal.enabled"),
        "the refusal names the one command that lifts it: {body}"
    );
    assert_eq!(
        fs::read_dir(journal.path()).unwrap().count(),
        0,
        "nothing was written into a journal directory nobody opted into"
    );
}

#[tokio::test]
async fn a_correction_only_answers_a_project_the_board_knows() {
    // The only project a card can be answered from is one the board is showing.
    // Scoping the write to a known session's directory is what keeps the
    // endpoint from being pointed at an arbitrary path, the same rule
    // `GET /api/work` holds to.
    let claude = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();

    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "working", 10, 1)],
    );

    let (addr, _started) = spawn_server_with_journal(
        claude.path().to_path_buf(),
        journal.path().to_path_buf(),
        true,
    )
    .await;

    let (status, _) = correct(
        addr,
        serde_json::json!({ "cwd": "/etc", "text": "Answer me", "handoff": "needs-you" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    // And an empty correction says nothing, so there is nothing to append: the
    // same refusal `riku journal note` gives.
    let (status, body) = correct(
        addr,
        serde_json::json!({ "cwd": "/Users/x/repos/foo", "text": "   ", "handoff": "needs-you" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");

    // Neither attempt left a file behind.
    assert_eq!(fs::read_dir(journal.path()).unwrap().count(), 0);
}

/// Start a Relay on an ephemeral port, returning its base URL.
async fn spawn_relay(token: &str) -> String {
    let app = relay::router(relay::RelayState::new(token));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// Start a board subscribed to `relay_url`, with an empty local root so only remote
/// sessions can appear.
async fn spawn_board_subscribed(relay_url: String, token: String) -> (SocketAddr, Started) {
    let empty = tempfile::tempdir().unwrap();
    let started = runtime::init(
        empty.path().to_path_buf(),
        None,
        None,
        Some(runtime::RelayConfig {
            url: relay_url,
            token,
        }),
        false,
    );
    // Keep the empty root alive for the board's lifetime.
    std::mem::forget(empty);
    let app = http::router(started.state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, started)
}

#[tokio::test]
async fn subscribed_board_surfaces_a_remote_session() {
    let token = "team-token";
    let relay_url = spawn_relay(token).await;

    // A Collector on another "machine": a temp Claude root with one session.
    let remote = tempfile::tempdir().unwrap();
    write_transcript(
        remote.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("remote-1", "hi from the desktop", 100, 10)],
    );
    tokio::spawn(relay::run_collector(relay::CollectorConfig {
        relay_url: relay_url.clone(),
        token: token.to_string(),
        claude_root: remote.path().to_path_buf(),
        codex_root: None,
        machine: "remote-desk".to_string(),
    }));

    let (addr, _started) = spawn_board_subscribed(relay_url, token.to_string()).await;

    // The remote card appears in the board's own /api/sessions snapshot (so a late
    // browser sees it), labelled with the Collector's machine — proving the full
    // Collector→Relay→board-subscription path and the local+remote merge.
    let client = reqwest::Client::new();
    let mut found = None;
    for _ in 0..150 {
        let body: serde_json::Value = client
            .get(format!("http://{addr}/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(card) = body["sessions"]
            .as_array()
            .and_then(|a| a.iter().find(|s| s["id"] == "remote-1").cloned())
        {
            found = Some(card);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let card = found.expect("remote session should appear in the board snapshot");
    assert_eq!(card["machine"], "remote-desk");
    assert_eq!(card["tokensIn"], 100);

    // And the board reports it is subscribed, for the topbar pill.
    let relay_status: serde_json::Value = client
        .get(format!("http://{addr}/api/relay"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(relay_status["configured"], true);
    assert_eq!(relay_status["connected"], true);
}

#[tokio::test]
async fn subscribed_board_streams_remote_session_events() {
    let token = "team-token";
    let relay_url = spawn_relay(token).await;
    let (addr, _started) = spawn_board_subscribed(relay_url.clone(), token.to_string()).await;

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/events"))
        .send()
        .await
        .unwrap();

    let remote = tempfile::tempdir().unwrap();
    write_transcript(
        remote.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line(
            "remote-stream-1",
            "REMOTE_STREAM_MARKER",
            100,
            10,
        )],
    );
    tokio::spawn(relay::run_collector(relay::CollectorConfig {
        relay_url,
        token: token.to_string(),
        claude_root: remote.path().to_path_buf(),
        codex_root: None,
        machine: "remote-desk".to_string(),
    }));

    let buf = read_until(resp, "REMOTE_STREAM_MARKER", Duration::from_secs(15)).await;
    assert!(
        buf.contains("event: session"),
        "expected a session event: {buf:?}"
    );
    assert!(
        buf.contains("\"machine\":\"remote-desk\""),
        "expected remote machine stamp in: {buf:?}"
    );
}

/// A fake Collector: a long-lived streaming `POST /collect` whose NDJSON body the
/// test writes by hand. It is how a session shape no Collector produces any more —
/// a legacy one — reaches a board.
fn connect_fake_collector(relay_url: &str, token: &str) -> tokio::sync::mpsc::Sender<String> {
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(16);
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, std::io::Error>);
    let body = reqwest::Body::wrap_stream(stream);
    let url = format!("{relay_url}/collect");
    let auth = format!("Bearer {token}");
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(url)
            .header(reqwest::header::AUTHORIZATION, auth)
            .body(body)
            .send()
            .await;
    });
    tx
}

/// The cards `GET /api/sessions` on `addr` is serving right now.
async fn cards_on(addr: SocketAddr) -> Vec<serde_json::Value> {
    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["sessions"].as_array().cloned().unwrap_or_default()
}

/// Poll `GET /api/sessions` on `addr` until a card with `id` satisfies `done`.
async fn wait_for_card(
    addr: SocketAddr,
    id: &'static str,
    done: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let fetch = || async move { serde_json::Value::Array(cards_on(addr).await) };
    let cards = wait_for(&fetch, |cards| {
        cards
            .as_array()
            .and_then(|a| a.iter().find(|s| s["id"] == id))
            .is_some_and(&done)
    })
    .await;
    cards
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap()
        .clone()
}

#[tokio::test]
async fn a_teammates_fan_out_crosses_the_relay_intact() {
    // The whole hop, at the seam a person sees. A Collector on another machine folds
    // a session with one Sub-agent still out and one already back, pushes it to a
    // Relay, and a subscribing board serves the roster — Errands, outcome word,
    // states, and per-child spend all the way through.
    let token = "team-token";
    let relay_url = spawn_relay(token).await;

    let remote = tempfile::tempdir().unwrap();
    let project = "-Users-x-repos-foo";
    let cwd = "/Users/x/repos/foo";
    let parent = write_transcript(
        remote.path(),
        project,
        "sess-team.jsonl",
        &[
            assistant_line("sess-team", "orchestrating", 1_000_000, 1_000_000),
            claude_agent_spawn("sess-team", cwd, "toolu_a", "map the parser"),
            claude_agent_spawn("sess-team", cwd, "toolu_b", "audit the tests"),
            claude_launch_ack("sess-team", "toolu_a"),
            // The second one is back, and it did not go well.
            claude_task_notification("sess-team", "toolu_b", "task-b", "failed"),
        ],
    );
    write_sub_agent(
        remote.path(),
        project,
        "sess-team",
        "a1b2c3",
        "toolu_a",
        "map the parser",
        1,
        cwd,
        "claude-haiku-4-5",
        1_000_000,
        0,
    );
    write_sub_agent(
        remote.path(),
        project,
        "sess-team",
        "d4e5f6",
        "toolu_b",
        "audit the tests",
        1,
        cwd,
        "claude-haiku-4-5",
        4_242,
        424,
    );
    // The parent's own transcript went quiet while its Sub-agent grinds — the state
    // the roster exists to keep legible, here on somebody else's machine.
    age_file(&parent, 30);

    let (addr, _started) = spawn_board_subscribed(relay_url.clone(), token.to_string()).await;
    // Watch the teammate's board stream from before the first frame crosses.
    // Awaiting send() guarantees the handler has subscribed.
    let stream = reqwest::Client::new()
        .get(format!("http://{addr}/api/events"))
        .send()
        .await
        .unwrap();

    tokio::spawn(relay::run_collector(relay::CollectorConfig {
        relay_url,
        token: token.to_string(),
        claude_root: remote.path().to_path_buf(),
        codex_root: None,
        machine: "remote-desk".to_string(),
    }));
    // The same fixture served by a board of its own: the source card this relayed one
    // must match, rather than a set of numbers re-derived by hand in the assertions.
    let (source_addr, _source) = spawn_server(remote.path().to_path_buf()).await;

    // Everything that reached the stream is the parent's card. A Sub-agent's file
    // moves its root, and it is the root that crosses — never an event of its own.
    let buf = read_until(stream, "audit the tests", Duration::from_secs(15)).await;
    let streamed: Vec<serde_json::Value> = buf
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect();
    assert!(!streamed.is_empty(), "expected streamed cards: {buf:?}");
    for c in &streamed {
        assert_eq!(
            c["id"], "sess-team",
            "a relayed Sub-agent must never stream as a card: {c:?}"
        );
    }

    let card = wait_for_card(addr, "sess-team", |c| {
        c["subAgentRoster"].as_array().is_some_and(|r| r.len() == 2)
    })
    .await;
    assert_eq!(card["machine"], "remote-desk");

    // Every entry intact, in spawn order.
    let roster = card["subAgentRoster"].as_array().unwrap();
    let out = &roster[0];
    // The Errand crosses unreduced: the orchestrator's own words, character for
    // character, not a redaction of them. A redacted Errand would leave a remote
    // roster listing rows that say nothing.
    assert_eq!(out["errand"], "map the parser");
    assert_eq!(out["state"], "running");
    assert_eq!(out["outcome"], serde_json::Value::Null);
    assert_eq!(out["tokensIn"], 1_000_000);
    assert_eq!(out["model"], "claude-haiku-4-5");
    assert_eq!(out["depth"], 1);

    let back = &roster[1];
    assert_eq!(back["errand"], "audit the tests");
    assert_eq!(back["state"], "finished");
    // How it ended, in the source's own word — read on the teammate's board, not
    // inferred there.
    assert_eq!(back["outcome"], "failed");
    assert_eq!(back["tokensIn"], 4_242);
    assert_eq!(back["tokensOut"], 424);
    assert!(
        back["costUsd"].as_f64().is_some_and(|c| c > 0.0),
        "a Sub-agent's own cost crosses too: {back:?}"
    );

    // The relayed card says what the source card says: the same roster, and headline
    // totals that count the same Sub-agent spend.
    let source = wait_for_card(source_addr, "sess-team", |c| {
        c["subAgentRoster"].as_array().is_some_and(|r| r.len() == 2)
    })
    .await;
    assert_eq!(card["subAgentRoster"], source["subAgentRoster"]);
    assert_eq!(card["tokensIn"], source["tokensIn"]);
    assert_eq!(card["tokensOut"], source["tokensOut"]);
    assert_eq!(card["costUsd"], source["costUsd"]);
    assert_eq!(card["status"], source["status"]);
    // …and that is the parent still Running while its own transcript is half an hour
    // quiet, on a board that never saw the files.
    assert_eq!(card["status"], "active");

    // A relayed Sub-agent is no more a card here than it is at home.
    let cards = cards_on(addr).await;
    let ids: Vec<&str> = cards.iter().filter_map(|s| s["id"].as_str()).collect();
    assert_eq!(ids, vec!["sess-team"], "a Sub-agent is not a card: {ids:?}");

    // Nor an id-lookup result, nor a Work Link target. Both of those surfaces resolve
    // against the local Engine — an Open lands a terminal on *this* machine, and a
    // Work Link chip points at a card a person can open — so on a subscribing board
    // neither can reach a relayed session at all, let alone a row on its roster.
    //
    // Which is to say these two say less than the assertions above them: they would
    // hold for a relayed *parent* too, so they cannot fail for a Sub-agent-shaped
    // reason today. They are kept as the tripwire for the day one of those pools
    // learns about remote sessions — a Sub-agent shares its parent's branch and cwd
    // verbatim and is usually more recently active, so it would win both contests.
    // What actually pins the Sub-agent invariant here is what crossed the wire: the
    // stream and the card list above.
    let missing = reqwest::Client::new()
        .post(format!("http://{addr}/api/sessions/a1b2c3/open"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let work = reqwest::Client::new()
        .get(format!("http://{addr}/api/work"))
        .query(&[("cwd", cwd)])
        .send()
        .await
        .unwrap();
    assert_eq!(work.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_legacy_collector_costs_the_badge_and_not_the_card() {
    // A Collector that predates the roster sends the old count-and-descriptions
    // object. Under the roster's new field name that one is simply unknown and
    // dropped, so the session still decodes: the teammate loses the badge, not the
    // card. (A legacy object arriving where the array is expected would be a decode
    // error, and the whole card would go.)
    let token = "team-token";
    let relay_url = spawn_relay(token).await;
    let (addr, _started) = spawn_board_subscribed(relay_url.clone(), token.to_string()).await;

    let old = connect_fake_collector(&relay_url, token);
    let line = serde_json::json!({
        "type": "upsert",
        "id": "sess-old", "tool": "claude", "project": "foo",
        "model": "claude-opus-4-8", "branch": "main", "cwd": "/Users/x/repos/foo",
        "tokensIn": 1_200, "tokensOut": 340, "activity": "orchestrating",
        "lastEventAt": "2026-07-19T10:00:00Z", "status": "active",
        "costUsd": 0.5, "machine": "old-desk",
        "subAgents": { "active": 2, "descriptions": ["map the parser", "audit the tests"] }
    });
    old.send(format!("{line}\n")).await.unwrap();

    let card = wait_for_card(addr, "sess-old", |_| true).await;

    // A normal card, whole: everything the legacy Collector did say still reads.
    assert_eq!(card["machine"], "old-desk");
    assert_eq!(card["tokensIn"], 1_200);
    assert_eq!(card["tokensOut"], 340);
    assert_eq!(card["activity"], "orchestrating");
    assert_eq!(card["status"], "active");
    // …and no badge: an empty roster, not a legacy count wearing the new name. The
    // old field's contents reach the board nowhere at all.
    assert_eq!(card["subAgentRoster"], serde_json::json!([]));
    assert_eq!(card["subAgents"], serde_json::Value::Null);
    assert!(
        !card.to_string().contains("map the parser"),
        "the legacy descriptions are dropped, not smuggled in: {card:?}"
    );
}

#[tokio::test]
async fn local_only_board_reports_no_relay() {
    let tmp = tempfile::tempdir().unwrap();
    write_transcript(
        tmp.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("sess-1", "hello", 10, 1)],
    );
    let (addr, _started) = spawn_server(tmp.path().to_path_buf()).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/relay"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Zero-setup solo mode: no Relay configured (User Story 1).
    assert_eq!(body["configured"], false);
    assert_eq!(body["connected"], false);
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
