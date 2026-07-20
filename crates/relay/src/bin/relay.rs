//! The `relay` binary: the fan-in/fan-out hub for a team's Collectors and boards.
//!
//! Unlike the board (localhost only), the Relay is the intentional network service,
//! so it binds a configurable address. A single shared token gates both roles; a
//! Relay started without one refuses to run, since the token is the whole auth model
//! (ADR 0004). No config file — address and token are flags/environment, matching
//! the board's `--port`/`--root` style.

use std::net::SocketAddr;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut addr: SocketAddr = "0.0.0.0:4343".parse().expect("valid default addr");
    let mut token = std::env::var("RELAY_TOKEN").ok();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => {
                if let Some(v) = args.next() {
                    addr = v.parse().unwrap_or_else(|_| {
                        eprintln!("invalid --addr '{v}'");
                        std::process::exit(2);
                    });
                }
            }
            "--token" => token = args.next(),
            other => eprintln!("ignoring unknown argument '{other}'"),
        }
    }

    let Some(token) = token.filter(|t| !t.is_empty()) else {
        eprintln!("a shared token is required: pass --token <token> or set RELAY_TOKEN");
        std::process::exit(2);
    };

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = rt.block_on(relay::run_relay(addr, token)) {
        eprintln!("relay error: {e}");
        std::process::exit(1);
    }
}
