use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use riku::cli::{self, BoardOptions, CollectOptions, RelayOptions, ResolvedCommand};
use tracing::info;

fn main() {
    let config_path = match riku::config::path() {
        Ok(path) => path,
        Err(error) => fail(&error),
    };
    let config_contents = match riku::config::read(&config_path) {
        Ok(contents) => contents,
        Err(error) => fail(&error),
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let environment: BTreeMap<String, String> = std::env::vars().collect();
    let command = match cli::resolve(&args, &environment, config_contents.as_deref()) {
        Ok(command) => command,
        Err(error) => fail(&error),
    };

    match command {
        ResolvedCommand::Help => print_help(),
        ResolvedCommand::ConfigSet { key, value } => {
            set_config(&config_path, config_contents, &key, &value)
        }
        ResolvedCommand::JournalNote {
            project,
            text,
            handoff,
        } => journal_note(&project, &text, handoff),
        ResolvedCommand::JournalPurge => journal_purge(),
        ResolvedCommand::Board(options) => run_with_tracing(|| run_board(options)),
        ResolvedCommand::Collect(options) => run_with_tracing(|| run_collector(options)),
        ResolvedCommand::Relay(options) => run_with_tracing(|| run_relay(options)),
    }
}

fn run_with_tracing(run: impl FnOnce() -> Result<(), String>) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    if let Err(error) = run() {
        fail(&error);
    }
}

fn set_config(path: &std::path::Path, contents: Option<String>, key: &str, value: &str) {
    let mut config = match contents {
        Some(contents) => match cli::Config::parse(&contents) {
            Ok(config) => config,
            Err(error) => fail(&error),
        },
        None => cli::Config::default(),
    };
    if let Err(error) = config.set(key, value) {
        fail(&error);
    }
    if let Err(error) = riku::config::write(path, &config) {
        fail(&error);
    }
    println!("saved {key} to {}", path.display());
}

/// Riku as the user's pen: append their words to a project's journal, where the
/// agent's next stop entry will read them (ADR 0013).
///
/// The thread the note answered is printed rather than assumed: the CLI picks it
/// by implication — whoever spoke last — and with two threads running that may
/// not be the one the user meant. Saying which one it was is what lets them
/// notice and answer the other.
fn journal_note(project: &str, text: &str, handoff: sessions::Handoff) {
    let noted = sessions::resolve_journal_project(project)
        .and_then(|project| sessions::append_note(&project, text, handoff));
    match noted {
        Ok(noted) if noted.session.is_empty() => println!(
            "noted in {} — the first entry in this journal",
            noted.path.display()
        ),
        Ok(noted) => println!(
            "noted in {} — answering session {} ({})",
            noted.path.display(),
            noted.session,
            handoff.as_str()
        ),
        Err(error) => fail(&error),
    }
}

fn journal_purge() {
    match sessions::purge_journals() {
        Ok(removed) if removed.is_empty() => println!("no journal files to remove"),
        Ok(removed) => println!(
            "removed {} journal file{}",
            removed.len(),
            if removed.len() == 1 { "" } else { "s" }
        ),
        Err(error) => fail(&error),
    }
}

fn run_board(options: BoardOptions) -> Result<(), String> {
    if options.relay_missing_token {
        eprintln!("--relay was resolved without a token; running the Board local-only");
    }
    let (root, codex_root) = session_roots(options.root, options.codex_root);
    let relay = options.relay.map(|relay| board::runtime::RelayConfig {
        url: relay.url,
        token: relay.token,
    });
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| format!("could not start runtime: {error}"))?;
    runtime.block_on(async move {
        let started = board::runtime::init(root, codex_root, options.web_dist, relay);
        let app = board::http::router(started.state.clone());
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), options.port);
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|error| format!("failed to bind {address}: {error}"))?;
        info!("Agent Board listening on http://{address}");
        open_browser(address);
        axum::serve(listener, app)
            .await
            .map_err(|error| format!("Board server error: {error}"))?;
        Ok(())
    })
}

fn run_collector(options: CollectOptions) -> Result<(), String> {
    let (claude_root, codex_root) = session_roots(options.root, options.codex_root);
    let machine = options
        .machine
        .unwrap_or_else(board::runtime::local_hostname);
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| format!("could not start runtime: {error}"))?;
    runtime.block_on(relay::run_collector(relay::CollectorConfig {
        relay_url: options.relay_url,
        token: options.token,
        claude_root,
        codex_root,
        machine,
    }));
    Ok(())
}

fn run_relay(options: RelayOptions) -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| format!("could not start runtime: {error}"))?;
    runtime
        .block_on(relay::run_relay(options.addr, options.token))
        .map_err(|error| format!("Relay error: {error}"))
}

fn session_roots(root: Option<PathBuf>, codex_root: Option<PathBuf>) -> (PathBuf, Option<PathBuf>) {
    (
        root.unwrap_or_else(|| {
            sessions::default_root().unwrap_or_else(|| PathBuf::from(".claude/projects"))
        }),
        codex_root.or_else(sessions::codex_default_root),
    )
}

fn open_browser(address: SocketAddr) {
    #[cfg(target_os = "macos")]
    if let Err(error) = std::process::Command::new("open")
        .arg(format!("http://{address}"))
        .spawn()
    {
        eprintln!("riku: could not open the Board in a browser: {error}");
    }
}

fn print_help() {
    println!(
        "riku — Agent Board\n\nUSAGE:\n    riku [BOARD OPTIONS]\n    riku collect [OPTIONS]\n    riku relay [OPTIONS]\n    riku config set <KEY> <VALUE>\n    riku journal note <PROJECT> \"<TEXT>\" [--handoff <STATUS>]\n    riku journal --purge\n\nCOMMANDS:\n    collect     Watch this Mac's Agent Sessions and push them to a Relay\n    relay       Run a loopback Relay for local development (put a real multi-machine\n                Relay behind a TLS-terminating proxy — see docs/relay-deployment.md)\n    config      Save relay.url, relay.token, paths.root, paths.codex_root, or\n                journal.enabled\n    journal     Answer your project journal in your own words, or delete it\n\nJOURNAL:\n    The project journal is off until 'riku config set journal.enabled true'. A note\n    answers the entry that spoke last — it says which — so your correction wins on\n    the board; nothing is edited or deleted, and nothing ever leaves this machine.\n\n    note <PROJECT> \"<TEXT>\"    Append your own entry. PROJECT is a directory (use '.'\n                              for this one), or the slug of a project that already\n                              has a journal\n    --handoff <STATUS>         Where the note leaves the card: needs-you (default),\n                              needs-review, or on-track to say 'this is fine, carry on'\n    --purge                    Delete every journal file on this machine\n\nBOARD OPTIONS:\n    --port <PORT>          Board port (default: 4242)\n    --root <PATH>          Claude Code sessions root\n    --codex-root <PATH>    Codex CLI sessions root\n    --web-dist <PATH>      Serve a development UI directory instead of the embedded UI\n    --relay <URL>          Relay URL (https://…; http:// only for a loopback host)\n    --token <TOKEN>        Relay token\n\nResolution order: explicit flag, then environment, then ~/.config/riku/config.toml.\nEnvironment: RELAY_URL, RELAY_TOKEN, RIKU_ROOT, RIKU_CODEX_ROOT."
    );
}

fn fail(message: &str) -> ! {
    eprintln!("riku: {message}");
    std::process::exit(2);
}
