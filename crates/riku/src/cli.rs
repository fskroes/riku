use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::{Host, Url};

/// Persistent values that can be shared by the Board and Collector. Parsing and
/// serializing stay in this pure module; filesystem access lives in `config`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Config {
    pub relay: RelayConfig,
    pub paths: PathsConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct RelayConfig {
    pub url: Option<String>,
    pub token: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct PathsConfig {
    pub root: Option<String>,
    pub codex_root: Option<String>,
}

impl Config {
    pub fn parse(contents: &str) -> Result<Self, String> {
        toml::from_str(contents).map_err(|error| format!("invalid config: {error}"))
    }

    pub fn serialize(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|error| format!("could not serialize config: {error}"))
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "relay.url" => {
                // Refuse to persist a URL that violates the transport-security
                // policy, so an unattended Collector never reads back an unsafe
                // Config (User Story 7). The same check runs at resolution, so a
                // Config written before this rule still fails clearly on use.
                validate_relay_url(value)?;
                self.relay.url = Some(value.to_string());
            }
            "relay.token" => self.relay.token = Some(value.to_string()),
            "paths.root" => self.paths.root = Some(value.to_string()),
            "paths.codex_root" => self.paths.codex_root = Some(value.to_string()),
            _ => return Err(format!("unknown config key '{key}'; expected relay.url, relay.token, paths.root, or paths.codex_root")),
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Connection {
    pub url: String,
    pub token: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardOptions {
    pub port: u16,
    pub root: Option<PathBuf>,
    pub codex_root: Option<PathBuf>,
    pub web_dist: Option<PathBuf>,
    pub relay: Option<Connection>,
    pub relay_missing_token: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectOptions {
    pub relay_url: String,
    pub token: String,
    pub root: Option<PathBuf>,
    pub codex_root: Option<PathBuf>,
    pub machine: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayOptions {
    pub addr: SocketAddr,
    pub token: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedCommand {
    Board(BoardOptions),
    Collect(CollectOptions),
    Relay(RelayOptions),
    ConfigSet { key: String, value: String },
    Help,
}

/// Resolve invocation inputs without reading process state or touching the disk.
/// Explicit flags override environment variables, which override persistent Config.
pub fn resolve(
    args: &[String],
    env: &BTreeMap<String, String>,
    config_contents: Option<&str>,
) -> Result<ResolvedCommand, String> {
    let config = match config_contents {
        Some(contents) => Config::parse(contents)?,
        None => Config::default(),
    };
    match args.first().map(String::as_str) {
        None => resolve_board(&[], env, &config),
        Some("--help") | Some("-h") | Some("help") => Ok(ResolvedCommand::Help),
        Some("collect") => resolve_collect(&args[1..], env, &config),
        Some("relay") => resolve_relay(&args[1..], env, &config),
        Some("config") => resolve_config(&args[1..]),
        Some(first) if first.starts_with('-') => resolve_board(args, env, &config),
        Some(command) => Err(format!("unknown command '{command}'; run 'riku --help'")),
    }
}

fn resolve_board(
    args: &[String],
    env: &BTreeMap<String, String>,
    config: &Config,
) -> Result<ResolvedCommand, String> {
    let mut port = 4242;
    let mut root = None;
    let mut codex_root = None;
    let mut web_dist = None;
    let mut relay_url = None;
    let mut token = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--port" => {
                port = next_flag_value(args, &mut index, "--port")?
                    .parse()
                    .map_err(|_| "--port must be a number from 0 to 65535".to_string())?
            }
            "--root" => root = Some(next_flag_value(args, &mut index, "--root")?),
            "--codex-root" => codex_root = Some(next_flag_value(args, &mut index, "--codex-root")?),
            "--web-dist" => web_dist = Some(next_flag_value(args, &mut index, "--web-dist")?),
            "--relay" => relay_url = Some(next_flag_value(args, &mut index, "--relay")?),
            "--token" => token = Some(next_flag_value(args, &mut index, "--token")?),
            option => return Err(format!("unknown option '{option}' for Board")),
        }
        index += 1;
    }
    let url = resolve_precedence(relay_url, env, "RELAY_URL", config.relay.url.clone())
        .filter(|value| !value.is_empty());
    if let Some(url) = &url {
        // Validate the resolved URL regardless of its source (flag, environment, or
        // Config), so precedence can never smuggle in an insecure value and a saved
        // remote http:// Config fails clearly instead of leaking (User Stories 6, 8).
        validate_relay_url(url)?;
    }
    let token = resolve_precedence(token, env, "RELAY_TOKEN", config.relay.token.clone());
    let relay_missing_token = url.is_some() && token.as_deref().is_none_or(str::is_empty);
    let relay = match (url, token.filter(|value| !value.is_empty())) {
        (Some(url), Some(token)) => Some(Connection { url, token }),
        _ => None,
    };
    Ok(ResolvedCommand::Board(BoardOptions {
        port,
        root: resolve_precedence(root, env, "RIKU_ROOT", config.paths.root.clone())
            .map(PathBuf::from),
        codex_root: resolve_precedence(
            codex_root,
            env,
            "RIKU_CODEX_ROOT",
            config.paths.codex_root.clone(),
        )
        .map(PathBuf::from),
        web_dist: web_dist.map(PathBuf::from),
        relay,
        relay_missing_token,
    }))
}

fn resolve_collect(
    args: &[String],
    env: &BTreeMap<String, String>,
    config: &Config,
) -> Result<ResolvedCommand, String> {
    let mut relay_url = None;
    let mut token = None;
    let mut root = None;
    let mut codex_root = None;
    let mut machine = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--relay" => relay_url = Some(next_flag_value(args, &mut index, "--relay")?),
            "--token" => token = Some(next_flag_value(args, &mut index, "--token")?),
            "--root" => root = Some(next_flag_value(args, &mut index, "--root")?),
            "--codex-root" => codex_root = Some(next_flag_value(args, &mut index, "--codex-root")?),
            "--machine" => machine = Some(next_flag_value(args, &mut index, "--machine")?),
            option => return Err(format!("unknown option '{option}' for Collector")),
        }
        index += 1;
    }
    let relay_url = resolve_precedence(relay_url, env, "RELAY_URL", config.relay.url.clone())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "a Relay URL is required: pass --relay <https://host>, set RELAY_URL, or run 'riku config set relay.url …'".to_string())?;
    // The Collector runs unattended, so an insecure URL must fail here rather than
    // stream this machine's Sessions and token over plaintext (User Stories 1, 8).
    validate_relay_url(&relay_url)?;
    let token = resolve_precedence(token, env, "RELAY_TOKEN", config.relay.token.clone())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "a shared token is required: pass --token <token>, set RELAY_TOKEN, or run 'riku config set relay.token …'".to_string())?;
    Ok(ResolvedCommand::Collect(CollectOptions {
        relay_url,
        token,
        root: resolve_precedence(root, env, "RIKU_ROOT", config.paths.root.clone())
            .map(PathBuf::from),
        codex_root: resolve_precedence(
            codex_root,
            env,
            "RIKU_CODEX_ROOT",
            config.paths.codex_root.clone(),
        )
        .map(PathBuf::from),
        machine,
    }))
}

