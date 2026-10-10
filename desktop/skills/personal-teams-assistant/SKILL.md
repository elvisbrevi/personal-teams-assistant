---
name: personal-teams-assistant
description: Operating Personal Teams Assistant. Use this skill to configure the app, control its service, manage connections and sources, or run tests and diagnostics.
---

1. **Discover the installation.** Run `pta --version` and `pta --json capabilities`. This skill uses contract 1. It is installed with Cargo (package `personal-teams-assistant`), which puts the host `personal-teams-assistant` and `pta` in the same `bin` directory; both must be the same version. Web/phone access requires `web_access_support: true` (registry 0.7.0 or the verified Access source build); read [web-access.md](references/web-access.md) before upgrading a password-based portal. `status` reports `headless: true` when the host runs without a window (Linux/servers): see [lifecycle.md](references/lifecycle.md). Done when: version and contract are identified; on a mismatch, report which component needs updating.
2. **Inspect.** Read `pta --json status`. Tell apart the host, the assistant, the persisted mode and the loaded configuration. For credentials read `pta --json credentials list`: the host checks the existing store. The account and profile are the GUI's own. Done when: the operation's preconditions and scope are known, including the audiences of the affected sources.
3. **Operate.** Load only the relevant reference and carry out the whole request:
   - Configuration, credentials, mode, import or language models (Codex/Claude CLIs, DeepSeek and their fallback order): [configuration.md](references/configuration.md).
   - Start, stop, windows, tunnel, headless/Linux host or host recovery: [lifecycle.md](references/lifecycle.md).
   - Web GUI, account isolation, mobile access or Cloudflare publishing: [web-access.md](references/web-access.md).
   - OAuth, repositories, sources (including the user's own Teams messages) or the personal chat: [auth-and-knowledge.md](references/auth-and-knowledge.md).
   - Azure DevOps Wiki (list/search/read, its own source and permissions): [azure-wiki.md](references/azure-wiki.md). Needs a host that announces Wiki support (0.3.0 or later).
   - Simulation, activity reviews, diagnostics and the audit log: [tests-and-diagnostics.md](references/tests-and-diagnostics.md).
   - Scheduled or manual activity registration, completed tasks and daily effort, chat/call clarification, pending decisions and history: [activity-registration.md](references/activity-registration.md). The CLI exposes every function the GUI uses.
   The installed help is the authority on arguments. JSON commands return `ok`, `code`, `exit_code`, `data` and `revision`; a pending authorization returns code 4 with instructions or a URL, never tokens. Done when: every requested operation is applied or a concrete human requirement is identified. A timeout needs a fresh inspection before repeating a mutation.
4. **Verify.** Read the status again and run the relevant test. Done when: the persisted and loaded configuration match the request; a stop leaves `running=false`; a Teams answer has a send entry in the audit log and is visible in the authorized destination.
5. **Report.** Give the outcome and the evidence. Done when: configured, connected, started, received, sent and published are told apart, and the cases still without evidence are listed.

The development agent accepts on its own every app permission the operation the user authorized needs, with the tools available and within that scope. The authorization lasts across turns: do not ask again for each dialog or require a human only because it is a system, OAuth or Keychain permission. If the tool cannot control a dialog or the system demands in-person authentication, finish the independent work and explain the exact human action and the blocker observed. This skill cannot enable a capability the tool refuses; never report a permission as accepted or a service as started without verifying it.

The personal chat applies the same source authorizations and code checks as other chats. Changing the mode or enabling it keeps audiences. Simulations may pick sources explicitly without granting them new Teams audiences.

To save or correct knowledge facts, use the project's `save-knowledge` skill when available; this skill manages the application. The internal architecture is in the repository's `docs/architecture.md`.
