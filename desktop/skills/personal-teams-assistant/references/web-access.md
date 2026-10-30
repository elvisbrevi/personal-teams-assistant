# Your private web installation

The same host binary serves the existing GUI in a browser: Home, Messages,
Activities, Knowledge, Test chat and Settings. Native Tauri and the CLI keep their
private loopback IPC. The portal is `personal-teams-assistant --web`, behind the
existing dedicated Tunnel. Its HTTPS hostname and complete API must be protected
by Cloudflare Access. GitHub owns the interactive sign-in; there is no second
password form, public registration or password authentication endpoint.

Installing the public crate never grants access to a maintainer's server. Each portal deployment supplies its own computer/VPS, HTTPS hostname, Cloudflare account/tunnel/Access application and dedicated GitHub OAuth app. Admitted accounts supply their own Microsoft application/session and provider credentials. No production URL or login identity is a runtime default. Owner-only access remains the default. Additional accounts need explicit local operator authorization and their own private configuration; installing the crate never registers them.

Configure `owner_github_id` to your verified positive integer GitHub user ID. Existing portals with exactly one locally associated identity infer that owner for compatibility; ambiguous legacy associations cannot start until an owner is explicitly configured. Legacy non-owner accounts remain on disk but cannot authenticate, receive callbacks or start a portal-owned host until the operator explicitly authorizes them.

## Operator-local accounts and recovery

```sh
pta web status
pta web users list
pta web users add owner --current-profile
pta web users bind owner < /private/path/verified-owner-identity.json
pta web users add colleague
pta web users authorize colleague
pta web users bind colleague < /private/path/verified-colleague-identity.json
pta web users enable colleague
pta web users unbind owner
pta web users revoke owner
pta web users disable owner
pta web users enable owner
pta --json web callbacks > /private/path/production-callbacks.json
```

Create `owner --current-profile` only if it is absent. An existing `owner` account
must keep its ID and current profile association. `add` takes no password and does
not grant access until `bind`. Additional accounts also require `authorize`, which is
operator-local and unavailable in the GUI/MCP account tools. `enable` never implies
admission; `--current-profile` can bind only the configured owner. No username/email
similarity associates accounts.

After authorizing the **dedicated login OAuth App**, obtain the identity response
from `https://assistant.example.com/cdn-cgi/access/get-identity` in the signed-in
browser. Save that response locally in a private file. The local operator checks
that it is the intended person's identity from the configured GitHub provider,
then supplies it to `users bind USER`. The response must include `id` (provider
subject), `idp.id`, `idp.type` (`github`), `account_id` and `user_uuid`. GitHub
returns `id` as a positive JSON integer; the CLI and runtime normalize it
losslessly to the same decimal string used for the local binding. Legacy string
subjects remain supported. Supply the official response unchanged; do not edit
its subject or convert it through floating-point JavaScript numbers. Never paste
Access cookies, JWTs or OAuth secrets into chat, command arguments or documentation.
Missing or unsupported provider subjects fail closed; do not substitute an email
or Access `sub`. The runtime repeats the identity lookup using the verified JWT;
a user-supplied identity header/file never authenticates an HTTP request.

The stored binding is issuer + GitHub IdP ID + provider subject. The Access `sub`
is email-associated, so it is checked against `user_uuid` and bound to each local
session, but it cannot select a profile. Bindings cannot belong to two accounts.
Local unbind/rebind is the recovery mechanism; it preserves files and credentials.
`revoke` invalidates local sessions and requires an Access assertion issued after
the local cutoff; log out of Access and sign in again. Disabling an account blocks
its next browser request and stops its portal-owned host on reconciliation.
Re-enabling, unbinding or reassociating changes the account version and revokes
its existing local sessions.

An ordinary account owns `web/profiles/<existing-opaque-id>/`, with independent
settings, knowledge, databases, file credentials and provider home. Exactly one
`--current-profile` account reuses the existing desktop host/data/Keychain without
a Microsoft re-login. The portal rejects duplicate Teams identities across
profiles. New profiles start stopped/in observation mode with sources disabled,
no audiences/external processing and DeepSeek as the default provider.

Legacy account/settings JSON remains readable without deleting passwords,
profiles or credentials. Retired hashes remain inert. Hosts must advertise
`shared_accounts_support` (0.9.11+) before the new account schema is written or an additional account host is reused; transition an older host first. No portal starts without Access
configuration. Native local recovery does not expose an HTTP password backdoor.

## Cloudflare setup

Preserve tunnel `personal-teams-assistant-web`, DNS, the desktop Teams tunnel and
the static landing. The portal ingress remains `http://127.0.0.1:38656`, with the
public Host header preserved and an unrelated-path 404 catch-all.

