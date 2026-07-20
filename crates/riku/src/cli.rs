use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
            "relay.url" => self.relay.url = Some(value.to_string()),
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
    let url = resolve_precedence(relay_url, env, "RELAY_URL", config.relay.url.clone());
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
        .ok_or_else(|| "a Relay URL is required: pass --relay <http://host:port>, set RELAY_URL, or run 'riku config set relay.url …'".to_string())?;
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
    let mut addr = "0.0.0.0:4343".parse().expect("valid Relay default address");
    let mut token = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--addr" => {
                addr = next_flag_value(args, &mut index, "--addr")?
                    .parse()
                    .map_err(|_| {
                        "--addr must be a socket address such as 0.0.0.0:4343".to_string()
                    })?
            }
            "--token" => token = Some(next_flag_value(args, &mut index, "--token")?),
            option => return Err(format!("unknown option '{option}' for Relay")),
        }
        index += 1;
    }
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
        let configured = "[relay]\nurl = 'http://hub'\ntoken = 'secret'\n";
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
                    "http://hub".into()
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
                "http://flag".into(),
                "--token".into(),
                "flag-token".into(),
                "--root".into(),
                "/flag/root".into(),
            ],
            &env(&[
                ("RELAY_URL", "http://environment"),
                ("RELAY_TOKEN", "environment-token"),
                ("RIKU_ROOT", "/environment/root"),
            ]),
            Some(file),
        )
        .unwrap();

        let ResolvedCommand::Collect(options) = command else {
            panic!("expected collector")
        };
        assert_eq!(options.relay_url, "http://flag");
        assert_eq!(options.token, "flag-token");
        assert_eq!(
            options.root.as_deref(),
            Some(std::path::Path::new("/flag/root"))
        );
    }

    #[test]
    fn environment_beats_config_when_no_flag_is_supplied() {
        let file = "[relay]\nurl = 'http://saved'\ntoken = 'saved-token'\n";
        let command = resolve(
            &["collect".into()],
            &env(&[
                ("RELAY_URL", "http://environment"),
                ("RELAY_TOKEN", "environment-token"),
            ]),
            Some(file),
        )
        .unwrap();
        let ResolvedCommand::Collect(options) = command else {
            panic!("expected collector")
        };
        assert_eq!(options.relay_url, "http://environment");
        assert_eq!(options.token, "environment-token");
    }

    #[test]
    fn collector_requires_a_relay_url_and_token() {
        assert!(resolve(&["collect".into()], &env(&[]), None)
            .unwrap_err()
            .contains("Relay URL is required"));
        assert!(resolve(
            &["collect".into(), "--relay".into(), "http://hub".into()],
            &env(&[]),
            None
        )
        .unwrap_err()
        .contains("shared token is required"));
    }

    #[test]
    fn config_serialization_round_trips() {
        let mut config = Config::default();
        config.set("relay.url", "http://hub").unwrap();
        config.set("relay.token", "secret").unwrap();
        config.set("paths.root", "/sessions").unwrap();
        let parsed = Config::parse(&config.serialize().unwrap()).unwrap();
        assert_eq!(parsed, config);
    }
}
