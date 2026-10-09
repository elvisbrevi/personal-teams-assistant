# Web access with Cloudflare Access and GitHub

The same host binary serves the existing GUI in a browser: Home, Settings,
Messages, Activities, Knowledge and Test chat. Native Tauri and the CLI keep their
private loopback IPC. The portal is `personal-teams-assistant --web`, behind the
existing dedicated Tunnel. Its HTTPS hostname and complete API must be protected
by Cloudflare Access. GitHub owns the interactive sign-in; there is no second
password form, public registration or password authentication endpoint.

Read [the Mac deployment guide](../../../../docs/web-macos-deployment.md) for the
production transition. A registry install of 0.6.9 predating the Access change
still has the password portal. Install the verified Access source build, including
the compatible host/CLI pair, before associating identities.

## Operator-local accounts and recovery

```sh
pta web status
pta web users list
pta web users add alice
pta web users add elvis --current-profile
pta web users bind alice < /private/path/verified-alice-identity.json
pta web users unbind alice
pta web users revoke alice
pta web users disable alice
pta web users enable alice
pta --json web callbacks > /private/path/production-callbacks.json
```

Create `elvis --current-profile` only if it is absent. An existing `elvis` account
must keep its ID and current profile association. `add` takes no password and does
not grant access until `bind`. No username/email similarity associates accounts.

After authorizing the **dedicated login OAuth App**, obtain the identity response
from `https://assistant.elvisbrevi.cl/cdn-cgi/access/get-identity` in the signed-in
browser. Save that response locally in a private file. The local operator checks
that it is the intended person's identity from the configured GitHub provider,
then supplies it to `users bind USER`. The response must include `id` (provider
subject), `idp.id`, `idp.type` (`github`), `account_id` and `user_uuid`. Never paste
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
`web_access_support` before the new account schema is written or the portal
reuses them; transition an older host first. No portal starts without Access
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
https://small-forest-4923.cloudflareaccess.com/cdn-cgi/access/callback
```

Verify the team domain again before creating/reusing the OAuth App. GitHub OAuth
App creation/secret generation happens in GitHub Developer Settings; the repository
connection cannot supply dedicated login credentials. Finish the provider's GitHub
authorization/Test flow in Cloudflare before declaring login operational.

The helper `scripts/configure-web-access.py` inspects existing resources, refuses
to overwrite overlapping/unmanaged apps, uses only the dedicated GitHub IdP,
limits edge admission to explicitly authorized emails plus that login method,
and reads back the apps/policies before writing private portal settings. The
allowlist is an edge restriction only; a local provider-subject binding is still
required. Never use an everyone or whole-domain Allow policy.

Required **account** permissions:

- `Access: Apps and Policies Write`: panel app, Allow policy and exact Graph bypass
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
  --hostname assistant.elvisbrevi.cl --allow-email AUTHORIZED_GITHUB_EMAIL \
  --callbacks-file /private/path/production-callbacks.json --dry-run
python3 scripts/configure-web-access.py \
  --hostname assistant.elvisbrevi.cl --allow-email AUTHORIZED_GITHUB_EMAIL \
  --callbacks-file /private/path/production-callbacks.json \
  --settings-file /private/path/access-settings.json
pta web configure < /private/path/access-settings.json
```

The real helper-generated settings contain `bind`, `public_url`, `session_hours`
and `access` (`issuer`, `audience`, `account_id`, `github_idp_id`). Audience/provider
IDs come from actual Cloudflare resources; never invent them. Configuration takes
effect when the portal restarts. Plain HTTP previews cannot authenticate Access.

`pta web callbacks` exports only exact `/webhooks/<profile-id>/graph/notifications`
and `/webhooks/<profile-id>/graph/lifecycle` paths. Re-run setup when adding an
isolated account. Only these callback POST handlers are exempt in the backend;
the exact matching paths have separate Access Bypass apps at the edge. No broad
`/webhooks/*` bypass is accepted. Disabled/unknown profiles and additional paths
cannot start hosts or administer the panel. Graph subscription, clientState,
tenant/resource and validation-token handling stay unchanged. The current
profile retains its separate existing Teams callback hostname/tunnel.

## Sessions, logout and remote operations

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
two distinct approved users' settings/credentials/history, blocked unknown and
disabled identities, JWT rejection, logout/replay/expiration, exact Graph callbacks,
and native Tauri/CLI behavior. Use synthetic data and existing status reads; never
send real Teams messages as a smoke test.
