//! The `collector` binary: a headless watcher that pushes one machine's Agent
//! Sessions to a Relay. No web UI — it is a lightweight background process on a
//! machine no one is looking at (User Story 15).
//!
//! It watches the same Claude Code and Codex CLI roots the board understands, so it
//! reports the same set of sessions the board would locally (User Story 16). The
//! Relay address and shared token come from flags/environment; the machine label
//! defaults to the host's name and can be overridden for clarity.

use std::path::PathBuf;

use relay::CollectorConfig;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut relay_url: Option<String> = None;
    let mut token = std::env::var("RELAY_TOKEN").ok();
    let mut claude_root =
        collector::default_root().unwrap_or_else(|| PathBuf::from(".claude/projects"));
    let mut codex_root = collector::codex_default_root();
    let mut machine = local_hostname();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--relay" => relay_url = args.next(),
            "--token" => token = args.next(),
            "--root" => {
                if let Some(v) = args.next() {
                    claude_root = PathBuf::from(v);
                }
            }
            "--codex-root" => {
                if let Some(v) = args.next() {
                    codex_root = Some(PathBuf::from(v));
                }
            }
            "--machine" => {
                if let Some(v) = args.next() {
                    machine = v;
                }
            }
            other => eprintln!("ignoring unknown argument '{other}'"),
        }
    }

    let Some(relay_url) = relay_url else {
        eprintln!("a Relay URL is required: pass --relay <http://host:port>");
        std::process::exit(2);
    };
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        eprintln!("a shared token is required: pass --token <token> or set RELAY_TOKEN");
        std::process::exit(2);
    };

    let config = CollectorConfig {
        relay_url,
        token,
        claude_root,
        codex_root,
        machine,
    };

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(relay::run_collector(config));
}

/// This machine's name, for stamping every Session it pushes. Falls back to
/// `unknown` if the OS hostname cannot be read, so a card is never left unlabelled.
fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}
