# Guide for agents

Read [the architecture](docs/architecture.md) first: it explains components, pipeline, data, credentials, invariants and change recipes. [CONTEXT.md](CONTEXT.md) holds the domain's vocabulary (source, context, verified note, activity record…); use its terms.

## Which skill to use

- Save, correct or organize facts in the knowledge base: [save-knowledge](.agents/skills/save-knowledge/SKILL.md).
- Configure, operate or diagnose the installed application: [personal-teams-assistant](desktop/skills/personal-teams-assistant/SKILL.md) (also `pta skill show`); from an agent with MCP, `pta mcp` exposes the same operations ([mcp.md](desktop/skills/personal-teams-assistant/references/mcp.md)).
- Change the GUI (`desktop/ui/`): follow [the GUI design conventions](docs/ui-design-conventions.md).

## Priorities

- The product is a single Cargo package, `personal-teams-assistant`: the GUI and host `personal-teams-assistant`, the `pta` CLI and its skill. The same binary runs without a window (`--headless`, or built with `--no-default-features` for Linux). There are no other crates, `.app`/`.dmg`/installer bundles or standalone server.
- Tauri is used only in `src/app/gui.rs` (resources in `desktop/`); operations go through `Host`/`Shell` and must build with and without the `gui` feature.
- Delivery priority is the CLI and its shared core. If the GUI falls behind, note it in the architecture's «Known limits»; its parity does not block CLI work. Do not reinstall, publish or update the GUI unless the user asks.
- Everything in the repository is in English: code, comments, documentation, prompts, the GUI's source text, CLI messages, IPC messages, diagnostics and audit steps. What the user reads follows `llm.language` (Spanish by default): the GUI (translated at runtime by `desktop/ui/i18n.js`, whose catalog is the only Spanish UI text), what the assistant writes to Teams, and the activity drafts and issues it writes for the user. Every new GUI string goes through `t()`/`tn()` (or static HTML) and gets its catalog entry; `scripts/test-ui-i18n.cjs` fails otherwise. Detecting messages without a model keeps Spanish and English phrases, because users write in both.

## Invariants that must not be broken

- Keep the profile `dev.personalteams.assistant`, the Keychain service `personal-teams-assistant.default`, credential names, `data_dir` and `STATE_ENCRYPTION_KEY`: the Microsoft session and the credentials keep working without asking for them again only because of them.
- Do not remove or rename fields of `Config`/`KnowledgeMap` (`deny_unknown_fields`); add only with `#[serde(default)]`. The single authorized exception is the retired `[jev]` section: `Config::retired_reviewer` accepts it so older profiles load, and never writes it again.
- Do not add Graph scopes or change the OAuth flow (public client + PKCE + loopback).
- Never retry a send to Graph; when in doubt, `uncertain` and human review.
- Azure evidence and daily activity runs are read-only. The implementation flow may create children only from a new implementation plan the user explicitly reviewed and confirmed, using its dedicated write credential. Daily activity runs never create, link or change work items. Legacy write credentials and audit attempts remain intact, and plans saved by older hosts cannot authorize new writes.
- Model decisions grant no permissions: audiences, paths, URLs and limits are checked in code.
- New sources start disabled, without audiences and without `external_processing`.
- Never print or log secrets; credentials only on stdin or from the system store.

## Rules for the assistant's answers

- Wiki: documentation the user created or edited can back an answer with their authority. Third-party documentation says where it is and who documented it, without inventing missing authorship. Every Wiki-based answer links the pages it used.
- Every concrete work item, pull request, commit, pipeline, stage or release mentioned carries its verified link.
- With information from Teams conversations, name the people involved when relevant and supported, keeping who said or did what. Do not invent names or attribute an interaction to every chat member.
- In an activity review, verified authorship (an own commit, pull request, run, release, approval, Wiki edit or message) proves the user's own action; never invent activity.

## Delivering

GitHub Actions is the gate and the delivery ([delivery](docs/delivery.md)). Develop, open a pull request, wait for its **CI result** check to pass and merge it: `gh pr merge --auto --merge --delete-branch` waits for CI. Never bypass the check (`--admin`) or merge with CI failing.

Merging delivers: a version bump (`Cargo.toml`, `Cargo.lock`, `desktop/tauri.conf.json`) is published to crates.io by `release.yml` once CI passed on `main`, with its tag and GitHub release; the server's updater installs it within minutes; a change under `site/` is published by Cloudflare. Do not publish, tag or install on the server by hand; do it only as the fallback in [delivery](docs/delivery.md#when-the-automatic-path-fails), when the automatic path itself failed.

When delivering product changes, include a new, unpublished version in all three version files before opening the pull request. A merge and push alone do not publish an existing version. Verify the new crates.io version and the server's running version and health before reporting delivery complete; distinguish merged, published and installed.

Workflows use only GitHub-hosted standard runners (free while the repository is public): no larger, paid or self-hosted runners, and never a runner on the server. `ci.yml` has no secrets because pull requests from forks run it; grant other permissions per job, only where needed.

Run the checks locally before pushing, as fast feedback (CI runs the same on Linux, plus macOS and an informative Windows build):

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
bun build desktop/ui/app.js --no-bundle --outfile /tmp/app-check.js   # if the GUI changed
node --test scripts/test-activity-ui.cjs scripts/test-ui-i18n.cjs  # if the GUI changed
```

If the GUI changed and Playwright is available, also run the preview checks in [the GUI design conventions](docs/ui-design-conventions.md#10-verification) (axe WCAG 2.2 AA scans and interaction checks with synthetic data).

Update the architecture and the skill when behavior, commands or schemas change.