fn resolve_relay(
    args: &[String],
    env: &BTreeMap<String, String>,
    config: &Config,
) -> Result<ResolvedCommand, String> {
    // `riku relay` is a loopback-only development component: a real multi-machine
    // Relay is a loopback riku process behind a TLS-terminating reverse proxy (User
    // Stories 10, 11). Default to loopback and refuse a non-loopback bind below.
    let mut addr: SocketAddr = "127.0.0.1:4343"
        .parse()
        .expect("valid Relay default address");
    let mut token = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--addr" => {
                addr = next_flag_value(args, &mut index, "--addr")?
                    .parse()
                    .map_err(|_| {
                        "--addr must be a socket address such as 127.0.0.1:4343".to_string()
                    })?
            }
            "--token" => token = Some(next_flag_value(args, &mut index, "--token")?),
            option => return Err(format!("unknown option '{option}' for Relay")),
        }
        index += 1;
    }
    validate_relay_bind(&addr)?;
    let token = resolve_precedence(token, env, "RELAY_TOKEN", config.relay.token.clone())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "a shared token is required: pass --token <token>, set RELAY_TOKEN, or run 'riku config set relay.token …'".to_string())?;
    Ok(ResolvedCommand::Relay(RelayOptions { addr, token }))
}

fn resolve_config(args: &[String]) -> Result<ResolvedCommand, String> {
    match args {
        [set, key, value] if set == "set" => {
            let mut config = Config::default();
            config.set(key, value)?;
            Ok(ResolvedCommand::ConfigSet {
                key: key.clone(),
                value: value.clone(),
            })
        }
        _ => Err(
            "usage: riku config set <relay.url|relay.token|paths.root|paths.codex_root> <value>"
                .to_string(),
        ),
    }
}

