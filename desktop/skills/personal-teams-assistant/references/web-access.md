# Web access and independent users

The same `personal-teams-assistant` binary can serve the existing GUI in a browser:
Home, Settings, Messages, Activities (pending decisions and daily history), Knowledge
and Test chat. The CLI and native Tauri GUI keep using their existing private IPC.

`personal-teams-assistant --web` is the portal entry point. It stays in the foreground
and owns a private `web/portal.lock` under the existing profile. It runs with or
without the Cargo `gui` feature. This is not the static landing in `site/`.

## Accounts

The operator creates accounts locally; there is no public registration. Passwords
are supplied on protected stdin, have 12–256 characters, and are stored as salted
Argon2id hashes, never plaintext. Do not put them in arguments, TOML or logs.

```sh
pta web users add alice < /private/path/alice-password
pta web users add elvis --current-profile < /private/path/elvis-password
pta web users list
pta web users password alice < /private/path/new-password
pta web users disable alice
pta web users enable alice
pta web status
```

An ordinary account gets a new profile under `<profile>/web/profiles/<random-id>/`
with its own settings, knowledge map, credentials, provider home and SQLite data.
The account connects its own Teams identity. New profiles start stopped and in
observation mode, with DeepSeek as their default provider. Sources start disabled,
without audiences or external processing, as in the desktop.

Exactly one account may use `--current-profile`. It controls the existing desktop
profile and reuses its live GUI/headless host, credentials and history. It does not
copy or migrate the Microsoft session. If that host is stopped, the portal starts
it headless. Do not create a second profile for the same Teams account: the portal
and host reject duplicate tenant/user identities, including the desktop profile.

Passwords can also be changed in web Settings. Password changes, disabling and
re-enabling an account revoke its old browser sessions. Signing out revokes that
session. Sessions expire (12 hours by default), use HttpOnly, SameSite=Strict
cookies, and on HTTPS a host-only Secure cookie. Browser writes require an exact
Origin and a session-specific CSRF header. The private CLI IPC still rejects Origin.

## Local preview

```sh
personal-teams-assistant --web
# Open http://localhost:38656/login on this computer.
```

The default URL is only for local preview. A phone uses the public HTTPS URL.
The portal binds to loopback; Cloudflare Tunnel or a trusted TLS reverse proxy
reaches it. Configure it with JSON on stdin, then restart the portal:

```sh
pta web configure <<'JSON'
{"bind":"127.0.0.1:38656","public_url":"https://assistant.elvisbrevi.cl","session_hours":12}
JSON
```

## Cloudflare

Use a dedicated **remotely managed Tunnel** for the portal, with this route:

| Public hostname | Origin |
| --- | --- |
| `assistant.elvisbrevi.cl` | `http://127.0.0.1:38656` |
| Catch-all | `http_status:404` |

Preserve the public Host header (or set `httpHostHeader` to the hostname). Keep the
landing `teams-assistant.elvisbrevi.cl` and the desktop's existing Tunnel unchanged.
The one portal origin forwards `/webhooks/<profile-id>/graph/notifications` and
`/webhooks/<profile-id>/graph/lifecycle` to the matching private host. Each host
still verifies its own Graph subscription, tenant, resource and clientState.
Unsigned callbacks cannot start a profile or administer it.

The repository provides an idempotent operator helper:

```sh
python3 scripts/configure-web-cloudflare.py \
  --domain elvisbrevi.cl --hostname assistant.elvisbrevi.cl \
  --token-file /private/path/pta-web-cloudflare-token
```

It requires `CLOUDFLARE_API_TOKEN` or `CLOUDFLARE_API_TOKEN_FILE`, with Zone Read,
DNS Edit on the domain and Cloudflare Tunnel Edit on its account. A connector's
`CLOUDFLARE_TUNNEL_TOKEN` cannot create DNS records. The helper creates/reuses
`personal-teams-assistant-web`, configures only its dedicated routes and writes
the connector token to a 0600 file. It refuses to overwrite unrelated DNS records
or Tunnel routes and does not touch the landing. It does not claim that the origin
is reachable: start the connector and check `/login` over HTTPS afterward.

Run the portal and the connector as services on the always-on machine:

```sh
personal-teams-assistant --web
# Separate foreground service, token contents never in argv:
TUNNEL_TOKEN_FILE=/private/path/pta-web-cloudflare-token cloudflared tunnel run
```

Cloudflare Access is an optional additional protection. If configured, protect
the panel and `/api/*`, but bypass Access on the exact `/webhooks/*` callback path:
Microsoft Graph cannot complete an interactive Access login. The application login
still selects the private user profile. These are application profiles for people
authorized by the same server operator, not separate OS accounts or containers.

## Remote operations

Sign in to the web account first, then connect Microsoft in Settings. The existing
public-client PKCE flow and Graph scopes are unchanged. On a phone, open the returned
Microsoft sign-in link, copy the final `http://localhost:PORT/?code=…&state=…` address
that the device cannot open, and paste it in the portal's **Final redirect address**.
The host forwards it only to its own pending loopback callback. The form clears the
address and sign-in URL afterward. GitHub's device code is authorized on the phone.

Activities open in the browser's Activities tab; native windows, the tray and OS
notifications stay on the desktop. Web polls show activity and status updates.
The server operator manages public URL/listener/tunnel and data paths, which the
web form shows as read-only. Repository paths are server paths inside the profile's
`data/repositories/`, with canonical path and symlink checks; the current-profile
account may also keep the repositories already in its desktop map. Imports must
stay inside that profile and preserve its data, map and hosting paths. No browser
request may select another account's IPC endpoint, files or credentials.

For operator-local CLI access to an isolated account, use the profile path shown by
`pta web users list` (use ordinary `pta` for the current-profile account):

```sh
PTA_PROFILE_DIR=/absolute/profile/path pta status
PTA_PROFILE_DIR=/absolute/profile/path pta credentials set DEEPSEEK_API_KEY < /private/path/key
```

`PTA_PROFILE_DIR` is an explicit isolated-profile override; omitting it keeps the
existing `dev.personalteams.assistant` profile, data directory, Keychain service
and Microsoft session. Isolated hosts use private credential files on all OSes
and do not inherit the operator's app credentials or provider logins.

The portal starts a host for each enabled account so activity schedules do not
depend on a browser being open. Assistant Start/Stop state is persisted separately
and restored when the portal restarts. On shutdown it stops only hosts it started;
an already running desktop host remains owned by the desktop. Disabling an account
stops its portal-owned host within the next reconciliation cycle and keeps its data.

Verification: check `/login` over the configured HTTPS hostname, then authenticate
two accounts and verify independent settings, credentials and history. Local tests
use synthetic profiles and mocked Graph/IPC, never real accounts or Teams sends.
