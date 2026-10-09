# Handoff: web portal and Cloudflare publication

## Goal and authorization

The user wants all existing desktop GUI functions available from a phone through
a web portal: settings, messages, activity/history, knowledge and test chat. Each
web user must connect their own Microsoft Teams account and have separate
configuration, credentials and data. The user requested Cloudflare and authorized
creating a subdomain under `elvisbrevi.cl`; the selected hostname is
`assistant.elvisbrevi.cl`.

Continue the implementation and publication already authorized. Do not ask again
whether to create the subdomain or tunnel. Ask only for genuinely missing inputs
or credentials. Communicate with the user in Spanish; repository content is English.

## Workspace and instructions

- Repository: `/workspace/personal-teams-assistant`.
- Branch: `work`; remote continuation branch: `origin/work`. The implementation
  and this handoff are committed together for the user-requested push. In a fresh
  checkout, fetch and check out `origin/work` rather than starting from `main`.
  Preserve any further working-tree changes. No PR was created.
- Read `AGENTS.md` and `docs/architecture.md` before changing code.
- Read `desktop/skills/personal-teams-assistant/SKILL.md` before operating the app.
  Its `references/web-access.md` is the deployment and account-management guide.
- Apply the managed cloud runtime skill and inspect environment readiness and
  network policy again in the new session. Previous readiness is not current proof.
- In this environment, source `/workspace/.cloud-setup/pta-env.sh` before Cargo/Bun
  commands if it still exists. It selects the toolchains and the existing development
  profile under `/workspace/.pta-dev`. Do not print its contents or dump environment
  values. Preserve the default profile, credential names and encryption key.
- Do not spawn subagents unless the user or applicable instructions authorize it.

## Completed implementation

The same binary now accepts `personal-teams-assistant --web`. It serves the existing
GUI through authenticated HTTP, binds to loopback by default at `127.0.0.1:38656`,
and works with and without the Cargo `gui` feature. Native Tauri/CLI operations
continue to use their original private IPC.

The portal includes:

- Operator-created users, Argon2 password hashes, login/logout, password changes,
  expiring HttpOnly sessions, HTTPS Secure cookies, CSRF and exact-Origin checks,
  rate limits and request/security headers. There is no public registration.
- Independent user profiles, credential stores, provider home directories and
  data. Isolated child processes do not inherit the operator's app credentials.
- One optional `--current-profile` account to access the existing desktop profile
  without copying or migrating its Microsoft session or history.
- All six existing UI sections, mobile layouts, browser activity navigation and
  the existing Microsoft PKCE/device-local callback flow adapted for remote use.
- Per-user Graph callback paths under `/webhooks/<profile-id>/graph/...`, forwarded
  to the correct private host. Existing callback validation and no-send-retry
  behavior remain intact. Duplicate Teams identities are rejected across profiles.
- Host lifecycle management independent of open browser tabs. The portal preserves
  explicitly selected assistant Start/Stop state and stops only hosts it owns.
- Operator-only hosting/data paths and repository/import confinement.

Important files:

- `src/app/web.rs`, `src/app/web/{store,profiles,tests}.rs`.
- `src/app/{control,cli,headless,skill}.rs`, `src/app.rs`, `src/security/mod.rs`.
- `src/config.rs`, `src/adapters/graph.rs` (optional callback prefix, old defaults
  unchanged).
- `desktop/ui/{transport,login,app}.js`, `desktop/ui/{index,login}.html`,
  `desktop/ui/style.css`.
- `scripts/configure-web-cloudflare.py` (Cloudflare API setup helper).
- `README.md`, `docs/architecture.md`, the operating skill and `web-access.md`.
- `Cargo.toml` and `Cargo.lock` include the Argon2 dependency.

## Validation already completed

