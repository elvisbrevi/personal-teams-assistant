# Handoff: GitHub login through Cloudflare Access

## Current outcome (2026-10-10)

**Cloudflare Access with the dedicated GitHub OAuth App is configured and
installed on the Mac. Real Mac sign-in, local profile access, logout, immediate
replay rejection and fresh sign-in passed. The owner's phone screenshot confirms
authenticated Home access as `elvis`. Microsoft MFA renewal is complete, live
Graph account verification passes and web Start was verified in observation
mode.** The owner also reported that web Start now succeeds. Complete phone
Settings/activity-history/logout QA and live Teams delivery remain unverified;
do not infer those results from Home access or service startup.

The workspace began clean on `work`, at
[`3674b80`](https://github.com/elvisbrevi/personal-teams-assistant/commit/3674b80).
The current changes preserve all existing profile IDs/directories, credentials,
Microsoft PKCE/scopes, Tauri/CLI IPC and separate tunnels. Original implementation delivery
is on [`feat/cloudflare-access`](https://github.com/elvisbrevi/personal-teams-assistant/tree/feat/cloudflare-access),
with verified implementation commit
[`4ea9255`](https://github.com/elvisbrevi/personal-teams-assistant/commit/4ea9255e676713203dbbffea1de1f2bbf79701c2). Follow the
[Mac transition guide](web-macos-deployment.md) using its verified source commit.

The previous handoff contained superseded installation/publication instructions.
The published
[`personal-teams-assistant 0.6.9`](https://crates.io/crates/personal-teams-assistant/0.6.9)
and Mac password-portal installation were already completed. The live registry
API reconfirmed 0.6.9, not yanked, created at 2026-10-09 03:28:44 UTC, with checksum
`b6b85760f24b8f02672105a1f955853cf476549acaaca21ff3f81fc6f33bcb6f`. At that initial
rollout no crate had been published for Access; registry 0.6.9 still has password
login. The installed Access source build retains package version 0.6.9; check its
`web_access_support` capability and exact source commit, not version alone.

## Crates.io release (2026-10-10)

[`personal-teams-assistant 0.7.0`](https://crates.io/crates/personal-teams-assistant/0.7.0)
is published and not yanked, created at **2026-10-10 22:27:23 UTC**. It contains
the Access/GitHub migration, Strict-cookie bootstrap and Microsoft renewal fixes.
The minor version signals the replacement of password authentication: prepare
Access before updating an older portal, update the host and CLI together, then
bind the existing verified GitHub identities. Preserve profiles and credentials.

Release source on `main`:
[`e131453ddfa0060c4353f84c80e8972e3a3d116e`](https://github.com/elvisbrevi/personal-teams-assistant/commit/e131453ddfa0060c4353f84c80e8972e3a3d116e).
Manifest, lockfile and Tauri version all match 0.7.0; dependency versions were not
changed. README, CLI help and the embedded skill now cover release migration,
required Access settings, mandatory identity binding and Microsoft renewal.
The skill links to the online Mac guide and identifies the configuration helper
as a repository script rather than an installed Cargo executable.

All project gates passed: formatting, both Clippy variants, **143 unit + 39
integration** tests with GUI, **141 unit + 39 integration** without GUI, JavaScript
syntax and seven synthetic Access-helper tests. Publication used a separate clean
`main` clone and the existing protected Cargo token. `cargo publish --dry-run
--locked` compiled the packaged GUI; Gitleaks scanned the extracted package with
redacted output and found no secrets. `cargo publish --locked` then succeeded.

The registry download matches the local package **byte for byte**, with 71 files,
the clean release VCS commit, Access implementation and updated documentation.
Retired password login assets, local data and `output/` are absent. SHA-256:
`adeca5173b8fa78e9dc7714d273cd6ecca11bdeb19ccaa7b1c3209f9338a0f91`.

Publishing did not reinstall the Mac's validated 0.6.9 Access source binaries or
alter its service state, accounts, credentials or tunnels. That installed pair's
source and live verification remain recorded below. Release evidence and logs
are in the private `/tmp/pta-crates-0.7.0._ru8e1_6/` staging directory; full-gate logs
are in `/tmp/pta-release-070-checks-u8wjj2ss/`. No GitHub Actions was added.

## Latest Mac rollout (2026-10-10)

Configuration and deployment are complete on the owner's Mac as `elvis`, without
`PTA_PROFILE_DIR`. The tested deployed source is
[`5607d2bb936a9e292eccee387092f65c0a6fa984`](https://github.com/elvisbrevi/personal-teams-assistant/commit/5607d2bb936a9e292eccee387092f65c0a6fa984)
now included in [`main`](https://github.com/elvisbrevi/personal-teams-assistant/tree/main).
The owner authorized integration on 2026-10-10. The rollout branch was merged by
fast-forward, preserving the validated implementation and fix commits, and
published to `origin/main`. The original
[`codex/cloudflare-access-rollout`](https://github.com/elvisbrevi/personal-teams-assistant/tree/codex/cloudflare-access-rollout)
branch remains available. Integration did not change the deployed binaries,
configuration or services.
The installed source pair's version remains 0.6.9; registry 0.6.9 has the password
portal, while the subsequently published registry 0.7.0 contains Access. Use the
pinned deployed source and `web_access_support` to identify this Mac installation.
No GitHub Actions workflow was added.
The original unrelated untracked `output/` directory was preserved.

### Configured and read back

- Dedicated
  [`personal-teams-assistant-login` GitHub OAuth App](https://github.com/settings/applications/3920526),
  owned by `elvisbrevi`. Its homepage is
  `https://small-forest-4923.cloudflareaccess.com`; its exact callback is
  `https://small-forest-4923.cloudflareaccess.com/cdn-cgi/access/callback`.
  Device Flow and redirect wildcards are disabled. The repository GitHub
  connection was not reused or modified. GitHub's required security confirmation
  was completed by the owner; the generated secret was captured privately.
- Dedicated GitHub IdP `df223db7-1f61-482f-931c-863a6b1922b4`.
  Actual **POST identity_providers succeeded**, and GET readback matched the
  dedicated OAuth client. This verifies the reported IdP write permission.
- Access application `personal-teams-assistant-web`, ID
  `e9e6ba9d-b856-4b39-bc94-fac2d02693f6`, protecting the whole
  `assistant.elvisbrevi.cl` hostname, including its API. Actual **POST apps
  succeeded**. Audience:
  `e3513b368c8cc140a54c6db1d517dda3f3ef4304d3ad75c99850bdee797ddf48`.
- Exactly one Allow policy, `Authorized GitHub identities`, includes only the
  owner's verified primary GitHub email, confirmed on
  [GitHub email settings](https://github.com/settings/emails), and requires the
  dedicated GitHub login method. No everyone/domain-wide admission is present.
  Session duration is 12 hours and the actual
  `http_only_cookie_attribute` is true. GET application/policy readback passed;
  the helper's subsequent **PUT reuse also succeeded** without creating another
  app/provider. Unrelated Workers and `agent-workflow.elvisbrevi.cl` apps remain.
- Team domain is unchanged: `small-forest-4923.cloudflareaccess.com`.
  The existing protected Cloudflare API token was reused; no token-policy
  management permission or replacement credential was requested.
- Dedicated tunnel `08cce5df-23a0-45e3-92f6-e65f2d8abe3e` remains **healthy with
  four connections**. Its connector LaunchAgent/PID was preserved throughout.
  CNAME and ingress remain `assistant.elvisbrevi.cl` → that tunnel →
  `http://127.0.0.1:38656`, preserving Host, with the existing 404 catch-all.
  No Tunnel, DNS, landing or desktop Teams route was changed.

### Installed and preserved

Both default-GUI host and CLI were built in a separate staging checkout before
cutover. Required gates passed before deploying fixes. The ordinary Cargo pair
was first installed from the source, then both tested staged files were replaced
atomically for the fixes and verified byte-for-byte against that staged pair.
`codesign --verify --strict` passed. Both the CLI and running host announce
`web_access_support: true`; native Tauri remains available and opens normally.

The existing enabled `elvis` account retains ID
`0b87fed6-24aa-4037-940e-300670570f4b`, `current_profile: true` and the original
`dev.personalteams.assistant` profile. The official signed-in Access identity
response matched the configured account/GitHub IdP, the verified email and
GitHub API numeric ID `916745`. The unchanged response was supplied to local
`pta web users bind elvis`; its provider-subject binding is `916745`.
No profile copy, replacement account or email-based association was performed.

The original configuration/knowledge-map fingerprints and revision remain
unchanged. Credential names and system-store origins, Keychain service,
`STATE_ENCRYPTION_KEY`, Microsoft PKCE/scopes and `data_dir` were preserved.
Microsoft and repository GitHub still report locally connected. The assistant
was stopped before cutover, briefly started under observation with a synthetic
sender filter for the verification below, then stopped again. Its original
configuration/revision was restored, with activity registration disabled. No
Teams message was sent and no model was invoked as a live test. The current host is
native Tauri (`headless: false`); the portal reuses that host through private IPC.

Existing service labels and plists are retained:
`dev.personalteams.assistant.web` and `.web.tunnel`. Both were verified running,
with the portal listening only on `127.0.0.1:38656`. A restart initially raced
LaunchAgent unloading and returned bootstrap error 5; inspection confirmed the
job absent, and re-bootstrap after unloading completed succeeded. No root
service or replacement plist was introduced.

Private staging is
`~/Library/Application Support/dev.personalteams.assistant/web/runtime/access-build/`.
It retains `source-path.txt`, `deployed-source.json` (source/binary fingerprints),
`production-callbacks.json`, official identity JSON, private OAuth bindings,
helper-generated settings, readback metadata and gate logs. Runtime credentials
are 0600 under a private directory and are never in Git or this handoff.
`rollback-path.txt` identifies the private pre-transition backup containing web
accounts/settings JSON, the portal plist, installed sibling binaries and preflight
metadata. Restore that web JSON/binary pair together if rollback is required;
never replace profile data or the encryption key.

The latest binary replacement and full-gate results are recorded separately in
`microsoft-renewal-deployed.json` and `microsoft-renewal-*.log`. Its private sibling
binary backup is recorded there; `microsoft-start-verification.json` records the
successful web Start, account read and restored final configuration. Earlier
`deployed-source.json` describes the preceding Access rollout.

### Graph callbacks

The new CLI exported actual production callbacks before account schema writes.
Its `paths` list is **empty**, because `elvis` is the only portal account and uses
its current desktop profile, retaining the separate existing Teams callback URL.
No wildcard or invented callback Bypass app was created. Re-export/reconfigure
exact notification/lifecycle exceptions before adding an isolated account.

Synthetic local POSTs showed an unknown exact canonical callback returns 404
without starting a host, while an extra path suffix remains protected (401).
At the edge, an unrelated canonical webhook URL redirects to Access (302).
Existing synthetic Graph validation/clientState regressions passed. With the
assistant intentionally stopped, live Teams callback delivery was not tested.

### Live Mac verification

- TLS verification enabled. Anonymous panel, API and unrelated webhook requests
  redirect to Access (302); no password login form is exposed.
- Real GitHub login and the official Access identity lookup passed. Authenticated
  `/api/session` returned 200 with `username: elvis`, `current_profile: true` and
  `auth_provider: github`, without a second password sign-in.
- Authenticated configuration/snapshot, audit, activity status, activity history
  and pending activity reads returned 200/`ok: true`. All six shared UI sections
  are present. No settings save, activity registration or chat send was used.
- CSP, nosniff, no-referrer and no-store headers were checked on the authenticated
  HTTPS session. Local and Access cookies are unavailable to page JavaScript.
  Cookie Secure/HttpOnly/Strict and JWT lifetime bounds passed the synthetic
  HTTP regression tests; raw live cookie values were never extracted or printed.
- Retired `/api/login` and `/api/password` returned 404 for an authenticated user.
  Valid read commands returned 200; missing/invalid CSRF returned 403.
  Native IPC still rejects a browser Origin (403).
- Direct loopback identity-only, cookie-only and malformed-assertion requests
  returned 401; a foreign Host returned 421. Invalid crypto/audience/time/provider,
  disabled/unknown users, two-user isolation and expiration passed synthetic tests.
- Logout returned 200, then session access and bootstrap with the same assertion
  returned 401 immediately. Revocation was retained in the private durable store
  across portal restart. Access logout displayed successful sign-out. A subsequent
  real GitHub flow returned to `/login` and produced a new authenticated 200
  `elvis` session after the Strict-cookie fix below.
- The initially failing Microsoft account read was diagnosed: the saved session
  decrypts with the existing key and matches the configured tenant/user/client,
  but Entra refresh returned HTTP 400, `invalid_grant`, `AADSTS50078` (expired MFA).
  The owner completed fresh interactive verification using the original public
  client/PKCE/loopback flow. No logout, key replacement or scope change was needed.
  `pta auth microsoft finish` and `pta test connectivity` then passed, including
  after deploying both corrected binaries. The live account is verified; sending
  and webhook reception are not inferred from that read.
- The shared browser's actual **Start assistant** button started the native host's
  service. Authenticated `/api/session` and `/api/control` snapshot returned 200;
  the UI showed **Running in observation mode**, and the host reported
  `running: true`, `tunnel_running: true`, `dry_run: true`. The test temporarily
  restricted allowed senders to a synthetic UUID, preventing real messages from
  reaching models, and kept activity registration disabled. Durable sent-message
  counts did not increase. The initial attempt waited in a macOS Keychain read
  and the browser timed out; the owner subsequently reported successful Start
  and both UI/host state confirmed it. No ACL or secret was changed by the agent.
  The service was then stopped and the exact original configuration/map/revision
  restored. Final state: stopped, native GUI available, Microsoft read passes.

The owner initially deferred physical-phone QA, then supplied an authenticated
phone Home screenshot for `elvis` and reported Start failing on both phone and
computer. After Microsoft renewal the owner reported web Start succeeds, without
specifying the device in that last report. Phone Home access is supported by the
screenshot; the Mac Start result above was directly checked. Settings, activity
history and phone logout/re-entry still need explicit confirmation. No connected
phone-control tool is available. Do not report complete phone QA, production
second-person login or live Teams delivery as completed.

### Fixes found by actual deployment

The previously validated implementation needed these live compatibility fixes:

1. Cloudflare returns GitHub `id` as a JSON integer. String-only deserialization
   caused HTTP 503 and refused operator binding. Positive integers now normalize
   losslessly to the existing string binding, preserving signature/issuer/audience,
   account/IdP/subject validation. Negative/zero/float/other types still fail closed.
2. Cloudflare returns the same application domain in three representations.
   The helper now deduplicates representations within one app, retaining its
   rejection of overlapping distinct/unmanaged resources. It uses the official
   HttpOnly attribute and requires its readback before writing local settings.
   These fixes are in
   [`e0c9b32b5db0ba74004531782e86b64d577b680d`](https://github.com/elvisbrevi/personal-teams-assistant/commit/e0c9b32b5db0ba74004531782e86b64d577b680d).
3. A real cross-site GitHub return omitted the Strict cookie through the original
   303 chain, producing a login loop and then 429. New session bootstrap now serves
   the shared panel document as a first-party response before asset/API requests.
   Strict/HttpOnly/Secure, CSRF and JWT validation remain enforced. The deployed
   fix is
   [`beee74ae1608537239a78f8fdac8a043ea8a735a`](https://github.com/elvisbrevi/personal-teams-assistant/commit/beee74ae1608537239a78f8fdac8a043ea8a735a);
   fresh login after Access logout passed.
4. Expired Microsoft MFA caused Start to return a generic configuration error.
   Entra's fixed `invalid_grant`/`interaction_required` codes now produce a typed
   authorization error, preserved when the service exits before readiness. GUI,
   CLI and web report `not_ready` with a Settings reconnection instruction.
   Provider descriptions stay private and failed renewal retains the encrypted
   session. The 60-second readiness timeout still stops/aborts the service.
   This fix is the latest pinned source above. Both regression tests failed
   before the fix; classified/unknown responses, session preservation, shared
   startup error handling and timeout cleanup now pass with synthetic data.

Regression tests failed before the fixes and passed after them. On the final Rust
implementation all required gates passed: formatting, both Clippy variants,
**143 unit + 39 integration** tests with GUI and **141 unit + 39 integration**
without GUI. Seven synthetic setup-helper tests and the JavaScript build passed
during the preceding Access fixes; those files were unchanged in the MFA fix.
Gitleaks scanned the fix diff with redacted output and found no secrets. No
production two-user test or physical-phone result is inferred from these counts.

### Remaining closure

1. Complete the phone's retained Settings/Activities-history and logout/re-entry
   checks. Phone Home access and the owner's later web Start report are recorded
   above; do not substitute them for the remaining checks.
2. The live Microsoft account read now passes after MFA renewal. Live Teams
   callback reception and message delivery were not tested. Preserve the OAuth
   flow/scopes, session, credentials and separate Teams tunnel; no real Teams
   sends were authorized as verification.
3. Keep the Mac awake/connected/logged in. The rollout is published in `main` and
   registry 0.7.0; the already validated Mac source installation was preserved.

## Earlier cloud continuation snapshot

Read-only API inspection at **2026-10-10 10:00 America/Santiago** reconfirmed:

- The configured token is active; its ID is `005cf5b38ffa573d4edfe8aae87886a4`
  (an identifier, not the token secret).
- Team domain: `small-forest-4923.cloudflareaccess.com`.
- Only the existing Cloudflare IdP and unrelated Workers application remain;
  there is still no GitHub IdP or Access application for the panel.
- The dedicated web tunnel is **healthy with four connections**, active since
  2026-10-10 09:25 America/Santiago. This supersedes the October 9 down status.
- The proxied CNAME and ingress remain correct: the dedicated tunnel serves
  `assistant.elvisbrevi.cl` at `http://127.0.0.1:38656`, then a 404 catch-all.
- No Cloudflare resource or production Mac file was modified in this follow-up.

The user reports adding both required Access write permissions to the existing
token. Preserve that progress: successful reads are verified, but **successful
writes have not been retested since the permission change**. Recheck with the
actual necessary configuration operations and report their specific errors; do
not treat the old 403 as a current rejection or request token-policy management.

Dedicated GitHub login bindings remain absent in the cloud environment. OAuth App
creation and provider Test were explained to the user, but their completion was
not confirmed. Check GitHub Developer Settings and Cloudflare again on the Mac
before creating anything. If the dedicated provider has already been configured
manually, the helper can reuse it without requiring its client secret on the Mac.
Only a provider that still needs creation requires the dedicated OAuth credentials.

TinyFish is confirmed installed and enabled, but no callable TinyFish/browser
connector tools appeared in this session. Local Chromium/Playwright opened GitHub
with TLS verification; it reached the sign-in page in a fresh cloud browser
session. That browser did not have the owner's Chrome login. Do not ask the user
to install TinyFish again or assume an open Mac Chrome is remotely controllable.
Use the Mac session's actually available browser tools, or let the owner complete
the required GitHub authentication locally. Never request passwords or OAuth
secrets through chat.

The cloud session has no Mac execution channel. Current Mac binary versions,
accounts, identity bindings and service state still require local inspection;
tunnel health alone does not verify the backend version or HTTPS sign-in.

## Cloudflare inspection and resources

The environment's actual token verified active, ID
`005cf5b38ffa573d4edfe8aae87886a4`. Credential values were never printed. Reads of
Access organization, identity providers, applications, the dedicated tunnel,
ingress and DNS succeeded with HTTP 200. During the original October 9 inspection,
declared token policies could not be inspected (`GET /user/tokens/ID`: HTTP
403/9109), so authorization was checked against the actual required write
operations, not inferred from token metadata.

Account: `26f1f3a05cbfe51ade90a57362c15fad`.
Zone: `aa9e93243e63fe077f154b6a3fdaac55`.
Team: `small-forest-4923.cloudflareaccess.com`.

Observed Access resources:

- Only the existing Cloudflare IdP (`16e67451-24e8-4572-b4f6-de33dfb0e333`);
  no GitHub provider.
- Only the unrelated `solid-editor - Cloudflare Workers` application
  (`c62fac5d-64e4-422e-b3ff-926e289167c8`); no panel application.
- The original October 9 GitHub provider and panel application creation attempts
  both returned HTTP 403 `auth.forbidden`. These precede the user's reported
  permission changes; no resource was created/modified.

Required permissions, scoped to this **account**:

- `Access: Apps and Policies Write` for the panel application, restricted Allow
  policy and exact callback Bypass applications.
- `Access: Identity Providers Write`, or the combined
  `Access: Organizations, Identity Providers, and Groups Write`, for the dedicated
  GitHub login provider. Existing organization/provider/application reads work.

The user reports updating the existing bound token with these permissions. A token
value does not need to be sent through chat. Token-policy read/management permission
is not required to complete this task and should not be requested merely to inspect
its declaration. Verify the Mac's available credential privately and test the
actual required write when its real inputs are ready.

Dedicated login bindings `PTA_ACCESS_GITHUB_CLIENT_ID[_FILE]` and
`PTA_ACCESS_GITHUB_CLIENT_SECRET[_FILE]` are absent in the cloud session; their Mac
availability is unknown. GitHub OAuth App existence in Developer Settings could
not be inspected from an authenticated session here. Check before
creating a dedicated **OAuth App** named `personal-teams-assistant-login`; never
reuse repository GitHub App/Device Flow credentials. Its callback must be:

```text
https://small-forest-4923.cloudflareaccess.com/cdn-cgi/access/callback
```

The existing dedicated tunnel/DNS/ingress are correct and unchanged:

- `personal-teams-assistant-web`: `08cce5df-23a0-45e3-92f6-e65f2d8abe3e`.
- Proxied `assistant.elvisbrevi.cl` CNAME → that tunnel's `.cfargotunnel.com`.
- `assistant.elvisbrevi.cl` → `http://127.0.0.1:38656`, preserving Host;
  then `http_status:404`.
- **Current inspection: healthy, four connections** (October 10). The previous
  down status and October 9 disconnection are historical.

The cloud network lacks the panel/team destinations, VPN and Mac TCP grants.
An HTTPS request to the panel with TLS verification enabled was blocked at proxy
CONNECT (403). No alternate proxy/Tunnel/route bypass was attempted. There is no
Mac execution channel, so current Mac account/files/services could not be read
or upgraded. The prior Mac observation recorded `elvis --current-profile`,
portal `~/.cargo/bin/personal-teams-assistant --web`, dedicated LaunchAgents
`dev.personalteams.assistant.web` and `.web.tunnel`, and private connector file
`~/Library/Application Support/dev.personalteams.assistant/web/runtime/cloudflared-token`.
Reinspect locally; do not recreate those resources from stale assumptions.

## Implemented source behavior

- RS256 JWT signature/issuer/audience/issuance/not-before/expiry and application
  type validation against the official team certs. Caller-selected URLs,
  identity/email-only headers, service tokens and invalid/duplicate assertions
  are rejected. JWKS freshness/rotation and identity lookups are bounded;
  stale keys never remain trusted after a failed refresh.
- Exact verified assertion lookup at the pinned official get-identity endpoint;
  GitHub IdP ID/type, account and `user_uuid == sub` must match. Sessions cannot
  cross assertions or identities. Positive identities cache for at most 30 seconds.
- Operator-local binding of issuer + GitHub IdP + provider subject `id`. No email,
  username or Access `sub` auto-association. Existing `elvis`/current-profile ID
  is retained; if absent, local `users add elvis --current-profile` prepares it.
- `web users bind/unbind/revoke/disable/enable` and `web callbacks`, documented
  in CLI help and the embedded operating skill. Recovery remains local. Revoke
  also requires a newly issued Access assertion, preventing old-token rebootstrap.
- `/login` creates a CSRF-bound session and serves the shared panel document
  without a password form, ending the cross-site navigation before Strict-cookie
  asset/API requests.
  Password assets, `/api/login` and `/api/password` are removed in the new binary.
  Legacy JSON remains readable and retired password hashes remain inert, with
  no profile/credential deletion or permissive authentication fallback.
- Local sessions/cookies expire no later than the JWT. Exact Origin, CSRF,
  command allowlist, version/disabled-account checks and file/credential isolation
  remain. Logout persistently blocks the assertion fingerprint, clears the local
  cookie and navigates to Access logout. Cloudflare global revocation propagation
  is distinct from immediate local replay rejection.
- Only canonical exact Graph notification/lifecycle POST handlers bypass backend
  Access middleware; existing forwarding/batch validation is unchanged. The
  helper takes exact paths from **production Mac** `pta web callbacks` output,
  creates their separate Bypass apps before protecting the panel hostname/API,
  and rejects wildcard/foreign/unmanaged applications and policies.
- Additive `web_access_support` descriptor/capability prevents silently reusing
  an incompatible password-era host. Local account edits also require existing
  hosts to support the schema. The guide stages the build, backs up web JSON and
  transitions the host/CLI together, restoring the previous assistant state.

Important files: `src/app/web/access.rs`, its synthetic tests,
`src/app/web/{store,tests,profiles}.rs`, `src/app/web.rs`, CLI/IPC capabilities,
shared UI transport/account controls, `scripts/configure-web-access.py` and
`scripts/test-configure-web-access.py`. Architecture, README, Mac guide and
`desktop/skills/personal-teams-assistant/references/web-access.md` are updated.

## Original implementation verification

All `AGENTS.md` gates passed on the final implementation:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
bun build desktop/ui/app.js --no-bundle --outfile /tmp/app-check.js
```

- Default GUI features: **139 unit + 39 integration** tests passed.
- No default features: **137 unit + 39 integration** tests passed.
- `transport.js` syntax build and six synthetic Access setup-helper tests passed.
- Gitleaks 8.30.1 scanned the complete deliverable source snapshot with redacted
  output and found no secrets. Its downloaded binary checksum was verified.
- The actual headless sibling binaries built with `--locked` and native CA roots.
- Real-process smoke QA in temporary synthetic profiles passed native CLI/IPC,
  account creation/association, duplicate binding rejection, retained IDs/current
  profile, callback export, fail-closed HTTP/JWT/identity-only access, exact bypass
  confinement, security headers and local recovery/revocation. All test hosts
  stopped; temporary profiles were removed.
- Crypto/HTTP tests exercise two isolated identities/profiles, invalid signatures,
  issuer/audience/time/type/service tokens, IdP/account/subject mismatch, JWKS
  rotation/outages, redirects/response limits, CSRF, logout/replay/restart, disabled
  accounts, local/JWT expiry, legacy migration and incompatible host refusal.
- Existing Graph/Microsoft/Tauri/CLI regressions passed. No real Teams messages,
  model calls or production credentials were used for testing.

These checks do **not** establish real GitHub OAuth compatibility, the live
provider-subject shape, Cloudflare configuration, Mac installation, physical-phone
HTTPS login, production two-user isolation or live Graph callback reachability.
Those remain pending and must be recorded independently after verification.

The documentation-only October 10 handoff update reran `cargo fmt --all --check`,
both required Clippy configurations, both Cargo test configurations and the six
synthetic Access setup-helper tests successfully. The implementation is unchanged;
these repeated checks still do not establish production deployment or browser QA.

## Original rollout checklist (historical)

1. Continue on the owner's Mac under the ordinary application owner, without a
   `PTA_PROFILE_DIR` override. Read `AGENTS.md`, `docs/architecture.md`, this latest
   branch handoff, `docs/web-macos-deployment.md` and
   `desktop/skills/personal-teams-assistant/references/web-access.md`. Preserve
   local checkout changes; inspect `git status`, `pta --version`,
   `pta --json capabilities`, `pta --json status`, `pta --json web status`,
   `pta web users list` and the existing LaunchAgents. Record assistant running
   state and the portal executable path without printing credentials or plist
   environment values. Implementation/configuration/deployment are already
   authorized; ask only for genuinely missing information or in-person login.
2. Check/reuse or create the dedicated GitHub **OAuth App**
   `personal-teams-assistant-login` in the owner's signed-in browser. Homepage:
   `https://small-forest-4923.cloudflareaccess.com`; callback:
   `https://small-forest-4923.cloudflareaccess.com/cdn-cgi/access/callback`.
   Complete the dedicated Cloudflare GitHub provider's authorization/Test and
   obtain the actual explicitly authorized GitHub email allowlist. Reuse a
   compatible provider created meanwhile. For helper-based creation, supply
   dedicated client ID/secret through private bindings or 0600 files; never use
   repository OAuth credentials or send secrets through chat. The Cloudflare API
   token bound in the cloud is not necessarily configured on the Mac: inspect its
   presence privately and use the owner's existing protected credential.
3. Follow the [Mac transition guide](web-macos-deployment.md). Build the pinned
   verified Access host/CLI pair with default GUI features in a separate staging
   location before stopping services; require `web_access_support: true`. The
   published registry package is still the password build. Export actual
   production callback IDs with the staged new CLI and run the Access helper's
   dry-run. Do not recreate the healthy tunnel, DNS or existing LaunchAgents.
4. Back up web account/settings JSON, the installed binary pair and portal plist
   privately. Stop the old portal and incompatible profile host normally; install
   the compatible pair. Run the helper with the real allowlist/callback export,
   then `pta web configure` on its private settings file. It prepares exact Graph
   callback Bypass apps before protecting the entire panel/API. Never bypass all
   `/webhooks/*`, overwrite the unrelated Workers app or change Microsoft scopes.
5. Keep the existing `elvis` account ID/current-profile association. Only if absent
   and the current profile is unclaimed, run `pta web users add elvis --current-profile`.
   Sign in through GitHub to the panel's `/cdn-cgi/access/get-identity`; save the
   official JSON in a private local file and verify its provider/account/person.
   Bind with `pta web users bind elvis < /private/path/verified-elvis-identity.json`.
   Require the stable provider `id`, GitHub `idp.id`/`idp.type`, account and
   `user_uuid`; investigate any unexpected live shape without weakening validation.
   Never associate by matching email/name or replace the existing profile.
6. Restore the portal, dedicated connector and previous assistant state, then
   verify GitHub-only sign-in from Mac/phone, two approved independent users,
   rejection cases, logout/replay/expiry, Graph callbacks and native Tauri/CLI.
   Update this handoff with actual deployed commit and evidence. No real Teams
   message may be sent as a test. Adding an isolated profile requires re-exporting
   callbacks and rerunning the helper for its exact exceptions.

## Local invariants and completion evidence

Preserve `dev.personalteams.assistant`, Keychain service
`personal-teams-assistant.default`, credential names, `data_dir`,
`STATE_ENCRYPTION_KEY`, profile IDs/directories, Microsoft PKCE/scopes and the
separate existing Teams tunnel. The expected owner profile is
`~/Library/Application Support/dev.personalteams.assistant`; expected web service
labels are `dev.personalteams.assistant.web` and
`dev.personalteams.assistant.web.tunnel`. Reinspect these local facts before edits.

The Mac handoff must report configured, deployed and verified separately. Record
the installed source commit and host capability, actual Access app audience/IdP,
the retained owner account/profile ID, service/tunnel health and each real QA
result. Required implementation gates and synthetic test counts above are evidence
for the unchanged pinned implementation; they do not replace production QA. Run
the `AGENTS.md` gates for any implementation fixes needed on the Mac. Rollback
restores the backed-up web JSON and binary pair together as described in the Mac
guide; preserve all profile data and exact Graph exceptions.
