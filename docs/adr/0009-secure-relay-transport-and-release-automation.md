# Encrypted Relay transport and immutable release automation

**Status: accepted**

**Partially superseded by ADR 0010:** accounts, token rotation, and mTLS remain out
of scope, but the Collector–Relay contract will gain a versioned privacy-safe
Attention projection and explicit capability negotiation. The encrypted transport
and release-hardening decisions below remain in effect.

Before riku becomes a public project, the two multi-machine security defaults are
made required contracts rather than deployment advice. A security audit found two
high-severity paths: the Collector-to-Relay and Relay-to-Board hops sent the shared
bearer token and the full Agent Session stream over plaintext HTTP, and the tagged
release workflow ran mutable third-party Actions before a secret-bearing publish step.

**Encrypted transport is enforced at one seam.** The pure `cli` resolver validates the
final Relay URL — after resolving flag, environment, and Config precedence — for every
Board and Collector command, and `config set relay.url` reuses the same validator
before writing. A URL is valid only if it is `https://`, or `http://` to a loopback
host (`localhost`, `127.0.0.0/8`, or `::1`). Every other value is refused: other
schemes, a missing host, embedded userinfo, and any non-loopback `http://`. Existing
Config is never rewritten; an unsafe saved URL simply fails clearly at resolution, so
an upgrade never silently keeps sending a token in cleartext. The Collector and board
HTTP clients use a TLS backend with normal certificate and hostname verification, and
no switch disables it.

**`riku relay` is a loopback-only development server.** It binds `127.0.0.1:4343` by
default and refuses a non-loopback `--addr`. This supersedes ADR 0007's "the Relay
binds a configurable address (it is the intentional network service)": native TLS is
still deliberately kept out of the Relay process, but a real multi-machine Relay is now
a loopback riku process behind a TLS-terminating reverse proxy that presents the
certificate. `docs/relay-deployment.md` documents one supported proxy shape and its
certificate, forwarding, and token requirements. The shared bearer token is unchanged;
rotation, accounts, mTLS, and a new wire protocol remain out of scope.

**Release automation is immutable and least-privilege.** Every workflow `uses:` is
pinned to a full 40-character commit SHA with the reviewed release recorded in a
trailing comment. Workflow permissions move from the repository default to per-job
scopes: the plan and build jobs are read-only, and only the separate publish job holds
`contents: write`. The Homebrew tap credential is a fine-grained token restricted to
`fskroes/homebrew-riku`, exposed only to the publish command's environment. A
`.github/scripts/check-workflow-security.sh` CI check fails on any mutable Action
reference, a `write-all` grant, or a missing `permissions:` block, so the guarantee
survives routine workflow edits.

Making the repository public and re-running the audit are tracked separately; this ADR
covers only closing the two verified findings.
