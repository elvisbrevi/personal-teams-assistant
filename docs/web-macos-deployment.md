# Deploying the Access portal on the existing Mac

The origin remains the owner's Mac and the initial account is `elvis`, attached
to the existing desktop profile. The dedicated portal and tunnel LaunchAgents
were installed on 2026-10-09; inspect them before changing anything. Cloudflare
reinspection on 2026-10-10 at 10:00 America/Santiago found the dedicated web tunnel
healthy with four connections, superseding the earlier down observation. This
does not verify the Mac binary version, origin response or Access login. Keep the
Mac awake, connected and logged into the ordinary application owner's macOS account.

These steps run **on that Mac**, without a `PTA_PROFILE_DIR` override. They update
the existing Cargo host/CLI together and preserve Tauri, data, credentials,
Microsoft OAuth/scopes, the encryption key and separate desktop Teams tunnel.
The October 10 Mac rollout is complete; its live Mac results, renewed Microsoft
MFA, verified web Start and remaining phone checks are recorded in the latest
handoff. The earlier cloud session had no Mac
execution channel.

## Inspect and build before the transition

Read the repository's `AGENTS.md`, architecture, latest branch handoff and Access
operating guide before checking out the pinned implementation for compilation;
retain the latest handoff's continuation state, as that older source commit has
the earlier documentation snapshot. Do
not reset or clean an existing checkout. Inspect its status, the installed
versions, `pta --json status`, `pta --json web status`, `pta web users list`, and
the two existing `dev.personalteams.assistant.web[.tunnel]` LaunchAgents. Record
whether the assistant is running and the existing portal executable path. Do not
print `control.json`, credential files, plist environment secrets or log bodies.

Obtain the Access source in a **separate** temporary clone, then build both
binaries with default GUI features so the native application remains available:

```sh
set +x
export PTA_ACCESS_PROFILE_DIR="$HOME/Library/Application Support/dev.personalteams.assistant"
export PTA_ACCESS_RUNTIME_DIR="$PTA_ACCESS_PROFILE_DIR/web/runtime/access-build"
export PTA_ACCESS_SOURCE_DIR="$(mktemp -d /tmp/pta-access-source.XXXXXX)"
git clone --branch codex/cloudflare-access-rollout \
  https://github.com/elvisbrevi/personal-teams-assistant "$PTA_ACCESS_SOURCE_DIR"
git -C "$PTA_ACCESS_SOURCE_DIR" checkout --detach 5607d2bb936a9e292eccee387092f65c0a6fa984
git -C "$PTA_ACCESS_SOURCE_DIR" rev-parse HEAD
cargo install --path "$PTA_ACCESS_SOURCE_DIR" --locked --root "$PTA_ACCESS_RUNTIME_DIR"
"$PTA_ACCESS_RUNTIME_DIR/bin/pta" --version
"$PTA_ACCESS_RUNTIME_DIR/bin/pta" --json capabilities
"$PTA_ACCESS_RUNTIME_DIR/bin/pta" --json web status
```

