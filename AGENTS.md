# Guide for agents

Read [the architecture](docs/architecture.md) first: it explains components, pipeline, data, credentials, invariants and change recipes.

## Which skill to use

- Save, correct or organize facts in the knowledge base: [save-knowledge](.agents/skills/save-knowledge/SKILL.md).
- Configure, operate or diagnose the installed application: [personal-teams-assistant](desktop/skills/personal-teams-assistant/SKILL.md) (also `pta skill show`).

## Priorities

- The product is a single Cargo package, `personal-teams-assistant`: the GUI and host `personal-teams-assistant`, the `pta` CLI and its skill. The same binary runs without a window (`--headless`, or built with `--no-default-features` for Linux). There are no other crates, `.app`/`.dmg`/installer bundles or standalone server.
- Tauri is used only in `src/app/gui.rs` (resources in `desktop/`); operations go through `Host`/`Shell` and must build with and without the `gui` feature.
- Delivery priority is the CLI and its shared core. If the GUI falls behind, note it in the architecture's «Known limits»; its parity does not block CLI work. Do not reinstall, publish or update the GUI unless the user asks.
- Everything in the repository is in English: code, comments, documentation, prompts, the GUI, CLI messages and audit steps. What the assistant writes to Teams follows `llm.language` (Spanish by default). Detecting messages without a model keeps Spanish and English phrases, because users write in both.

## Invariants that must not be broken

- Keep the profile `dev.personalteams.assistant`, the Keychain service `personal-teams-assistant.default`, credential names, `data_dir` and `STATE_ENCRYPTION_KEY`: the Microsoft session and the credentials keep working without asking for them again only because of them.
- Do not remove or rename fields of `Config`/`KnowledgeMap` (`deny_unknown_fields`); add only with `#[serde(default)]`. The single authorized exception is the retired `[jev]` section: `Config::retired_reviewer` accepts it so older profiles load, and never writes it again.
- Do not add Graph scopes or change the OAuth flow (public client + PKCE + loopback).
- Never retry a send to Graph; when in doubt, `uncertain` and human review.
- Outside Teams the only writes register the user's unregistered work: personal-chat links/new tasks (`ado::link`) after exact-plan confirmation, or completed tasks under an HU through explicitly configured local/scheduled activity registration (`activity`, `ado::registration`). Both use their own credential (`link_secret_ref`), verified evidence and in-scope open HUs, record before the request and never retry. Automatic registration is opt-in; unclear descriptions, effort, HUs and call attendance require local/CLI clarification. Do not add other writes without the same safeguards.
- Model decisions grant no permissions: audiences, paths, URLs and limits are checked in code.
- New sources start disabled, without audiences and without `external_processing`.
- Never print or log secrets; credentials only on stdin or from the system store.

## Rules for the assistant's answers

- Wiki: documentation the user created or edited can back an answer with their authority. Third-party documentation says where it is and who documented it, without inventing missing authorship. Every Wiki-based answer links the pages it used.
- Every concrete work item, pull request, commit, pipeline, stage or release mentioned carries its verified link.
- With information from Teams conversations, name the people involved when relevant and supported, keeping who said or did what. Do not invent names or attribute an interaction to every chat member.
- In an activity review, verified authorship (an own commit, pull request, run, release, approval, Wiki edit or message) proves the user's own action; never invent activity.

## Before delivering

There is no CI (GitHub Actions is disabled to avoid costs): these local checks are the only gate. Do not add GitHub Actions workflows.

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
bun build desktop/ui/app.js --no-bundle --outfile /tmp/app-check.js   # if the GUI changed
```

Update the architecture and the skill when behavior, commands or schemas change.
