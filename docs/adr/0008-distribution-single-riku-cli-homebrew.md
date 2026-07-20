# Distribution: a single `riku` CLI via Homebrew

**Status: accepted**

riku is installed on macOS as one `riku` binary. Bare `riku` runs the Board;
`riku collect` runs the Collector; `riku relay` runs the Relay; and `riku config
set` writes the user-level Config. The former component binaries are not distributed.
This supersedes the “Workspace shape” binary-target statement in ADR 0007; its
runtime architecture remains intact.

The Board embeds the built `web/dist` assets at compile time. This removes any
dependence on the current working directory at runtime. `--web-dist` remains an
explicit contributor override for disk-backed development; an absent override always
uses the embedded assets.

The Collector reads `~/.config/riku/config.toml`, written mode `0600`, so a Homebrew
launchd service can run the bare `riku collect` command. Flag values override
environment variables, which override Config values. The Board and Collector are
per-Mac Homebrew components; the Relay is intentionally excluded from the formula
because it is shared infrastructure, not laptop software.

The personal `fskroes/homebrew-riku` tap starts with a source-build formula once a
public source archive is available. Tagged releases use cargo-dist to produce arm64
and x86_64 macOS bottles and update the tap. This keeps the source path verifiable
before adding release automation while giving users a single `brew install riku`
entry point.
