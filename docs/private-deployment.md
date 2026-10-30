# Private portal: macOS deployment and verification

The canonical [web operating guide](../desktop/skills/personal-teams-assistant/references/web-access.md) describes a self-hosted, single-owner installation; the [security policy](../SECURITY.md) describes its trust boundaries. The maintainer's portal is private and is not a service for people installing the crate.

Keep actual account IDs, owner identity responses, tunnel/DNS identifiers, server addresses, settings, tokens, service definitions, logs, backups and deployment evidence in private storage outside this repository. Public documentation uses example domains and synthetic identities. Supply credentials through the system store, protected environment bindings or private files, never in command arguments, public notes or chat.

## Run on macOS

1. Install the same Cargo package and compatible CLI/host pair described in the [README](../README.md). Keep the existing profile, Keychain service, encryption key and Microsoft session. Do not replace a running installation before the new build and configuration are ready.
2. Follow the web operating guide with your own HTTPS hostname, Cloudflare account, dedicated Tunnel/Access/GitHub OAuth application and verified numeric GitHub owner ID. Installing the crate does not grant access to the maintainer's server. Configure one owner; files for other profiles remain inert.
3. On an existing portal, record the assistant's running state and stop the portal normally before changing its service/binary pair. Only a sole, explicitly associated legacy GitHub identity can be inferred as the owner; ambiguous legacy accounts need an explicit `owner_github_id` through `pta web configure`. A new install must be configured with its own owner and Access provider before it can start.
4. Launch `personal-teams-assistant --web` from your user LaunchAgent, with the dedicated connector forwarding to loopback. Do not reuse another person's tunnel or credentials. Restore the recorded assistant state, then verify as below.

Connecting another computer you own is described in the web operating guide (browser sign-in, or a private pairing file from `pta web machines authorize`).

## Verify a deployment

1. Check the CLI and running host versions and the owner's local binding without printing credentials. Confirm that the configured owner, HTTPS origin and Access provider/audience belong to this installation.
2. Check that another identity cannot log in or use an enabled legacy profile. Anonymous or forged browser requests must not read settings, and POSTs require the existing Origin and CSRF protections.
3. Confirm a GitHub API token cannot enroll a machine. Test synthetic machine-bound pairing expiry, one-time consumption and isolation from another installation without starting a real coding run.
4. Verify that only exact public asset, callback and machine routes bypass Access and that each dynamic exception keeps backend authentication. Check hostname-only HSTS at the origin and edge where configured.
5. Preserve the Microsoft session, credential identifiers, encryption key, selected Teams machine and prior assistant running state. Do not send real Teams messages as a smoke test.
6. Distinguish merged, published and installed versions, following [delivery](delivery.md).
