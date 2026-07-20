//! End-to-end tests: a temp-dir projects root -> discovery -> HTTP snapshot and
//! SSE `session` / `removed` events on file append / delete.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use board::{http, runtime, Launcher, Started};
use collector::DeepLink;
use futures::StreamExt;

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

/// Start the server with a [`RecordingLauncher`] wired in, returning the recorder
/// so a test can assert which deep link `POST …/open` resolved.
async fn spawn_server_recording(root: PathBuf) -> (SocketAddr, Started, Arc<Mutex<Vec<DeepLink>>>) {
    let mut started = runtime::init(root, None, PathBuf::from("does-not-exist"));
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
    assert!(buf.contains("event: session"), "expected a session event: {buf:?}");
    // Tokens accumulated across the appended line.
    assert!(buf.contains("\"tokensIn\":150"), "expected summed tokens: {buf:?}");
    // C7: the streamed event is stamped with this machine's name, like the snapshot.
    let stamp = format!("\"machine\":\"{}\"", board::runtime::local_hostname());
    assert!(buf.contains(&stamp), "expected machine stamp {stamp:?} in: {buf:?}");
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
async fn attention_reason_surfaces_for_both_tools() {
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    // Claude: an unanswered tool_use → waiting.
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
    write_codex_rollout(codex.path(), "rollout-2026-07-19T10-00-00-codex-err.jsonl", &codex_lines);

    let (addr, _started) =
        spawn_server_with(claude.path().to_path_buf(), Some(codex.path().to_path_buf())).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/sessions"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = body["sessions"].as_array().unwrap();

    let waiting = sessions.iter().find(|s| s["id"] == "claude-wait").unwrap();
    assert_eq!(waiting["status"], "attention");
    assert_eq!(waiting["attentionReason"], "waiting");

    let errored = sessions.iter().find(|s| s["id"] == "codex-err").unwrap();
    assert_eq!(errored["status"], "attention");
    assert_eq!(errored["attentionReason"], "error");
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
    assert!(buf.contains("event: session"), "expected a session event: {buf:?}");
    assert!(buf.contains("\"status\":\"active\""), "expected active after answer: {buf:?}");
    assert!(
        buf.contains("\"attentionReason\":null"),
        "attention reason should clear: {buf:?}"
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
        &[assistant_line_in("sess-dl", &proj_cwd, "fix/W-12-download-flow")],
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
    assert_eq!(w12["effort"], "~2d");
    // The Work Link: W-12's branch names the item, so its session is attached.
    assert_eq!(w12["session"]["id"], "sess-dl");
    assert_eq!(w12["session"]["branch"], "fix/W-12-download-flow");

    let w14 = items.iter().find(|i| i["id"] == "W-14").unwrap();
    assert_eq!(w14["blockedBy"], serde_json::json!(["W-12"]));
    assert!(w14["session"].is_null(), "W-14 has no matching branch: {w14:?}");
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
    let cost = card["costUsd"].as_f64().expect("costUsd present for a priced model");
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
    assert!(card["diff"].is_null(), "no diff for a non-repo cwd: {card:?}");
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
    assert!(opened.lock().unwrap().is_empty(), "nothing should have launched");
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
