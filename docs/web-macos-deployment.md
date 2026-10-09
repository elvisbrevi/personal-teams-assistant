# Deploying the web portal on the existing Mac

The selected origin is the user's Mac. The initial account is `elvis`, attached
to the existing desktop profile through `--current-profile`. Keep the Mac awake
and connected for remote access. LaunchAgents run while the user is logged in;
they do not make a sleeping or powered-off Mac reachable.

These commands are for a terminal on that Mac, not the cloud development
workspace. No installation on the Mac has been performed by the cloud session.
Use the ordinary macOS account that already owns the application and its profile.
Run without a `PTA_PROFILE_DIR` override so the existing profile is selected.
Keep any current desktop checkout and its uncommitted work intact.

## Install the web-capable binaries

The published crate may not contain the web portal yet. Install both headless
binaries from the verified implementation commit into a dedicated directory,
leaving the existing GUI installation in place:

```sh
set +x
export PTA_WEB_PROFILE_DIR="$HOME/Library/Application Support/dev.personalteams.assistant"
export PTA_WEB_RUNTIME_DIR="$PTA_WEB_PROFILE_DIR/web/runtime"
mkdir -p "$PTA_WEB_RUNTIME_DIR" "$PTA_WEB_PROFILE_DIR/web/logs"
chmod 700 "$PTA_WEB_PROFILE_DIR/web" "$PTA_WEB_RUNTIME_DIR" "$PTA_WEB_PROFILE_DIR/web/logs"
cargo install --git https://github.com/elvisbrevi/personal-teams-assistant \
  --rev 926007b8e67ff543441915b3687a40e3621837d0 \
  --locked --no-default-features --root "$PTA_WEB_RUNTIME_DIR" \
  personal-teams-assistant
"$PTA_WEB_RUNTIME_DIR/bin/pta" --version
"$PTA_WEB_RUNTIME_DIR/bin/pta" --json web status
```

The two new binaries share their build. The portal can reuse an already running
compatible desktop host, and starts a headless host only when needed. This install
does not start the Teams service or replace the Microsoft session or encryption key.

Before starting the portal, inspect the existing host's capabilities without
printing its private IPC token:

```sh
python3 - <<'PY'
import json, os, pathlib
descriptor = pathlib.Path(os.environ['PTA_WEB_PROFILE_DIR']) / 'control.json'
if descriptor.exists():
    data = json.loads(descriptor.read_text())
    print({'web_support': data.get('web_support', False),
           'activity_registration_support': data.get('activity_registration_support', False)})
else:
    print('No existing host descriptor.')
PY
```

If an older desktop host is running with either capability absent, record its
assistant's running state with the installed `pta status`, then quit that older
GUI normally before starting the portal. The new portal can then start its own
compatible headless host against the same profile. Do not delete or reimport any
profile files. Restore Assistant Start in the web panel only if it was running
before the transition. Reopening an older GUI can claim the profile first again;
use the compatible host for the web session.

## Configure the dedicated Cloudflare route

Cloudflare setup has already succeeded from the cloud session. The remotely
managed tunnel is `personal-teams-assistant-web`, ID
`08cce5df-23a0-45e3-92f6-e65f2d8abe3e`. Its proxied CNAME and routes are verified;
it has not been connected to the Mac yet. The steps below reuse that same tunnel
and obtain its dedicated connector credential privately on the Mac. Do not copy
the cloud workspace's development profile or its existing desktop tunnel token.

