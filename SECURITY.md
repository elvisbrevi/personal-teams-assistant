# Security policy

Report vulnerabilities privately through [GitHub Security Advisories](https://github.com/elvisbrevi/personal-teams-assistant/security/advisories/new). Do not put credentials, Access assertions, provider tokens, private work items or personal configuration in public issues, pull requests or screenshots.

## Private installations

This repository distributes self-hosted software. It does not provide a shared account service, hosting, credentials or access to the maintainer's infrastructure. Every operator configures their own machine, Microsoft application, provider logins/API keys and, when enabling web access, their own HTTPS domain and Cloudflare Tunnel/Access/GitHub OAuth application.

A portal serves only its locally configured GitHub owner. There is no public registration. A machine joins only after the operator issues a private, expiring, single-use pairing file for that machine on that installation. GitHub API tokens cannot authorize enrollment and are never sent to a coordinator. Treat pairing files and issued machine tokens as secrets. Reassociation, disabling an account or disconnecting a machine revokes its access.

Profiles share the trusted operator's OS account. They are not containers or independent OS tenants, and must not be offered as execution environments for unrelated people. Provider directory permissions are additional controls, not a general filesystem isolation guarantee. Run untrusted code under a separate OS identity/container with appropriately restricted filesystem and network access.

## Deployment

- Keep the browser listener and private IPC on loopback. Protect the complete panel hostname and API with Cloudflare Access, one owner email and the dedicated GitHub provider. Preserve only the documented exact callback/icon/machine protocol exceptions; each dynamic exception authenticates independently.
- Use HTTPS, Secure/HttpOnly/SameSite cookies and the existing Origin/CSRF checks. The origin sends hostname-only HSTS; configure edge headers separately for Access-generated responses. Do not enable parent-domain HSTS preload/includeSubDomains without reviewing the other hosts.
- Use SSH keys and an inbound firewall. Keep remote desktops bound to loopback and behind an authenticated gateway. Verify a working key session before disabling SSH passwords.
- Keep private profiles, credential files and operational/audit records outside the repository, with private directory/file permissions. Linux's credential files rely on OS permissions; use systemd credentials or a secret manager through `NAME_FILE` when appropriate.
- Keep the existing Microsoft session, credential identifiers and state encryption key intact. Never widen Graph scopes to troubleshoot reception. Sending is never automatically retried after an uncertain result.

## Public-source protections

CI scans tracked history and the working tree with a pinned, checksum-verified Gitleaks release, audits `Cargo.lock` against RustSec and rejects newly introduced vulnerable dependencies using GitHub's advisory-backed dependency review. The checks run for pull requests, main and on a weekly schedule (dependency diffs run on pull requests), without repository secrets on standard GitHub-hosted runners. GitHub secret scanning, push protection, vulnerability alerts and automated security fixes complement these checks. Dependency updates require the ordinary review/CI gate.

Only explicitly included source/assets enter the Cargo package. Local output and private runtime state are ignored and excluded from public snapshot exports. Public examples use synthetic identities and example domains; real deployment records belong in the operator's private state directory. Historical public infrastructure identifiers are not credentials or authorization, but should not be copied into a new installation.

A clean scan covers known patterns/advisories at the time it runs. It does not establish that arbitrary repository content, model output or code executed by an implementation agent is trustworthy. Keep provider permissions, source audiences and execution directory checks in force.

The RSA fixture in `src/app/web/access/synthetic-rsa.der` is deliberately public test data, compiled only in tests. Production never signs with it or trusts its public key; Access trust comes exclusively from the pinned team's fetched public keys.

### Upstream maintenance notices

The security update removes the reported vulnerable TLS/crypto dependencies, updates Tokio and upgrades JWT validation to 10.3+ for [the GitHub JWT type-confusion advisory](https://github.com/advisories/GHSA-h395-gr6q-cpjc), which was not in the RustSec findings. Both advisory sources must be checked. RustSec still reports maintenance notices for transitive GUI/parser crates, plus [GLib's Variant type-confusion notice](https://rustsec.org/advisories/RUSTSEC-2024-0429.html) in Tauri's Linux GTK 3 stack. The app does not call the affected Variant API directly. This is an upstream desktop limitation, not a clean bill of health for that stack; the Linux server uses `--no-default-features` and does not include GTK/GLib. Keep the notices visible in dependency audits and reassess when upstream replacements are available.

Application accounts and profile directories are not an operating-system sandbox.
Different people sharing a computer need distinct OS security contexts; switching
app accounts does not protect against an attacker controlling that same OS account.
Additional portal users remain unavailable. Ordinary-account coding, configurable
providers and generic variable consumers require a verified OS boundary before
server execution; absent one, use the account owner's personal trusted machine.
