//! The `board` binary: an axum server that serves the web UI and streams live
//! Agent Session updates from the Claude Code [`collector`].
//!
//! Binds `127.0.0.1` only — the board is a local mission-control view, not a
//! network service.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use board::{http, runtime};
use tracing::info;

struct Config {
    port: u16,
    root: PathBuf,
    web_dist: PathBuf,
}

fn parse_config() -> Config {
    let mut port = 4242u16;
    let mut root = collector::default_root().unwrap_or_else(|| PathBuf::from(".claude/projects"));
    let mut web_dist = PathBuf::from("web/dist");

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
            "--web-dist" => {
                if let Some(v) = args.next() {
                    web_dist = PathBuf::from(v);
                }
            }
            other => eprintln!("ignoring unknown argument '{other}'"),
        }
    }
    Config { port, root, web_dist }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = parse_config();
    let started = runtime::init(config.root, config.web_dist);
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