fn next_flag_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn resolve_precedence(
    flag: Option<String>,
    env: &BTreeMap<String, String>,
    env_key: &str,
    file: Option<String>,
) -> Option<String> {
    flag.or_else(|| env.get(env_key).cloned()).or(file)
}

/// Enforce riku's Relay transport-security policy on a fully resolved URL.
///
/// HTTPS is always allowed. Plain `http://` is allowed only to a loopback host —
/// `localhost`, an IPv4 loopback address (`127.0.0.0/8`), or the IPv6 loopback
/// (`::1`) — the same-machine development topology. Every other value is refused so
/// the shared token and the Agent Session stream never cross a network in cleartext:
/// other schemes, a missing host, embedded userinfo, and any non-loopback `http://`.
/// The error always names the safe HTTPS alternative (User Story 5).
fn validate_relay_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|error| {
        format!("invalid Relay URL '{raw}': {error}. Use an https:// URL, or http://localhost for local development.")
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!(
            "Relay URL '{raw}' must not embed credentials; pass the shared token via --token, RELAY_TOKEN, or 'riku config set relay.token …'."
        ));
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(url.host()) => Ok(()),
        "http" => Err(format!(
            "insecure Relay URL '{raw}': plain http:// is allowed only for a loopback host (localhost, 127.0.0.1, or [::1]). Use https://{} for a remote Relay, or run the Relay behind a TLS-terminating proxy.",
            url.host_str().unwrap_or("<host>")
        )),
        scheme => Err(format!(
            "unsupported Relay URL scheme '{scheme}' in '{raw}': use https:// (or http://localhost for local development)."
        )),
    }
}

/// Whether a URL host is exactly a loopback host — the only case where plain
/// `http://` is permitted.
fn is_loopback_host(host: Option<Host<&str>>) -> bool {
    match host {
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(addr)) => addr.is_loopback(),
        Some(Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    }
}

