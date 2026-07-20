//! End-to-end tests at the HTTP boundary: fake Collectors push tagged `Event`s to a
//! bound Relay, and a subscribing board reads the merged fan-out. Assertions are on
//! the bytes that cross the wire — the `Event`s a subscriber receives — never the
//! Relay's internal maps (the ticket's testing rule). One test drives the real
//! `run_collector` loop against a temp transcript to prove machine stamping through
//! the whole pipeline.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use relay::{router, CollectorConfig, RelayState};
use reqwest::header::AUTHORIZATION;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const TOKEN: &str = "shared-secret";

/// Bind the Relay on an ephemeral port and return its address.
async fn spawn_relay(token: &str) -> SocketAddr {
    let app = router(RelayState::new(token));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// A fake Collector: a long-lived streaming `POST /collect` whose NDJSON body we feed
/// by hand. Returning the sender lets a test push events; dropping it ends the body
/// stream, simulating the Collector going offline.
fn connect_collector(addr: SocketAddr, token: &str) -> mpsc::Sender<String> {
    let (tx, rx) = mpsc::channel::<String>(16);
    let stream = ReceiverStream::new(rx).map(Ok::<_, std::io::Error>);
    let body = reqwest::Body::wrap_stream(stream);
    let url = format!("http://{addr}/collect");
    let auth = format!("Bearer {token}");
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(url)
            .header(AUTHORIZATION, auth)
            .body(body)
            .send()
            .await;
    });
    tx
}

/// One NDJSON `Upsert` line for a session `id` on `machine` (the wire shape the
/// Collector produces: a flattened Session with a `type` discriminator).
fn upsert_line(id: &str, machine: &str, activity: &str) -> String {
    let v = serde_json::json!({
        "type": "upsert",
        "id": id, "tool": "claude", "project": "p",
        "model": null, "branch": null, "cwd": null,
        "tokensIn": 0, "tokensOut": 0, "activity": activity,
        "lastEventAt": "2026-07-19T10:00:00Z",
        "status": "active", "attentionReason": null, "costUsd": null,
        "machine": machine,
    });
    format!("{v}\n")
}

/// A board's `GET /subscribe` SSE response.
async fn subscribe(addr: SocketAddr, token: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("http://{addr}/subscribe"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .unwrap()
}

/// Read an SSE response until `needle` appears (or time out), returning the buffer.
async fn read_until(resp: reqwest::Response, needle: &str, dur: Duration) -> String {
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let fut = async {
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk.expect("chunk")));
            if buf.contains(needle) {
                return buf.clone();
            }
        }
        buf.clone()
    };
    tokio::time::timeout(dur, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}; got: {buf:?}"))
}

#[tokio::test]
async fn merges_two_machines_and_fans_out_live_updates() {
    let addr = spawn_relay(TOKEN).await;

    // A board subscribes first, so it is live for everything that follows.
    let board = subscribe(addr, TOKEN).await;

    // Two machines' Collectors push one session each.
    let desk = connect_collector(addr, TOKEN);
    desk.send(upsert_line("a1", "desk", "on desk")).await.unwrap();
    let mate = connect_collector(addr, TOKEN);
    mate.send(upsert_line("b1", "mate", "on mate")).await.unwrap();

    // The subscriber receives both machines' sessions.
    let buf = read_until(board, "b1", Duration::from_secs(10)).await;
    assert!(buf.contains("a1") && buf.contains("\"machine\":\"desk\""), "missing desk: {buf:?}");
    assert!(buf.contains("b1") && buf.contains("\"machine\":\"mate\""), "missing mate: {buf:?}");

    // A later update from one machine reaches the subscriber too.
    let board = subscribe(addr, TOKEN).await;
    desk.send(upsert_line("a1", "desk", "UPDATED_MARKER")).await.unwrap();
    let buf = read_until(board, "UPDATED_MARKER", Duration::from_secs(10)).await;
    assert!(buf.contains("UPDATED_MARKER"), "update not fanned out: {buf:?}");
}

#[tokio::test]
async fn serves_a_snapshot_to_a_late_subscriber() {
    let addr = spawn_relay(TOKEN).await;

    // A Collector pushes before any board is listening.
    let desk = connect_collector(addr, TOKEN);
    desk.send(upsert_line("snap-1", "desk", "already here")).await.unwrap();

    // Give the Relay a moment to ingest, then subscribe: the current state must be
    // delivered as a snapshot, proving the self-healing re-sync (ADR 0004).
    tokio::time::sleep(Duration::from_millis(200)).await;
    let board = subscribe(addr, TOKEN).await;
    let buf = read_until(board, "snap-1", Duration::from_secs(10)).await;
    assert!(buf.contains("\"machine\":\"desk\""), "snapshot missing machine: {buf:?}");
}

