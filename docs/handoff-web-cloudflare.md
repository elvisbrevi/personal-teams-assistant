# Private web deployment verification

The canonical [web operating guide](../desktop/skills/personal-teams-assistant/references/web-access.md) describes a self-hosted, single-owner installation. The [security policy](../SECURITY.md) describes its trust boundaries. The maintainer's portal is private and is not a service for people installing the crate.

Keep actual account IDs, owner identity responses, tunnel/DNS identifiers, server addresses, service logs and deployment evidence in private operator storage outside this repository. Public documentation must use example domains and synthetic identities. Earlier public rollout notes are historical records, not reusable configuration or proof of current security.

Verify a deployment using its own infrastructure:

1. Check the CLI/running host versions and the owner's local binding without printing credentials. Confirm that the configured owner, HTTPS origin and Access provider/audience belong to this installation.
2. Check that another identity cannot log in or use an enabled legacy profile. Anonymous/forged browser requests must not read settings, and POSTs require the existing Origin and CSRF protections.
3. Confirm a GitHub API token cannot enroll a machine. Test synthetic machine-bound pairing expiry, one-time consumption and isolation from another installation without starting a real coding run.
4. Verify only exact public asset/callback/machine routes bypass Access and that each dynamic exception retains backend authentication. Check hostname-only HSTS at the origin and edge where configured.
5. Preserve the Microsoft session, credential identifiers, encryption key, selected Teams machine and prior assistant running state. Do not send real Teams messages as a smoke test.
6. Distinguish merged, published and installed versions, following [delivery](delivery.md). Save private deployment evidence and remaining limitations outside Git.
