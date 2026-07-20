# Deploying a multi-machine Relay

riku treats encrypted Relay transport as a required contract. The Collector and the
board require an `https://` Relay URL; a remote `http://` URL is refused before any
token or Session data leaves the machine. Plain `http://` is accepted only for a
loopback host (`localhost`, `127.0.0.1`, or `[::1]`) — the same-machine development
topology.

`riku relay` is intentionally a **loopback-only development server**: it binds
`127.0.0.1:4343` by default and refuses a non-loopback `--addr`. A production Relay is
therefore a loopback riku process placed behind a TLS-terminating reverse proxy. This
keeps native TLS out of the Relay process while still giving every remote hop normal
certificate and hostname verification (riku's HTTP client verifies certificates and
has no switch to disable that).

## Supported shape: reverse proxy → loopback Relay

```
Collector / board  ──https──▶  reverse proxy (:443, TLS)  ──http──▶  127.0.0.1:4343  (riku relay)
```

1. **Run the Relay bound to loopback** on the hub, kept alive by your process manager
   (systemd, launchd, `tmux`, …):

   ```sh
   riku relay --addr 127.0.0.1:4343 --token "$RELAY_TOKEN"
   ```

2. **Terminate TLS in a reverse proxy** that forwards to `127.0.0.1:4343`. The proxy
   owns the certificate. Both Relay endpoints are long-lived streams (`POST /collect`
   is an open upload; `GET /subscribe` is Server-Sent Events), so the proxy must not
   buffer them and must allow effectively unbounded read timeouts.

   Caddy (automatic certificates via Let's Encrypt):

   ```
   relay.example.com {
       reverse_proxy 127.0.0.1:4343 {
           flush_interval -1          # stream SSE and the push body, do not buffer
       }
   }
   ```

   nginx:

   ```nginx
   server {
       listen 443 ssl;
       server_name relay.example.com;

       ssl_certificate     /etc/letsencrypt/live/relay.example.com/fullchain.pem;
       ssl_certificate_key /etc/letsencrypt/live/relay.example.com/privkey.pem;

       location / {
           proxy_pass http://127.0.0.1:4343;
           proxy_http_version 1.1;
           proxy_set_header Host $host;
           proxy_buffering off;           # required for SSE fan-out
           proxy_read_timeout 1h;         # long-lived collect/subscribe streams
       }
   }
   ```

3. **Point Collectors and boards at the HTTPS front door:**

   ```sh
   riku collect --relay https://relay.example.com --token "$RELAY_TOKEN"
   riku --relay https://relay.example.com --token "$RELAY_TOKEN"
   ```

### Requirements checklist

- **Certificate.** The proxy must present a certificate the clients' system trust
  store accepts (a public CA such as Let's Encrypt, or an internal CA installed on
  every machine). riku does not accept invalid or self-signed certificates.
- **Forwarding.** Proxy to `127.0.0.1:4343` with buffering disabled and long read
  timeouts, so the streaming push and SSE fan-out are not cut off or buffered.
- **Token.** The shared bearer token gates both roles. Keep it secret and distribute
  it out of band; the same token goes on the Relay, every Collector, and every board.
  Do **not** embed it in the URL — riku rejects a Relay URL that carries userinfo.

## Homebrew tap token

The release workflow (`.github/workflows/release.yml`) publishes the GitHub Release
and updates the personal tap `fskroes/homebrew-riku`. The tap update uses
`HOMEBREW_TAP_TOKEN`, which must be:

- **Fine-grained**, restricted to the single repository `fskroes/homebrew-riku`, with
  only Contents: read-and-write. Its compromise must not be able to modify any other
  repository.
- **Scoped to the publish step only.** The token is exposed as `GH_TOKEN` in the
  environment of the `Publish` step inside the isolated `host-and-publish` job. The
  read-only `plan` and `build-local-artifacts` jobs never see it.

**Rotation.** Before trusting any earlier release workflow run, rotate
`HOMEBREW_TAP_TOKEN` (delete the old fine-grained token, mint a new one scoped to the
tap, update the repository secret) and audit recent commits on both this repository
and the tap for anything unexpected.

## Action pinning

Every `uses:` in `.github/workflows/**` is pinned to a full 40-character commit SHA so
a moved tag or branch can never alter release execution. A trailing comment records
the human-readable release next to the SHA, e.g.:

```yaml
- uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.2.2
```

To update an Action, resolve the new release to its commit SHA and change **both** the
SHA and the version comment together:

```sh
gh api repos/actions/checkout/git/refs/tags/v4.2.2 -q '.object.sha'
```

`.github/scripts/check-workflow-security.sh` (run by the **Workflow security** CI job
on every pull request and on `main`) fails the build if any `uses:` is not pinned to a
SHA, if a workflow grants `write-all`, or if a workflow omits an explicit
`permissions:` block. Run it locally before pushing:

```sh
bash .github/scripts/check-workflow-security.sh
```
