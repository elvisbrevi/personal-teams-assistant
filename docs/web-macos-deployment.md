# Run your private portal on macOS

Install the same Cargo package and compatible CLI/host pair described in the [README](../README.md). Keep the existing profile, Keychain service, encryption key and Microsoft session. Do not replace a running installation before the new build and configuration are ready.

Follow the [web operating guide](../desktop/skills/personal-teams-assistant/references/web-access.md) using your own HTTPS hostname, Cloudflare account, dedicated Tunnel/Access/GitHub OAuth application and verified numeric GitHub owner ID. Installing the crate does not grant access to the maintainer's server. Configure one owner; existing files for other profiles remain inert.

Store actual settings, tokens, identity JSON, service definitions and backups in private local storage outside the repository. Supply credentials through the system store, protected environment bindings or private files. Never put them in command arguments, public deployment notes or chat.

On an existing portal, record its assistant running state and stop the portal normally before changing its service/binary pair. Only a sole, explicitly associated legacy GitHub identity can be inferred as the owner; ambiguous legacy accounts require an explicit `owner_github_id` through `pta web configure`. An existing current-profile association keeps its ID, files and credentials. A new install must be configured with its own owner and Access provider before it can start.

Launch `personal-teams-assistant --web` using your normal user LaunchAgent, with the dedicated connector forwarding to loopback. Do not reuse another person's tunnel or credentials. Restore the recorded assistant state and verify HTTPS, anonymous rejection and owner access using the [deployment verification guide](handoff-web-cloudflare.md).

To connect another computer you own, read its `machine_settings.id`, authorize it locally on your coordinator with `pta web machines authorize MACHINE_ID --output PRIVATE_FILE`, and securely transfer that private file to the selected computer. Pass it to `pta tools call machine_connect` on stdin within ten minutes, then delete it. GitHub tokens and the GUI's URL-only Connect action cannot authorize enrollment. No inbound listener is needed on the connected computer.