Use a dedicated GitHub **OAuth App** named `personal-teams-assistant-login`,
separate from the GitHub App/Device Flow used for repositories. Cloudflare's
[GitHub setup guide](https://developers.cloudflare.com/cloudflare-one/integrations/identity-providers/github/)
requires the team origin as homepage and this callback:

```text
https://YOUR-TEAM.cloudflareaccess.com/cdn-cgi/access/callback
```

Verify the team domain again before creating/reusing the OAuth App. GitHub OAuth
App creation/secret generation happens in GitHub Developer Settings; the repository
connection cannot supply dedicated login credentials. Finish the provider's GitHub
authorization/Test flow in Cloudflare before declaring login operational.

The helper `scripts/configure-web-access.py` inspects existing resources, refuses
to overwrite overlapping/unmanaged apps, uses only the dedicated GitHub IdP,
limits edge admission to your one explicit owner email plus that login method,
and reads back the apps/policies before writing private portal settings. The
allowlist is an edge restriction only; a local provider-subject binding is still
required. Never use an everyone or whole-domain Allow policy.

Required **account** permissions:

- `Access: Apps and Policies Write`: panel app, Allow policy and exact Graph and icon bypass
  applications. Read permission alone cannot configure them.
- `Access: Identity Providers Write`, or `Access: Organizations, Identity Providers,
  and Groups Write`: dedicated GitHub IdP. Existing read access to the organization
  is also needed to discover the team domain.

Supply `CLOUDFLARE_API_TOKEN[_FILE]` and dedicated
`PTA_ACCESS_GITHUB_CLIENT_ID[_FILE]`/`PTA_ACCESS_GITHUB_CLIENT_SECRET[_FILE]` through
protected environment bindings or 0600 files. The helper never prints secrets.
Do not reuse `GITHUB_OAUTH_TOKENS` or a Tunnel connector token.

```sh
python3 scripts/configure-web-access.py \
  --hostname assistant.example.com --owner-github-id YOUR_NUMERIC_GITHUB_ID \
  --allow-email YOUR_VERIFIED_GITHUB_EMAIL \
  --callbacks-file /private/path/production-callbacks.json --dry-run
python3 scripts/configure-web-access.py \
  --hostname assistant.example.com --owner-github-id YOUR_NUMERIC_GITHUB_ID \
  --allow-email YOUR_VERIFIED_GITHUB_EMAIL \
  --callbacks-file /private/path/production-callbacks.json \
  --settings-file /private/path/access-settings.json
pta web configure < /private/path/access-settings.json
```

Run this helper from a checkout of the project's `main` branch; Cargo installs
the binaries and embedded skill, while the setup script stays in the repository.

The real helper-generated settings contain `bind`, `public_url`, `session_hours`,
`owner_github_id` and `access` (`issuer`, `audience`, `account_id`, `github_idp_id`). Audience/provider
IDs come from actual Cloudflare resources; never invent them. Configuration takes
effect when the portal restarts. Plain HTTP previews cannot authenticate Access.

`pta web callbacks` exports only exact `/webhooks/<profile-id>/graph/notifications`
and `/webhooks/<profile-id>/graph/lifecycle` paths for enabled, bound, admitted accounts only. For a fresh isolated owner, the first
export is empty while Access/binding are being configured. After binding the owner,
export `pta --json web callbacks` again to the callbacks file and rerun the Access
helper with that refreshed file before starting Teams. Repeat after any owner-profile
change; an old callback export never updates itself. Only these callback POST handlers are exempt in the backend;
the exact matching paths have separate Access Bypass apps at the edge. No broad
`/webhooks/*` bypass is accepted. The static icons and PWA manifest (`/favicon.png`,
`/manifest.webmanifest`, `/apple-touch-icon.png`, `/icon-192.png`, `/icon-512.png`)
are the only other exception: WebKit fetches a page's icons without cookies, so an
iPhone would otherwise get the login page as its home-screen icon. The helper
creates an exact Bypass app for each and the backend serves only their GETs
without Access. Disabled/unknown profiles and additional paths
cannot start hosts or administer the panel. Graph subscription, clientState,
tenant/resource and validation-token handling stay unchanged. The current
profile retains its separate existing Teams callback hostname/tunnel.

## Sessions, logout and remote operations

The first authenticated `/login` response serves the shared panel document. This
finishes the cross-site GitHub/Access navigation before asset/API requests need
the Strict cookie. An existing first-party session may redirect to `/`; the
initial login never relies on that redirect or relaxes SameSite protection.

Access assertions are verified against the official team HTTPS public keys for
signature, issuer, audience, not-before and expiration. Each session is bound to
the exact verified assertion, provider identity, Access subject and local account
version. Its lifetime/cookie is capped by JWT expiration. Every browser POST
requires the exact configured Origin and the session's CSRF header. Native IPC
continues to reject Origin and browser cookies.

Sign out revokes the local session and persists the Access assertion fingerprint
until expiration, then navigates to `/cdn-cgi/access/logout`, clearing the Access
application cookie and revoking Access sessions. Cloudflare documents propagation
of global revocation in 20–30 seconds; local assertion replay is blocked immediately.
A stolen cookie alone, invalid JWT or identity/email-only header cannot authenticate.

Microsoft connection keeps its public-client PKCE/scopes and loopback callback.
On a phone, authorize the returned Microsoft link, copy the final
`http://localhost:PORT/?code=…&state=…` address and paste it into **Final redirect
address**. Repository GitHub still uses its separate Device Flow. Browser activity
navigation stays in the tab. Hosting/data paths remain operator-managed, and
repositories/imports remain confined to each profile.

For isolated operator-local CLI access use the path in `web users list`:

```sh
PTA_PROFILE_DIR=/absolute/profile/path pta status
PTA_PROFILE_DIR=/absolute/profile/path pta credentials set DEEPSEEK_API_KEY < /private/path/key
```

Without `PTA_PROFILE_DIR`, the existing `dev.personalteams.assistant` profile,
Keychain service, encryption key and Microsoft session remain selected. Isolated
hosts inherit network/CA settings, but never the operator's application secrets
or provider logins. The portal remembers Assistant Start/Stop state and stops only
hosts it owns on exit; a running desktop host remains owned by the desktop.

Verify GitHub sign-in over HTTPS from the Mac and phone with no password form,
owner and admitted second-account access, blocked unauthorized identities and
disabled identities, JWT rejection, logout/replay/expiration, exact Graph callbacks,
and native Tauri/CLI behavior. Use synthetic data and existing status reads; never
send real Teams messages as a smoke test.

## Add a computer you own by signing in (0.9.12+)

This is the normal path for a new desktop, CLI or headless installation. Both the
installation's CLI/host and the coordinator must advertise `machine_login_support: true`.

1. On the computer to add, stop its Teams assistant. The app shows its sign-in screen at
   start (or choose Sign in in the sidebar): enter your coordinator's HTTPS origin and select
   Sign in, or run `pta auth machine login https://your-portal.example` (add `--no-browser` on a
   headless host). Through MCP/CLI tools call `machine_login` with `url` (and `open: true`
   on a desktop host). The origin is validated (HTTPS, no credentials, path or query) and
   remembered for the next sign-in; there is no default coordinator.
2. The host opens the system browser or returns a public approval URL
   (`https://your-portal.example/login?enroll=…`) and a confirmation code such as
   `1A2B-3C4D`. Nothing else leaves the host: its verifier never reaches GUI, CLI or MCP.
3. Open the URL on any device, sign in with your GitHub identity through Access, check that
   the page names this computer and shows the same code, then choose Trust this machine (or
   Deny). This one confirmation is the only approval; signing in alone registers nothing.
4. On the computer, select nothing more in the GUI (it finishes by itself and brings the
   account's configuration and variables), or run `pta auth machine finish --wait`
   (which then runs `pta account sync`) / call `machine_login_finish` until it returns
   `connected`, then `account_sync`. `pta auth machine status` (`machine_login_status`) reports `pending`,
   `approved`, `denied`, `expired` or `none` without finishing. `pta auth machine cancel`
   (`machine_login_cancel`) withdraws a pending request.
5. Confirm `machine_list` shows the computer. Then choose its allowed roots, and separately
   the Teams machine and Start, if wanted: trust grants no directories, Teams ownership,
   assistant start or provider login.

Security properties: each request expires after ten minutes, is single use and bound to the
installation's machine ID, its private installation key (sent only as a hash) and the verifier
it holds. Approval is bound to the signed-in account and its version; disable, revoke, unbind or
rebind before finishing issues nothing. A computer already connected to an account can only be
re-approved by that account; another account's approval is refused and the existing
connection stays. A different installation cannot claim an enrolled machine ID; repeating the
sign-in from the same installation rotates its own credential without duplicate entries.
Denied, expired, cancelled or mismatched sign-ins store nothing. Browser cookies and Access
assertions never reach the installation, GitHub tokens are never accepted, and the credential
is written only to the installation's own credential store. The unauthenticated start endpoint
is rate limited and holds at most 100 pending requests.

Recovery: start again after a denial or expiry. If an installation was lost or its credential
was not stored, signing in again from the same installation replaces its own entry. If a
different installation reports that the machine ID belongs to another installation, disconnect
it on that computer (`machine_disconnect`) or use an operator pairing file.

## Authorize a computer with a pairing file

The operator-local pairing file remains available, for example for coordinators that do not
offer browser sign-in.

1. On the computer to connect, call `machine_settings` through CLI/MCP and read its `id`. Stop its Teams assistant before pairing.
2. On your own coordinator, run `pta web machines authorize MACHINE_ID [USER] --output /private/unused/pairing.json`. This operator-local command requires an enabled admitted account. Omit USER for the owner, or supply the explicitly authorized account's local username. It writes a 0600 JSON credential file; command output contains only its path and expiration. The server stores only a hash. Never paste the file or token in chat or command arguments.
3. Transfer that file securely to the intended computer. Run `pta tools call machine_connect < /private/path/pairing.json` there within ten minutes. Delete the file after use. It carries your coordinator URL; there is no default shared server.
4. Confirm `machine_list` shows your computer. A pairing cannot be reused, used for a different machine or installation, or used after account revocation/reassociation. A failed registration needs a new local authorization; never retry an uncertain enrollment with the old file.

Machine registration never sends a GitHub access token to a coordinator. Existing machine credentials remain account-version-bound and revocable. Initial pairing requires CLI 0.9.4 or later. The GUI's Connect action uses browser sign-in, not pairing files. Normal management remains available in the GUI after pairing.

HTTPS origin responses include hostname-only HSTS. Configure matching edge HSTS for Access-generated responses without enabling parent-domain includeSubDomains or preload indiscriminately.

Both CLI and host must advertise `machine_pairing_support: true` (0.9.4 or later). The portal refuses to reuse an older native/headless host, and IPC rejects machine connection before forwarding any request to one. This prevents an older host from falling back to GitHub-token enrollment. Browser sign-in likewise requires `machine_login_support: true` (0.9.12+) on the CLI and host before any sign-in operation is sent, and the portal never forwards a sign-in registration to an account host without it. An older coordinator answers the start request as unsupported; nothing falls back to GitHub-token enrollment.

## Account and execution authority

Version 0.9.10 adds mandatory `account_authority_support` to CLI capabilities and
host descriptors. Update the host and CLI together before operating through the
new catalog. Older hosts are refused before forwarding; never work around this
by selecting another profile or using an operator credential. The existing matched
local-installation update may inspect/stop an older owner host solely to retire it
and preserve its running state; this is not a portal operation or security downgrade.

The host derives account/profile ownership from its private registry and verifies
the binding again on each call. Browser and machine invocations use distinct
scoped IPC credentials and cannot grant operator maintenance authority. Do not
supply account, owner, transport or maintenance flags as tool arguments. After an
account reassociation, restart its host before continuing. The existing owner,
profile identifiers, Microsoft session and safe single-use pairing remain intact.

Version 0.9.11 admits explicitly authorized additional accounts. Their GUI, CLI and MCP
can use their own API assistant configuration, source/notes/index data, records,
background daily snapshots and Microsoft/GitHub API sessions. Their provider catalog
never probes installed CLIs. Active CLI model chains, local coding, CLI sign-in,
Git clone/sync and request/child creation on the shared server are refused before spawn;
these unsupported operations do not fall back to the operator's installed CLI. A stored
CLI chain must be changed to an API chain before assistant operations can run.

Ordinary accounts resolve credentials only from their private profile file store,
ignoring inherited NAME/NAME_FILE values and OS keychains. Hosting, data/map paths and
repository roots are operator-managed, including through native CLI/MCP. Disable,
revoke, unbind and rebind invalidate sessions and machine credentials on the next checked
request. Reconciliation stops only the affected app-managed host; an existing desktop
host is not killed. Rebinding requires a new host, preserves the account's files and
must be treated as an explicit operator transfer of that private account.

An ordinary account cannot run configurable providers, coding or generic variable
consumers in the operator's OS context.
Use a personal trusted machine; selecting a working directory or removing tools
from a prompt does not provide OS isolation. A paired machine cannot forward a
coding/provider/directory operation into the coordinator's local context. Existing
account-qualified polling, session summaries, results, leases and remote execution
on personal machines retain their checks.

Different people sharing one computer need separate OS security contexts. App
account switching cannot protect against someone who controls the same OS account,
including its private IPC descriptors and credential store. Portal administration
is a local operator operation, unavailable from an isolated account profile.

Ordinary shared accounts also refuse SQL sources that could use OS-integrated authentication. Their embeddings requests reuse the public-source DNS/IP pinning and reject operator-local destinations; API keys still resolve only from that account.
