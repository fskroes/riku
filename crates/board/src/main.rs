//! The `board` binary: an axum server that serves the web UI and streams live
//! Agent Session updates from the [`collector`] (Claude Code + Codex CLI sources).
//!
//! Binds `127.0.0.1` only — the board is a local mission-control view, not a
//! network service.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use board::runtime::RelayConfig;
use board::{http, runtime};
use tracing::info;

struct Config {
    port: u16,
    root: PathBuf,
    codex_root: Option<PathBuf>,
    web_dist: PathBuf,
    relay: Option<RelayConfig>,
}

fn parse_config() -> Config {
    let mut port = 4242u16;
    let mut root = collector::default_root().unwrap_or_else(|| PathBuf::from(".claude/projects"));
    // Codex CLI sessions, honoring CODEX_HOME. `None` only if we cannot resolve a
    // home dir at all; the source then simply finds nothing.
    let mut codex_root = collector::codex_default_root();
    let mut web_dist = PathBuf::from("web/dist");
    // Relay subscription (C7). Absent → local-only, zero-setup solo mode. The token
    // may also come from the environment, matching the Collector and Relay.
    let mut relay_url: Option<String> = None;
    let mut token = std::env::var("RELAY_TOKEN").ok();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                if let Some(v) = args.next() {
                    port = v.parse().unwrap_or_else(|_| {
                        eprintln!("invalid --port '{v}', using {port}");
                        port
                    });
                }
            }
            "--root" => {
                if let Some(v) = args.next() {
                    root = PathBuf::from(v);
                }
            }
            "--codex-root" => {
                if let Some(v) = args.next() {
                    codex_root = Some(PathBuf::from(v));
                }
            }
            "--web-dist" => {
                if let Some(v) = args.next() {
                    web_dist = PathBuf::from(v);
                }
            }
            "--relay" => relay_url = args.next(),
            "--token" => token = args.next(),
            other => eprintln!("ignoring unknown argument '{other}'"),
        }
    }

    // A Relay is subscribed to only when both a URL and a token are present; a URL
    // without a token is a misconfiguration we refuse rather than connect insecurely.
    let relay = match (relay_url, token.filter(|t| !t.is_empty())) {
        (Some(url), Some(token)) => Some(RelayConfig { url, token }),
        (Some(_), None) => {
            eprintln!("--relay given without a token; pass --token <token> or set RELAY_TOKEN. Running local-only.");
            None
        }
        _ => None,
    };

    Config { port, root, codex_root, web_dist, relay }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = parse_config();
    let started = runtime::init(config.root, config.codex_root, config.web_dist, config.relay);
    let app = http::router(started.state.clone());

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), config.port);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };
    info!("Agent Board listening on http://{addr}");
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }

    // Keep the watcher alive for the process lifetime.
    drop(started.watch_guard);
}