The API token needs Zone Read and DNS Edit for `elvisbrevi.cl`, and Cloudflare
Tunnel Edit for that zone's account. For current permission names, see the
[Cloudflare tunnel creation API](https://developers.cloudflare.com/api/resources/zero_trust/subresources/tunnels/subresources/cloudflared/methods/create/).
Do not use the existing desktop connector token.

Fetch the setup helper from the same verified commit:

```sh
curl --fail --location \
  https://raw.githubusercontent.com/elvisbrevi/personal-teams-assistant/926007b8e67ff543441915b3687a40e3621837d0/scripts/configure-web-cloudflare.py \
  --output "$PTA_WEB_RUNTIME_DIR/configure-web-cloudflare.py"
```

The following wrapper uses an existing protected API-token file or environment
binding when present, otherwise prompts privately in the terminal. Its value
never goes in chat, arguments, repository files or output. The helper safely
reuses the dedicated route if it was already created from the cloud session.

```sh
python3 - <<'PY'
import getpass, os, pathlib, subprocess
runtime = pathlib.Path(os.environ['PTA_WEB_RUNTIME_DIR'])
env = os.environ.copy()
if not env.get('CLOUDFLARE_API_TOKEN_FILE') and not env.get('CLOUDFLARE_API_TOKEN'):
    env['CLOUDFLARE_API_TOKEN'] = getpass.getpass('Cloudflare API token: ')
subprocess.run([
    'python3', str(runtime / 'configure-web-cloudflare.py'),
    '--domain', 'elvisbrevi.cl', '--hostname', 'assistant.elvisbrevi.cl',
    '--token-file', str(runtime / 'cloudflared-token'),
], env=env, check=True)
PY
```

The dedicated connector credential is stored in a 0600 file. No existing tunnel,
unrelated DNS record or landing is overwritten. If the API returns 403, correct
the token's write permissions and account scope before continuing.

## Configure the portal and create the owner account

```sh
"$PTA_WEB_RUNTIME_DIR/bin/pta" web configure <<'JSON'
{"bind":"127.0.0.1:38656","public_url":"https://assistant.elvisbrevi.cl","session_hours":12}
JSON
"$PTA_WEB_RUNTIME_DIR/bin/pta" web users list
```

If `elvis` already exists, inspect its profile choice; do not overwrite it. For
a new account, choose a unique password of 12–256 characters through this private
terminal prompt. No password is printed or stored in plaintext:

```sh
python3 - <<'PY'
import getpass, os, pathlib, subprocess
password = getpass.getpass('New password for elvis: ')
if password != getpass.getpass('Confirm password: '):
    raise SystemExit('Passwords do not match.')
pta = pathlib.Path(os.environ['PTA_WEB_RUNTIME_DIR']) / 'bin' / 'pta'
subprocess.run([str(pta), 'web', 'users', 'add', 'elvis', '--current-profile'],
               input=password + '\n', text=True, check=True)
PY
```

## Prepare the two dedicated LaunchAgents

Use the Mac's existing `cloudflared` installation. Check `command -v cloudflared`
and `cloudflared tunnel run --help` for `--token-file`. If it is absent and the
Mac uses Homebrew, install it with `brew install cloudflared`.

The generator below checks that both executables and the private connector file
exist before writing plists. It refuses to overwrite an existing service file.
It does not include credential values or reuse the desktop tunnel's service.

```sh
python3 - <<'PY'
import os, pathlib, plistlib, shutil
profile = pathlib.Path(os.environ['PTA_WEB_PROFILE_DIR'])
runtime = pathlib.Path(os.environ['PTA_WEB_RUNTIME_DIR'])
portal = runtime / 'bin' / 'personal-teams-assistant'
token = runtime / 'cloudflared-token'
cloudflared = shutil.which('cloudflared')
if not portal.is_file() or not os.access(portal, os.X_OK) or not cloudflared or not token.is_file():
    raise SystemExit('Install the portal, cloudflared and dedicated connector token first.')
if token.stat().st_mode & 0o077:
    raise SystemExit('The connector token must be private: chmod 600 its file.')
agents = pathlib.Path.home() / 'Library' / 'LaunchAgents'
agents.mkdir(parents=True, exist_ok=True)
logs = profile / 'web' / 'logs'
logs.mkdir(parents=True, exist_ok=True, mode=0o700)
services = [
    ('dev.personalteams.assistant.web', [str(portal), '--web']),
    ('dev.personalteams.assistant.web.tunnel',
     [cloudflared, '--no-autoupdate', 'tunnel', 'run', '--token-file', str(token)]),
]
if any((agents / (label + '.plist')).exists() for label, _ in services):
    raise SystemExit('A dedicated web service plist already exists; inspect it before replacing it.')
for label, argv in services:
    definition = {
        'Label': label, 'ProgramArguments': argv,
        'RunAtLoad': True, 'KeepAlive': True, 'ThrottleInterval': 10,
        'ProcessType': 'Background',
        'StandardOutPath': str(logs / (label + '.out.log')),
        'StandardErrorPath': str(logs / (label + '.err.log')),
    }
    path = agents / (label + '.plist')
    with path.open('xb') as output:
        plistlib.dump(definition, output)
    path.chmod(0o600)
    print(path)
PY
```

Start only these new services:

```sh
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/dev.personalteams.assistant.web.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/dev.personalteams.assistant.web.tunnel.plist"
launchctl print "gui/$(id -u)/dev.personalteams.assistant.web"
launchctl print "gui/$(id -u)/dev.personalteams.assistant.web.tunnel"
```

## Verify from the Mac and a phone

```sh
curl --fail --silent --show-error --dump-header - \
  --output /dev/null https://assistant.elvisbrevi.cl/login
```

Keep TLS verification enabled. Require HTTP 200, CSP, `X-Content-Type-Options:
nosniff`, `Referrer-Policy: no-referrer` and `Cache-Control: no-store`. Confirm
the dedicated tunnel has an active connector in Cloudflare. A DNS record alone
does not establish publication.

Open `https://assistant.elvisbrevi.cl/login` on the phone, sign in as `elvis`, and
check that the existing profile's settings and history are available. Verify
logout and, in browser developer tools, that the `__Host-pta-session` cookie is
Secure, HttpOnly and SameSite=Strict. Do not send real Teams messages as a smoke
test. Additional isolated users are created locally with ordinary `web users add`
without `--current-profile`.

To stop the two web services while preserving the desktop application's own
service, use `launchctl bootout gui/UID/dev.personalteams.assistant.web.tunnel`
and `launchctl bootout gui/UID/dev.personalteams.assistant.web`, replacing UID
with the result of `id -u`. Keep the private token and account data out of Git.