#[tokio::test]
async fn collector_disconnect_removes_its_sessions() {
    let addr = spawn_relay(TOKEN).await;
    let board = subscribe(addr, TOKEN).await;

    let desk = connect_collector(addr, TOKEN);
    desk.send(upsert_line("gone-1", "desk", "here for now")).await.unwrap();

    // Read the upsert through, then drop the Collector's connection.
    let mut stream = board.bytes_stream();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    // Wait for the upsert to land.
    while !buf.contains("gone-1") {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("timed out before upsert")
            .expect("stream open")
            .expect("chunk");
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }

    // Collector goes offline: dropping the sender ends its push body stream.
    drop(desk);

    // The subscriber now receives a Removed for that machine's session.
    while !buf.contains("\"type\":\"removed\"") {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("timed out before removal")
            .expect("stream open")
            .expect("chunk");
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(buf.contains("gone-1"), "removal should name the dropped session: {buf:?}");
}

#[tokio::test]
async fn rejects_a_wrong_token_before_any_state_flows() {
    let addr = spawn_relay(TOKEN).await;

    // A Collector presenting the wrong token is rejected outright.
    let collect = reqwest::Client::new()
        .post(format!("http://{addr}/collect"))
        .header(AUTHORIZATION, "Bearer nope")
        .body("ignored")
        .send()
        .await
        .unwrap();
    assert_eq!(collect.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A board presenting the wrong token is rejected before any session bytes flow.
    let sub = reqwest::Client::new()
        .get(format!("http://{addr}/subscribe"))
        .header(AUTHORIZATION, "Bearer nope")
        .send()
        .await
        .unwrap();
    assert_eq!(sub.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// Bind a TLS listener on an ephemeral loopback port that presents a self-signed
/// certificate and completes the handshake (then drops the connection). This is a
/// certificate the system trust store does not know, so a client that verifies
/// certificates normally must reject it.
async fn spawn_self_signed_tls_server() -> SocketAddr {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_der = cert.cert.der().clone();
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()),
    );
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let _ = acceptor.accept(stream).await;
            });
        }
    });
    addr
}

/// The transport contract: the HTTP clients the Collector and board use
/// (`reqwest::Client::new()`) verify TLS certificates by default, so encrypted
/// transport cannot be silently downgraded (issue #17, User Story 12). A request to
/// an https:// endpoint serving an untrusted self-signed certificate fails with a
/// certificate error rather than connecting.
#[tokio::test]
async fn https_clients_verify_certificates_by_default() {
    // The process-wide crypto provider the rustls ServerConfig builder needs.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let addr = spawn_self_signed_tls_server().await;
    let url = format!("https://localhost:{}/health", addr.port());

    let error = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect_err("an untrusted self-signed certificate must be rejected");
    let rendered = format!("{error:?} {error}").to_lowercase();
    assert!(
        rendered.contains("certificate") || rendered.contains("unknownissuer") || rendered.contains("tls"),
        "expected a certificate verification failure, got: {rendered}"
    );
}

/// Build one Claude Code assistant transcript line.
fn assistant_line(id: &str, activity: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": id,
        "timestamp": "2026-07-19T10:00:00Z",
        "cwd": "/Users/x/repos/foo",
        "gitBranch": "main",
        "message": {
            "model": "claude-opus-4-8",
            "usage": { "input_tokens": 100, "output_tokens": 10 },
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

#[tokio::test]
async fn real_collector_loop_stamps_and_pushes_local_sessions() {
    let addr = spawn_relay(TOKEN).await;

    // A temp Claude root with one discoverable session.
    let claude = tempfile::tempdir().unwrap();
    write_transcript(
        claude.path(),
        "-Users-x-repos-foo",
        "aaaa.jsonl",
        &[assistant_line("real-1", "hello from a collector")],
    );

    // Drive the actual Collector loop against that root; it runs until the process
    // ends (this test), pushing whatever it discovers to the Relay.
    tokio::spawn(relay::run_collector(CollectorConfig {
        relay_url: format!("http://{addr}"),
        token: TOKEN.to_string(),
        claude_root: claude.path().to_path_buf(),
        codex_root: None,
        machine: "loki.local".to_string(),
    }));

    // A board subscribes and sees the discovered session, stamped with the
    // Collector's machine name — the whole watcher→stamp→push→relay→board path.
    let board = subscribe(addr, TOKEN).await;
    let buf = read_until(board, "real-1", Duration::from_secs(15)).await;
    assert!(buf.contains("\"machine\":\"loki.local\""), "expected machine stamp: {buf:?}");
    assert!(buf.contains("\"tokensIn\":100"), "expected the session's stats: {buf:?}");
}
