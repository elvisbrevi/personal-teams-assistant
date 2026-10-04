# Proposed improvements

Proposals that came out of reviewing the whole codebase (2026-10-01, updated 2026-10-03). Unless «Status» says otherwise, they are not implemented. Priority: **P1** high value and low risk, **P2** clear value with more effort, **P3** evolution. Each item says where the problem is, so it can be picked up without investigating again.

## 1. Architecture

### P1 — Typed host operations
- **Problem:** `src/app.rs` (≈1,300 lines) mixes the tray UI, service start, OAuth, tunnel, settings and Wiki. `control::operate` dispatches on strings (`"start_assistant"`) and exit codes are derived from text prefixes (`"[invalid_input] …"`, `"[not_ready] …"`).
- **Status:** operations no longer depend on Tauri (`Host` + `Shell`, `gui.rs`/`headless.rs`), but they are still dispatched on strings.
- **Proposal:** `enum Operation` (serde) and `enum HostError { InvalidInput, NotReady, Conflict, Network, Pending, Failed }` mapped to exit codes. Tauri and IPC stay thin adapters.
- **Benefit:** operations tested without a window, fewer mistakes when adding commands, consistent GUI/CLI messages.

### P1 — One provider factory
- **Problem:** the model chain, the redactor and the HTTP client are built the same way in `runtime.rs` and `local_chat.rs`, with the DeepSeek endpoint fixed in `llm.rs`.
- **Proposal:** `Providers::from_config(&Config)` returning llm, redactor and client; optional endpoints in `[llm]` with `#[serde(default)]`.

### P2 — Pipeline in stages with typed states
- **Problem:** `Pipeline::process_with_sources` (`src/pipeline.rs`) is a ~500-line function; states and reasons are free strings; the activity tool used by status questions is still recognized by the fixed ID `"azure-devops-status"` (activity reviews already route by `ToolSpec` type).
- **Proposal:** stages `triage → retrieve → generate → cite → deliver`, each with a typed result; `enum JobStatus` and `enum Reason` serialized as today (audit compatibility); route by `ToolSpec` type, not by ID.
- **Benefit:** each stage testable on its own and stable reasons for the GUI.

### P3 — CLI in its own crate
- **Status:** with `--no-default-features` the package already builds `pta` and the host without Tauri. With the GUI on, `pta` still links Tauri.
- **Proposal:** a `pta-protocol` crate (Request/Reply/contract) and `pta` as its own crate, so installing or building only the CLI is fast in every variant.

### P2 — Versioned configuration schema
- **Problem:** `graph.channels` and `policy.max_answer_chars`/`max_detailed_answer_chars` are no longer used but must persist because older hosts sharing the profile require them (`deny_unknown_fields`); the retired `[jev]` section is accepted and dropped by a special field.
- **Proposal:** `version = 2` in `config.toml` with an explicit migration and backup, applied once no 0.5.x host remains. Then remove the legacy fields.

### P3 — Split `src/ado.rs` (≈1,700 lines) and `src/ado/wiki.rs` (≈2,300)
- Separate catalog, Git/commits, pipelines/stages, releases, Teams context and references; the existing tests move with each module (`ado/review.rs` already lives apart).

## 2. Assistant behavior

### P1 — Automatic reception and a participation decision
- **Problem:** normal use still depends on `discover_all_chats` or a manual `allowed_chats` list («Allowed chats» in the GUI); there is no explicit «try / stay silent / leave it to the person» decision for each eligible message.
- **Proposal:** `discover_all_chats = true` by default with an observable migration; extend the intent classification with `attempt | ignore | human`; show the reason in GUI/CLI.

### P1 — Per-stage metrics in the audit
- **Problem:** the time of each call (Graph, tool, each model call) is not recorded; diagnosing latency or cost requires reproducing. An activity review takes about two minutes and it is unclear how much is Azure DevOps and how much the model.
- **Proposal:** durations and call counts in `Audit`; `pta audit stats` with percentiles per reason and source.

### P2 — Several tool sources for any answer
- **Status (2026-10-03):** activity reviews already read Azure DevOps activity, Wiki edits and own Teams messages together, with a shared context budget.
- **Problem:** other questions still read one tool; «what did I do this week and how is X configured according to the wiki» loses a part.
- **Proposal:** up to N authorized tools with a shared context budget and an overall deadline, keeping the reference registry per source.

### P2 — Routing less dependent on language
- **Problem:** `question_request`, `documentation_question`, `status_question`, `activity_review_request` and `work_mention` are lists of Spanish and English prefixes/words; new phrasings take the wrong path.
- **Proposal:** let the intent classification also return the capability sought (`documentation | activity | review | other`) when several tools exist, keeping the heuristics as a cheap shortcut and the closed set of IDs.

### P2 — Cheaper reference selection
- **Status (2026-10-03):** the model is asked only about references code did not see named, with short closed keys.
- **Problem:** an activity review still sends dozens of references to the model (about 25 s with Codex).
- **Proposal:** skip the call when every remaining reference is a registered work item the answer only summarizes, or batch the selection into the answer call.