/// `riku relay` is a local development server; it must bind a loopback address so it
/// cannot accidentally become a plaintext public service (User Story 10). A
/// multi-machine Relay is a loopback riku process behind an external TLS proxy.
fn validate_relay_bind(addr: &SocketAddr) -> Result<(), String> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err(format!(
            "'riku relay' binds a loopback address only, but '{addr}' is not loopback. A multi-machine Relay runs as a loopback riku process behind a TLS-terminating reverse proxy (see docs/relay-deployment.md). Use --addr 127.0.0.1:<port> to choose another loopback port."
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{resolve, Config, ResolvedCommand};

    fn env(values: &[(&str, &str)]) -> BTreeMap<String, String> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn dispatches_the_umbrella_commands() {
        assert!(matches!(
            resolve(&[], &env(&[]), None),
            Ok(ResolvedCommand::Board(_))
        ));
        let configured = "[relay]\nurl = 'https://hub'\ntoken = 'secret'\n";
        assert!(matches!(
            resolve(&["collect".into()], &env(&[]), Some(configured)),
            Ok(ResolvedCommand::Collect(_))
        ));
        assert!(matches!(
            resolve(&["relay".into()], &env(&[]), Some(configured)),
            Ok(ResolvedCommand::Relay(_))
        ));
        assert!(matches!(
            resolve(
                &[
                    "config".into(),
                    "set".into(),
                    "relay.url".into(),
                    "https://hub".into()
                ],
                &env(&[]),
                None
            ),
            Ok(ResolvedCommand::ConfigSet { .. })
        ));
    }

    #[test]
    fn rejects_unknown_commands_and_flags() {
        assert!(resolve(&["wat".into()], &env(&[]), None)
            .unwrap_err()
            .contains("unknown command"));
        assert!(
            resolve(&["collect".into(), "--wat".into()], &env(&[]), None)
                .unwrap_err()
                .contains("unknown option")
        );
    }

    #[test]
    fn explicit_values_beat_environment_and_config() {
        let file =
            "[relay]\nurl = 'http://saved'\ntoken = 'saved-token'\n[paths]\nroot = '/saved/root'\n";
        let command = resolve(
            &[
                "collect".into(),
                "--relay".into(),
                "https://flag".into(),
                "--token".into(),
                "flag-token".into(),
                "--root".into(),
                "/flag/root".into(),
            ],
            &env(&[
                ("RELAY_URL", "https://environment"),
                ("RELAY_TOKEN", "environment-token"),
                ("RIKU_ROOT", "/environment/root"),
            ]),
            Some(file),
        )
        .unwrap();

        let ResolvedCommand::Collect(options) = command else {
            panic!("expected collector")
        };
        assert_eq!(options.relay_url, "https://flag");
        assert_eq!(options.token, "flag-token");
        assert_eq!(
            options.root.as_deref(),
            Some(std::path::Path::new("/flag/root"))
        );
    }

    #[test]
    fn environment_beats_config_when_no_flag_is_supplied() {
        let file = "[relay]\nurl = 'https://saved'\ntoken = 'saved-token'\n";
        let command = resolve(
            &["collect".into()],
            &env(&[
                ("RELAY_URL", "https://environment"),
                ("RELAY_TOKEN", "environment-token"),
            ]),
            Some(file),
        )
        .unwrap();
        let ResolvedCommand::Collect(options) = command else {
            panic!("expected collector")
        };
        assert_eq!(options.relay_url, "https://environment");
        assert_eq!(options.token, "environment-token");
    }

    #[test]
    fn collector_requires_a_relay_url_and_token() {
        assert!(resolve(&["collect".into()], &env(&[]), None)
            .unwrap_err()
            .contains("Relay URL is required"));
        assert!(resolve(
            &["collect".into(), "--relay".into(), "https://hub".into()],
            &env(&[]),
            None
        )
        .unwrap_err()
        .contains("shared token is required"));
    }

    #[test]
    fn config_serialization_round_trips() {
        let mut config = Config::default();
        config.set("relay.url", "https://hub").unwrap();
        config.set("relay.token", "secret").unwrap();
        config.set("paths.root", "/sessions").unwrap();
        let parsed = Config::parse(&config.serialize().unwrap()).unwrap();
        assert_eq!(parsed, config);
    }

    /// The resolved Collector URL for a given source, or the resolution error.
    fn collect_url(
        flag: Option<&str>,
        env_url: Option<&str>,
        config_url: Option<&str>,
    ) -> Result<String, String> {
        let mut args = vec!["collect".to_string(), "--token".into(), "t".into()];
        if let Some(flag) = flag {
            args.push("--relay".into());
            args.push(flag.into());
        }
        let env = match env_url {
            Some(url) => env(&[("RELAY_URL", url)]),
            None => env(&[]),
        };
        let config = config_url.map(|url| format!("[relay]\nurl = '{url}'\n"));
        match resolve(&args, &env, config.as_deref())? {
            ResolvedCommand::Collect(options) => Ok(options.relay_url),
            other => panic!("expected collector, got {other:?}"),
        }
    }

    #[test]
    fn accepts_https_and_loopback_http_from_every_source() {
        // HTTPS is always allowed; loopback http:// is allowed for local development
        // (User Stories 1, 4, 9). The policy holds identically for flag, environment,
        // and Config so precedence never changes the security decision (User Story 8).
        for url in [
            "https://hub.example.com:4343",
            "http://localhost:4343",
            "http://127.0.0.1:4343",
            "http://[::1]:4343",
        ] {
            assert_eq!(
                collect_url(Some(url), None, None).as_deref(),
                Ok(url),
                "flag {url}"
            );
            assert_eq!(
                collect_url(None, Some(url), None).as_deref(),
                Ok(url),
                "env {url}"
            );
            assert_eq!(
                collect_url(None, None, Some(url)).as_deref(),
                Ok(url),
                "config {url}"
            );
        }
    }

    #[test]
    fn rejects_non_loopback_http_from_every_source() {
        // A remote plaintext URL must fail no matter how it arrives, and the error
        // must name the safe HTTPS alternative (User Stories 3, 5, 6, 8).
        let url = "http://hub.example.com:4343";
        for resolved in [
            collect_url(Some(url), None, None),
            collect_url(None, Some(url), None),
            collect_url(None, None, Some(url)), // a saved unsafe Config fails at resolution
        ] {
            let error = resolved.unwrap_err();
            assert!(
                error.contains("insecure Relay URL"),
                "unexpected error: {error}"
            );
            assert!(
                error.contains("https://hub.example.com"),
                "error should name the HTTPS fix: {error}"
            );
        }
    }

    #[test]
    fn rejects_malformed_schemes_and_embedded_credentials() {
        assert!(collect_url(Some("ftp://hub"), None, None)
            .unwrap_err()
            .contains("unsupported Relay URL scheme"));
        assert!(collect_url(Some("not a url"), None, None)
            .unwrap_err()
            .contains("invalid Relay URL"));
        // A wss:// (websocket) or file:// scheme is not http(s) either.
        assert!(collect_url(Some("wss://hub"), None, None).is_err());
        // Userinfo would carry a secret in the URL; refuse it outright.
        assert!(collect_url(Some("https://user:pass@hub"), None, None)
            .unwrap_err()
            .contains("must not embed credentials"));
    }

    #[test]
    fn board_rejects_a_saved_remote_plaintext_relay() {
        // Upgrading with an old remote http:// Config must fail clearly for the Board
        // too, never silently keep streaming the token in plaintext (User Story 6).
        let config = "[relay]\nurl = 'http://hub.example.com'\ntoken = 'secret'\n";
        let error = resolve(&[], &env(&[]), Some(config)).unwrap_err();
        assert!(
            error.contains("insecure Relay URL"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn config_set_refuses_to_persist_a_remote_plaintext_url() {
        // `config set relay.url` is the write path for an unattended Collector: it must
        // accept secure/loopback values and refuse a remote plaintext one (User Story 7).
        assert!(Config::default().set("relay.url", "https://hub").is_ok());
        assert!(Config::default()
            .set("relay.url", "http://localhost:4343")
            .is_ok());
        let error = Config::default()
            .set("relay.url", "http://hub.example.com")
            .unwrap_err();
        assert!(
            error.contains("insecure Relay URL"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn relay_binds_loopback_by_default_and_refuses_a_public_bind() {
        // The default bind is loopback so `riku relay` cannot accidentally become a
        // plaintext public service (User Story 10).
        let ResolvedCommand::Relay(options) = resolve(
            &["relay".into(), "--token".into(), "t".into()],
            &env(&[]),
            None,
        )
        .unwrap() else {
            panic!("expected relay")
        };
        assert!(
            options.addr.ip().is_loopback(),
            "default bind must be loopback: {}",
            options.addr
        );

        let error = resolve(
            &[
                "relay".into(),
                "--addr".into(),
                "0.0.0.0:4343".into(),
                "--token".into(),
                "t".into(),
            ],
            &env(&[]),
            None,
        )
        .unwrap_err();
        assert!(error.contains("loopback"), "unexpected error: {error}");
    }
}