The last complete verification cycle passed on the final implementation:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
cargo build --no-default-features --bins
git diff --check
```

Default-feature tests: **129 unit + 39 integration** passed. No-default-feature
tests: **127 unit + 39 integration** passed. Bun parsing/build checks also passed
for `app.js`, `transport.js` and `login.js`. The Python helper passed compilation.

Real-host/browser QA used temporary synthetic profiles and Chromium at mobile
390x844 and desktop 1280x900. Both passed login, all six tabs, settings save,
logout, no horizontal overflow and zero page errors. Two users had separate
settings, credential stores and data; root provider credentials were not inherited.
No real Teams sends or model calls were made.

If retained, the QA harnesses are `/workspace/scratch/pta-web-qa.py` and
`/workspace/scratch/pta-web-browser-qa.cjs`; screenshots are under
`/workspace/scratch/pta-web-qa/`. They are outside the repository and may not
survive a different environment. Unit tests are checked into the working tree.

Do not repeat all completed checks merely because this is a new session. If code
changes, run the appropriate checks and all gates required by `AGENTS.md` before
delivering the new implementation.

## Exact publication status and blocker

**The portal has not been publicly deployed. No new Cloudflare DNS record or tunnel
has been created by this work. No production web account has been created.**

The user created/configured `CLOUDFLARE_API_TOKEN`, but the previous running
environment did not receive it. Both the direct environment and the setup script
still lacked `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_API_TOKEN_FILE`. The managed
environment status still described its earlier configuration. This is why the user
requested a new-session handoff: the next environment should load the new variable.

The earlier environment had `CLOUDFLARE_TUNNEL_TOKEN`, which is a connector
credential for an existing tunnel. It cannot administer DNS. **Do not start, modify
or reuse that existing tunnel for this portal**, since it may serve the existing app.
Also preserve the existing landing at `teams-assistant.elvisbrevi.cl`.

The requested API token permissions are Zone Read and DNS Edit for `elvisbrevi.cl`,
plus Cloudflare Tunnel Edit for the corresponding account. Credentials must come
from environment secret bindings or protected files, never chat, argv, git or logs.

## Next steps

1. Fetch/check out the `work` branch and confirm that the implementation and new
   files are present. Inspect the new environment's credential readiness and policy.
   Check token presence by name only; never print its value. If it is still
   unavailable, explain that exact blocker.

2. Read the deployment guide and inspect the chosen profile with the new CLI's
   local `web status` / `web users list` commands. These are local admin operations.
   Use the built binaries in `target/debug` if available, not an older installed CLI.
   Avoid unrelated start/stop operations on the existing desktop profile.

3. Run the authorized Cloudflare setup when the API token is ready:

   ```sh
   cd /workspace/personal-teams-assistant
   python3 scripts/configure-web-cloudflare.py \
     --domain elvisbrevi.cl --hostname assistant.elvisbrevi.cl \
     --token-file /workspace/.pta-web-secrets/cloudflared-token
   ```

   The helper creates or reuses the dedicated remotely managed tunnel named
   `personal-teams-assistant-web`, configures the origin
   `http://127.0.0.1:38656`, preserves the public Host header, adds a 404 catch-all,
   writes the new connector token privately (0600), and creates a proxied CNAME.
   It refuses to replace unrelated DNS or different tunnel routes. Read API errors
   safely; never dump API bodies or credentials. The token output file belongs
   outside the repository. The command above is for this workspace; use a durable
   private service directory when installing on a permanent host.

4. Select/confirm an always-on origin host and durable service lifecycle before
   claiming permanent publication. The managed development workspace is not proven
   to be a permanent production server. Cloudflare Tunnel supplies access/TLS, but
   the Rust portal and per-user assistants still need a running origin. The user
   has not yet selected a permanent server. Preserve existing data and credentials
   rather than silently migrating the default profile to another machine.

5. Configure the portal's own public URL with JSON on stdin:

   ```sh
   target/debug/pta web configure <<'JSON'
   {"bind":"127.0.0.1:38656","public_url":"https://assistant.elvisbrevi.cl","session_hours":12}
   JSON
   ```

   Create the initial web account using a password through protected stdin (12–256
   characters). Obtain the username/profile choice and a secure password setup if
   needed; do not invent a public weak password or ask for a password in chat.
   `--current-profile` is available for the owner's existing GUI/history; ordinary
   accounts create isolated profiles for their own Teams identities.

6. Start the portal and its dedicated connector as separately managed services:

   ```sh
   target/debug/personal-teams-assistant --web
   # Separate service; only the credential path goes in the command environment:
   TUNNEL_TOKEN_FILE=/workspace/.pta-web-secrets/cloudflared-token cloudflared tunnel run
   ```

   The previous workspace installed Cloudflared at
   `/workspace/.cloud-setup/bin/cloudflared`. Check availability and supported
   `TUNNEL_TOKEN_FILE` configuration again. Do not supply token contents in argv.

7. Diagnose real network failures against the supported policy; do not bypass it.
   The old HTTP policy allowed `api.cloudflare.com`, but did not yet allow
   `assistant.elvisbrevi.cl`. It had no verified Cloudflare edge TCP grants.
   A new environment may differ. Tunnel edge connectivity and external HTTPS
   verification require the applicable permitted routes. Do not claim success from
   DNS setup alone. If Cloudflare Access is used, Graph's exact `/webhooks/*` paths
   must bypass interactive Access; the application still authenticates web users.

8. Verify `https://assistant.elvisbrevi.cl/login`, expected security headers,
   HTTPS cookies, authenticated mobile access and independent user state. Reuse
   synthetic checks for isolation; do not send real Teams messages as a smoke test.
   Report precisely which stages are complete and any actual remaining blocker.

## Last user request

The user requested this handoff to continue in another session that loads the new
environment variables, then explicitly requested a Git push. This document is the
continuation context. The goal remains public web access with independent users,
not a new implementation from scratch. Pushing the code does not publish the portal;
the Cloudflare/origin setup above is still pending.