The pinned [Access implementation commit](https://github.com/elvisbrevi/personal-teams-assistant/commit/5607d2bb936a9e292eccee387092f65c0a6fa984)
passed all project gates. Require `web_access_support: true`. Verify the source
commit against the handoff
before installation. The old 0.6.9 registry package has no Access support; no new
crate publication is implied by this change. Both binaries must come from the
same verified build. Compilation should finish before stopping a running service.

## Credentials, GitHub and Access

The actual Cloudflare token in the cloud session is
`005cf5b38ffa573d4edfe8aae87886a4`, for account
`26f1f3a05cbfe51ade90a57362c15fad`. Its reads work. Original October 9 application
and GitHub IdP creation attempts returned HTTP 403 `auth.forbidden`; on October 10
the user reported adding account permissions `Access: Apps and Policies Write`
and `Access: Identity Providers Write` (or the combined
`Access: Organizations, Identity Providers, and Groups Write`). Necessary writes
have not been retested since that update; use real configuration operations to
verify current authorization. Inspect the Mac's available protected credential,
as the cloud binding does not imply a local Mac binding. Never send token values
through chat or request token-policy management just to inspect permissions.

Check for an existing dedicated OAuth App in GitHub Developer Settings. If none
exists, create **`personal-teams-assistant-login`** as an OAuth App, separate from
the repository GitHub connection. The current Cloudflare team is
`small-forest-4923.cloudflareaccess.com`. Homepage:
`https://small-forest-4923.cloudflareaccess.com`; callback:
`https://small-forest-4923.cloudflareaccess.com/cdn-cgi/access/callback`.
Verify the team domain through the API before creating the OAuth App. GitHub App
Device Flow/repository credentials cannot replace these login credentials.

Supply the API token and dedicated OAuth client ID/secret through private
bindings or 0600 files using `CLOUDFLARE_API_TOKEN[_FILE]`,
`PTA_ACCESS_GITHUB_CLIENT_ID[_FILE]` and
`PTA_ACCESS_GITHUB_CLIENT_SECRET[_FILE]`. The helper inspects before creating,
refuses conflicting resources and reports the exact API operation/permission on
403. If the dedicated provider was already configured manually with this OAuth
App, the helper reuses it and its client secret need not be transferred to the
Mac. Finish the GitHub provider's authorization/Test in Cloudflare Zero Trust.

Export the **actual Mac accounts'** callbacks with the new CLI, while still only
reading legacy account data:

```sh
umask 077
"$PTA_ACCESS_RUNTIME_DIR/bin/pta" --json web callbacks \
  > "$PTA_ACCESS_RUNTIME_DIR/production-callbacks.json"
python3 "$PTA_ACCESS_SOURCE_DIR/scripts/configure-web-access.py" \
  --hostname assistant.elvisbrevi.cl --allow-email AUTHORIZED_GITHUB_EMAIL \
  --callbacks-file "$PTA_ACCESS_RUNTIME_DIR/production-callbacks.json" --dry-run
```

Use the intended users' explicit GitHub email allowlist; repeat `--allow-email`
for approved people. Edge admission is restricted to that list **and** the
dedicated GitHub provider; local bindings independently decide profile access.
Do not infer an email or bind by username similarity. The current-profile account
retains its existing separate desktop Graph callback hostname. Additional isolated
profiles need only their exported exact notification/lifecycle paths exempted.
The helper refuses wildcard, administrative and unrelated callback exceptions.

## Controlled cutover and account association

Before schema writes, privately back up `web/accounts.json`, `web/settings.json`,
the existing portal plist and installed sibling binaries. Preserve all other
profile files. New account fields cannot be read by the password-era host.
Stop the old portal using `launchctl bootout gui/UID/dev.personalteams.assistant.web`
(replace UID with `id -u`). Leave the dedicated web tunnel LaunchAgent in place.
If the desktop/profile host lacks `web_access_support`, quit it normally using its
installed `pta app quit` after recording its assistant's running state. This is a
controlled binary transition, not a profile migration. Do not signal a PID or
start a second assistant for the same Teams identity.

Install the already-built compatible GUI/host and CLI into the ordinary Cargo
location, retaining the staged pair for diagnosis:

```sh
cargo install --path "$PTA_ACCESS_SOURCE_DIR" --locked --force
pta --json capabilities
```

Prepare Access before restarting the portal. The helper creates/readbacks exact
Graph Bypass apps **before** protecting the enclosing panel hostname and API:

```sh
python3 "$PTA_ACCESS_SOURCE_DIR/scripts/configure-web-access.py" \
  --hostname assistant.elvisbrevi.cl --allow-email AUTHORIZED_GITHUB_EMAIL \
  --callbacks-file "$PTA_ACCESS_RUNTIME_DIR/production-callbacks.json" \
  --settings-file "$PTA_ACCESS_RUNTIME_DIR/access-settings.json"
pta web configure < "$PTA_ACCESS_RUNTIME_DIR/access-settings.json"
pta web users list
```

`access-settings.json` is private and uses Cloudflare's real audience and IdP ID.
Keep the listener `127.0.0.1:38656` and existing public URL. No new tunnel/DNS route
or connector token is needed. Reuse
`personal-teams-assistant-web` (`08cce5df-23a0-45e3-92f6-e65f2d8abe3e`), the proxied
CNAME and existing private `web/runtime/cloudflared-token`.

If `elvis` exists, require its current-profile association and keep its ID. If it
is absent, create it with `pta web users add elvis --current-profile`, without a
password. If another account already owns the current profile, inspect the
association instead of creating a duplicate. The account is blocked until bound.

Sign in through GitHub to
`https://assistant.elvisbrevi.cl/cdn-cgi/access/get-identity` and save the official
identity JSON locally in a 0600 file. The Cloudflare edge serves this identity
endpoint; it need not expose a password form at the origin. Verify the intended
GitHub person, account and dedicated IdP, then associate **locally**:

```sh
pta web users bind elvis < /private/path/verified-elvis-identity.json
```

The binding uses the provider subject `id`, not email or email-associated Access
`sub`. Unexpected/missing provider fields require investigation; never invent an
ID or relax validation. Binding changes preserve the old profile, Microsoft
session, Keychain service and encryption key. `users unbind/revoke/disable/enable`
provides local recovery and revocation without a public password backdoor.

Launch the compatible native GUI if the desktop previously owned its host. Read
`pta --json status` and restore Assistant Start only if it was running before the
transition. The portal can reuse only a host advertising `web_access_support`.

Inspect the existing portal plist. If it already invokes
`~/.cargo/bin/personal-teams-assistant --web`, preserve it. If it uses another
runtime location, back it up and update **only** its executable to the compatible
host, preserving label, logs and other settings. Do not rewrite the tunnel plist.
Bootstrap the existing portal plist and check both service states:

```sh
launchctl bootstrap "gui/$(id -u)" \
  "$HOME/Library/LaunchAgents/dev.personalteams.assistant.web.plist"
launchctl print "gui/$(id -u)/dev.personalteams.assistant.web"
launchctl print "gui/$(id -u)/dev.personalteams.assistant.web.tunnel"
pta --json web status
pta --json status
```

If the web connector is stopped, start its **existing** LaunchAgent with the
existing dedicated token file. Confirm the tunnel becomes healthy with active
connections in Cloudflare. Do not reuse the desktop connector credential. A
sleeping/offline Mac cannot be recovered by changing DNS.

## Verify HTTPS and preserve evidence

From the Mac, with TLS verification enabled and a browser User-Agent:

```sh
curl --silent --show-error --dump-header - --output /dev/null \
  --user-agent 'Mozilla/5.0' https://assistant.elvisbrevi.cl/login
```

An unauthenticated request should redirect to Access/GitHub, not return the old
password form. Browser Integrity Check can reject non-browser User-Agents with
1010; keep it enabled. Complete these checks from the Mac and an actual phone:

- GitHub sign-in opens the existing `elvis` settings/history with no second form.
- A second explicitly approved/bound synthetic account has independent settings,
  credential files and history. No production credentials are copied to it.
- Unknown identities, disabled/unbound accounts, invalid signatures/audiences/
  issuers, expired tokens and cookie-only/identity-header-only requests are blocked.
- All writes, including read commands over POST, require exact Origin and CSRF.
- Sign out clears the local cookie, blocks assertion replay immediately and visits
  Access logout. Fresh login succeeds after Cloudflare revocation propagation.
  Local/Access expiration and `pta web users revoke` prevent old assertion reuse.
- Exact Graph validation/invalid-clientState checks still work without interactive
  Access; unrelated webhook paths stay protected. Use synthetic notifications,
  never real Teams messages. Existing Graph scopes/credentials are unchanged.
- Tauri opens normally and ordinary CLI status/config reads still use private IPC.
  The desktop service/tunnel and previous assistant state are restored.
- The local session cookie is `__Host-pta-session`, Secure, HttpOnly, SameSite=Strict,
  with lifetime no longer than the Access assertion. CSP, nosniff, no-referrer and
  no-store headers remain present.

Do not label the migration deployed or verified before these live checks pass.
Record exact results and unresolved steps in `docs/handoff-web-cloudflare.md`.

For rollback, stop only the new portal and transition the current host normally,
restore the backed-up **web account/settings JSON and binary pair** together,
then restore the prior assistant state. An old binary cannot read the new web
schema. Never delete/copy profile data or change `STATE_ENCRYPTION_KEY`. Access
rollback must preserve the exact Graph exceptions; inspect the actual Access
policies before any removal, as another application must never be altered.