### P2 — Register unlinked work from the chat
- **Problem:** an activity review ends asking in which work item to register each piece of work, but the app only reads Azure DevOps; the user links them by hand.
- **Proposal:** an explicit confirmation step in the personal chat («link PR #12 to #78») with a separate write credential (`vso.work_write`) used only to add artifact links, each write confirmed by the user and recorded in the audit; never from a model decision alone.

### P2 — Detect a manual answer from the user before sending
- **Problem:** if the user answers while the proposal is being written, the assistant sends anyway (known limit).
- **Proposal:** in the re-read before sending, list the chat's later messages and abort if there is a human one from the user.

### P3 — Full `missed` recovery
- A cursor per conversation to page beyond the recent page, keeping the policy of not answering old messages.

### P3 — Audit retention
- `jobs.audit` and `events` grow without limit. A configurable policy (e.g. 90 days) that keeps recent deduplication keys, and indexes `jobs(status, next_at)`.

## 3. Stack and dependencies

- **P2 — Cargo features for optional integrations.** `tiberius` (SQL Server), RabbitMQ and generic HTTP behind features (`sql-server`, `rabbitmq`), on by default if compatibility is wanted. Less build time and surface.
- **P2 — Versioned SQLite migrations** with `PRAGMA user_version` instead of only `CREATE TABLE IF NOT EXISTS`.
- **P3 — Shared Rust→JS types** (`ts-rs`/`specta`) so `ui/app.js` stays in sync with `Snapshot`/`Config`.

## 4. GUI

- **P1 — Sources of any type.** Status (2026-10-03): the Knowledge tab lists every source and edits description, topics, audiences and the enabled/external-processing switches; the test chat offers all of them. Adding tool sources and editing their parameters (`tool`) and `allowed_senders` is still missing.
- **P1 — Real operational state.** Status (2026-10-03): Home shows the assistant, active subscriptions, tunnel, account, mode, models, sources, setup and recent activity, and the sidebar flags errors/uncertain sends of the last 24 h. Each subscription's expiry and the personal chat's outputs pending reconciliation are still missing.
- **P2 — Tray icon with state** (not configured / stopped / running / needs attention) and a system notification on `uncertain`, a subscription failure or an expired login.
- **P2 — Start with the session** (an option off by default; `tauri-plugin-autostart`): reception depends on the host being alive. On Linux a systemd user service with `--headless --start` already does it.
- **P2 — A more informative test chat:** Status (2026-10-03): it renders Markdown and shows verified references, partial coverage, warnings and a readable reason. Testing without a selected source is still missing (`local_chat` requires one), and the test chat has no Microsoft session, so the Teams messages source reports itself unread there.
- **P3 — Audit viewer** with filters by state/reason and content only on explicit request (like `pta audit show --content`).

## 5. Security and operation

- **P1 — Stable code signing on macOS.** Each `cargo install` produces a binary without a stable signature; the Keychain may ask «Allow access» again (credentials are not lost). Signing locally with a stable own certificate after installing (or a `pta`-assisted script) keeps the ACL across versions.
- **P2 — Rate and concurrency limits on the public listener** (`tower` `ConcurrencyLimit`/`RateLimit` in `webhook::router`); today any origin that reaches the tunnel can force validations and database reads.
- **P2 — Configurable absolute path for `cloudflared`** instead of searching `/opt/homebrew`, `/usr/local` and `PATH` (`app.rs::start`).
- **P2 — Rotating local log** with the same fixed events `tracing` already emits, to diagnose after a host crash (today there is only the `events` table).
- **P3 — Guided rotation of `STATE_ENCRYPTION_KEY`** (re-encrypting `vault`) and of `GRAPH_WEBHOOK_SECRET` (recreating own subscriptions).

## 6. Quality and tests

- **P1 — Check the JavaScript before delivering** (`bun build desktop/ui/app.js --no-bundle` or `node --check` at least; there is no CI).
- **P2 — Host tests without Tauri** once operations are extracted (start/stop, revision, settings journal, identity and key protection).
- **P2 — Property tests** (`proptest`) for `teams::canonical_resource`, `knowledge` (paths) and `Redactor`.
- **P2 — End-to-end test on real Linux** of the headless host with systemd, a tunnel and a new question in Teams (there is no CI: unit tests run only locally).

## 7. Distribution

- **Publishing.** One crate, `personal-teams-assistant` (0.6.2: library, GUI/host and `pta`). The old `personal-teams-desktop` crate is obsolete. Review the package with `cargo package --list` and gitleaks before publishing. Publishing is manual (`cargo publish --locked` from `main`); GitHub Actions is disabled to avoid costs.
- **P2 — Linux with Secret Service** (when D-Bus exists) instead of the file store.
- **P2 — Working Windows:** build and test the tray, Credential Manager, ACLs and start/stop before announcing it (without CI nobody builds it).
