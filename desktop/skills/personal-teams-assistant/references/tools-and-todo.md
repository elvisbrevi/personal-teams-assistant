# Shared tools and To-Do

`pta tools list` returns the application's shared tool catalog, descriptions, JSON input schemas
and effects without starting a host. The same catalog is exposed by `pta mcp`. Existing tools
for messages, answers, sources, contexts, questions, people, verified notes, the knowledge index,
activities and assistant control live together with the To-Do tools. They use the same host
operations as their GUI sections and dedicated CLI commands.

`pta tools call NAME` reads a JSON object from stdin. Read and local tools run directly.
Tools with effects outside the app or that grant access require `confirmed: true`, only after
the user has approved the exact action. MCP retains its client-mediated confirmation flow.

## List the user's Azure DevOps HUs and bugs

```sh
printf '%s\n' '{}' | pta --json tools call list_azure_hus
printf '%s\n' '{"source":"azure-devops-status","state":"Developing","limit":25}' | pta --json tools call list_azure_hus
```

- `source`: an enabled Azure DevOps activity source ID. Optional when exactly one is enabled.
- `project`: optionally narrow to a project in that source's catalog.
- `state`: an exact state returned by the tool. Omit it for all states.
- `kind`: `hu` or `bug`; omit for both requirement-category HUs and bug-category items. Bug results also include the task-category type `Bug Desarrollo`.
- `limit`: 1–100, default 10, newest creation date first across the selected projects.

The tool reuses the catalog's organization, projects, author identity and read credential.
`*` retains its meaning: projects of teams the connected user belongs to. It discovers the
process's requirement and bug categories (including custom types), plus `Bug Desarrollo` from the
task category, all defined states, developer identity fields and start/finish/target/closure date fields. It matches the catalog user in
any developer role, rather than the current responsible owner. Assignment is discovered separately
for each type: a bug without developer identity fields falls back to `System.AssignedTo` even
when the HU type has developer roles. Returned assignments are checked
again after reading the items. Completed items are included; there is no activity time window.

Results include `items`, `states` (including states without matching items), `projects`,
`assignment_fields`, `has_more`, `partial` and `warnings`. Each item has `id`, `title`, a verified
provider `url`, `project`, `kind` (the original type), `state`, `created_at` and `dates` with the original field reference,
display name and nullable value. Separate HU and development dates retain their own labels.
Metadata is cached for five minutes; items are read fresh, with only the requested fields.
Unavailable projects produce partial coverage and a visible warning, never an apparently
complete empty list. New or changed states become available after the metadata cache expires.

## List GitHub epics

```sh
printf '%s\n' '{"limit":10}' | pta --json tools call list_github_epics
printf '%s\n' '{"project":"example/work","state":"closed","limit":25}' | pta --json tools call list_github_epics
```

Uses the existing GitHub App connection and issues of type `Epic` assigned to its authenticated
user. It does not interpret labels as issue types. `project` optionally narrows to `OWNER/REPO`;
`state` is `open` or `closed`; `limit` is 1–100, default 10. Search runs on the server ordered by
creation descending. The tool verifies issue type, assignment and canonical issue links.
The GitHub App needs Issues read access to the selected repositories. No new OAuth flow or
Microsoft Graph scopes are introduced.

GitHub's issue API exposes creation and closure dates; an unavailable start date is null.
Project-board custom workflow states and dates are not issue states or dates and are not read
by this tool. Search truncation or incomplete results are reported. No dates are inferred
from body text, milestones, changes or labels.

## Read one HU, bug or epic

```sh
printf '%s\n' '{"id":4321}' | pta --json tools call get_azure_hu
printf '%s\n' '{"id":4410,"source":"azure-devops-status","project":"Payments"}' | pta --json tools call get_azure_hu
printf '%s\n' '{"id":42,"project":"example/work"}' | pta --json tools call get_github_epic
```

`get_azure_hu` requires a positive integer `id` and accepts optional `source` and `project`.
It reads only enabled activity sources and projects in their configured catalog (including
team discovery for `*`), verifies the returned ID, project and requirement/bug category,
and rejects ambiguous IDs across organizations. Detail reads are not restricted to current
assignment or open states. Select a source with one organization if an ID is ambiguous.

`get_github_epic` requires `id` (the issue number) and `project` (`OWNER/REPO`), because issue
numbers are local to their repository. It verifies the canonical URL, number and Epic type,
rejects pull requests and uses the existing GitHub connection and Issues read access.

Both return `item`, `fields` (reference, original display name, data type, original value and safe rich-text `content` tree for HTML),
`comments` (author, dates, text and format), `relations` (label, detail and safe URL,
optional resolved `item` in the shared list shape, and process-verified `task` flag),
`history`, `partial` and `warnings`. Azure retains all returned standard and custom fields:
Assigned To, developer roles, description, acceptance criteria, bug reproduction steps,
severity, evidence and process-specific data. Relations retain attachments, linked work
items and development artifacts; Git branches, PRs, commits and builds get verified links.
GitHub returns the description, assignees, author, labels, milestone, state details and dates,
comments, sub-issues and events. No fields or links are inferred from a model.

Reads are fresh and bounded: Azure up to 500 latest comments and the first 200 revisions;
GitHub up to 500 comments, 500 sub-issues and 200 events. Azure resolves at most 200 linked
work item summaries in configured projects. Unavailable sections and bounds produce explicit
warnings; a failed read is never presented as a complete empty discussion. Original links
remain available. No response-provided pagination URL receives a credential.

## GUI

To-Do is a daily-work section. It calls `list_azure_hus` and `list_github_epics` directly through
the host contract, with provider, type (HUs and bugs / HUs / Bugs), state and item-count selectors (10 by default). Multiple
enabled Azure sources add a source selector. It keeps state names from the provider and
shows missing dates explicitly. The existing sticker design and device theme are preserved.
On phones, four daily destinations plus More keep the floating bar at five targets; More
reveals Knowledge and Test chat, and closes on selection, outside click or Escape. Read failures and partial coverage appear inline. Changing
filters discards stale responses.

The link icon opens a new browser tab in the portal and the default browser in the native
app (`open_work_item`, also a CLI/MCP tool taking `{"url":"…"}`). Its adjacent copy icon copies
the work item's URL to the clipboard. The host accepts only canonical HTTPS Azure work-item
and GitHub issue links. These local reads work with Teams stopped, use no model, and do not
change any source audience or authorize external processing.

Pressing an item opens its mobile detail inside To-Do through `get_azure_hu`, `get_azure_task`
(for `Bug Desarrollo`) or `get_github_epic`. Card backgrounds and borders reinforce known states:
green for completed, blue for active/development, amber for review/testing/validation and gray for
not started; unknown states keep the default card style. Back restores the list, filters, scroll and keyboard focus. Type-specific
rich fields, comments and evidence stay readable; all original fields and change history
are folded. Rich HTML is parsed in the core into bounded safe trees and rebuilt from allowed elements,
without browser HTML parsing, scripts, original attributes or remote image loads; images and attachments open through their original links rather than
inline previews. The same device theme, touch targets and translation rules apply. Refresh,
inline errors, partial warnings and stale-response guards also apply to detail reads.

Links and evidence has a local work item type selector and an Other links option for artifacts
and attachments. Resolved Azure tasks reuse the saved-day task cards; pressing a task
opens the same `get_azure_task` detail as Activities, including custom process task types.
Back restores the preceding detail, filter, scroll and focus. Nested navigation works from
Activities as well. Plain-text Markdown and Markdown embedded in preformatted evidence use
the safe Markdown renderer, while explicit code remains code. Bare HTTPS URLs in comments
and rich text become validated links with the same browser/native behavior.
Other related work item types keep their original provider links and remain filterable.

## Existing Azure tasks by user and day

```sh
printf '%s\n' '{"day":"2026-10-14"}' | pta --json tools call list_azure_tasks
printf '%s\n' '{"day":"2026-10-14","user":"person@example.test","source":"azure-devops-status","project":"Payments"}' | pta --json tools call list_azure_tasks
printf '%s\n' '{"id":4502,"source":"azure-devops-status","project":"Payments"}' | pta --json tools call get_azure_task
```

`list_azure_tasks` requires an exact day and optionally selects an Azure email/uniqueName (`user`),
source and project. The configured catalog user is the default. It reads historical daily
hours and completions within the configured time zone, process task category and project
scope. `get_azure_task` reads one task with the same detail schema and safeguards as HU detail.
Neither writes to Azure. The Activities screen uses daily saved copies and reuses To-Do's
cards, detail rendering and original-link icon. See [daily task record](activity-registration.md)
for date attribution, limits, scheduling and saved history.

## Implement work items and inspect agent sessions

Requires host and CLI `implementation_support: true`. Daily task snapshots and all evidence
reads retain their read-only behavior. The separate implementation flow can create reviewed
children and run installed coding agents.

1. Read `machine_settings` and `machine_list`. The current coordinator defaults to itself as
   the single Teams machine. In Settings > Language models, name this machine and set its
   allowed directory roots. Isolated portal profiles remain restricted to their own repositories.
   The GUI uses a shared directory sheet for allowed roots and local checkout add/edit paths,
   with single or multiple selection, a leading `../` row, parent navigation and Apply/Cancel. `machine_browse`
   takes an optional absolute `path` and reads this host's directories for these choices;
   an empty path lists its filesystem root (the home drive on Windows). Isolated web
   profiles can browse only their own repositories. Browsing grants no execution access.
   Leave Connection portal empty on the coordinator; enter its HTTPS origin on another
   computer that you want to connect.
   In 0.9.13+, the account list is authoritative from desktop, web, CLI and MCP. After
   enrollment, verify the same machine ID from a second account client with `machine_list`;
   use `machine_detail` (`machine`) for trust, presence, last-seen time, last-known assistant
   state, roots and capabilities. An offline report is last-known, never proof that Teams
   stopped. Missing/unknown capabilities require a compatible target and a fresh poll.
   `machine_update` takes `machine`, optional `name` and/or `roots`: names can change offline,
   roots require an online compatible machine and explicit canonical directories. Empty roots
   grant no native execution access. `machine_directories` (`machine`, optional `path`) is
   the remote directory-sheet read; it grants no roots. Both GUIs expose Manage machine
   beside every row and use the shared sheet. Portal roots remain operator-controlled.
   `machine_revoke` (`machine`) revokes even an offline/selected remote machine, invalidating
   its credential and deselecting Teams without starting another assistant. The old lease
   still delays a successor. Inspect list/detail to verify `trust: revoked`; another enrollment
   requires fresh browser trust or operator-local pairing. The coordinator cannot revoke
   itself this way. No operation returns credential values; variable use is a future feature
   and must require the same live authorization. Revocation cannot retract data already
   received by an external program. CLI/MCP edits and revocation require confirmation.
   After any timeout/failure, refresh `machine_list` and `machine_detail` before a deliberate
   repeat. Never automatically replay start, send, implementation or root edits.
2. On another computer you own, stop its Teams assistant and sign in (0.9.12+, `machine_login_support`):
   Settings > Language models > Connect this machine with the coordinator's HTTPS origin, or
   `pta auth machine login URL [--no-browser]`, or the `machine_login` tool (`url`, optional
   `open`). Give the user only the returned approval URL and code; they sign in on any device
   and choose Trust this machine. Then `pta auth machine finish --wait` or `machine_login_finish`
   connects it (`machine_login_status` reads the state, `machine_login_cancel` withdraws it).
   You never receive the credential; trust grants no roots, Teams ownership or start. The
   origin is remembered (`machine_settings.coordinator_origin`). The operator-local pairing file
   remains available: on your coordinator run `pta web machines authorize MACHINE_ID --output PRIVATE_FILE`,
   securely transfer it and within ten minutes run `pta tools call machine_connect < PRIVATE_FILE`
   on that computer, then delete it. Both are single-use and machine/installation-bound;
   GitHub login or an API token cannot enroll machines. Each installation needs its own
   coordinator URL and hosting credentials. Details: [web-access.md](web-access.md#add-a-computer-you-own-by-signing-in-0912).
3. Select the single Teams machine in Settings > Microsoft Teams or `machine_teams`. The
   previous machine stops after losing its lease; handover waits up to 120 seconds before a
   new machine can start. Start the Teams assistant explicitly on the selected machine.
   Settings > Language models shows the same choice as a radio beside each machine;
   both lists also offer Start/Stop buttons. `machine_assistant` takes `machine`, `action`
   (`start`/`stop`) and CLI/MCP confirmation. It uses the target's ordinary assistant controls;
   Start still checks the selected Teams machine, lease, configuration and credentials.
   It does not change ownership or start automatically after choosing a radio. Refresh the
   machine list after any timeout; never repeat an uncertain command automatically.
   Machines report `assistant_phase` through authenticated presence polling (0.8.1 or later).
   Older workers without a report show an unknown state and disabled controls; an offline
   machine shows its last reported state. The portal and coordinator data stay where they are.
4. Use Implement, immediately before the original-link action on a To-Do card/shared detail,
   or call `implementation_prepare` with the verified item selection. Provide context, machine,
   multiple directories, installed CLI, model and effort from that machine's catalog. The
   directory sheet supports multiple selections across hierarchy navigation and an
   always-available add action. Cancel/Escape discards pending selections. Implementation
   browsing stays within the selected machine's allowed roots.
5. `implementation_start` on an HU/bug/Epic proposes children. Review/edit/select them and
   confirm `implementation_create`; no child is written just because a model proposed it.
   On a process task, Bug Desarrollo or ordinary GitHub issue, Start implements that item in
   the chosen directories. Changes remain local; the coding prompt does not authorize pushing,
   publishing, changing work items or sending Teams messages.

Every tool is available through `pta tools list`, `pta tools call NAME` (JSON on stdin) and MCP.
Examples use synthetic identifiers; obtain actual machine IDs and verified item details first:

```sh
printf '%s\n' '{}' | pta --json tools call machine_list
printf '%s\n' '{"url":"https://assistant.example.com"}' | pta --json tools call machine_login
printf '%s\n' '{}' | pta --json tools call machine_login_finish
pta --json tools call machine_connect < /private/path/pairing.json
printf '%s\n' '{"item":{"provider":"azure","id":7,"project":"Payments","source":"azure","task":true}}' | pta --json tools call implementation_prepare
printf '%s\n' '{"machine":"11111111-1111-4111-8111-111111111111","work_item_url":"https://dev.azure.com/example/Payments/_workitems/edit/7"}' | pta --json tools call implementation_runs
```

Selections contain `provider` (`azure`/`github`), positive `id`, `project`, optional Azure `source`
and `task` for a process task category. Code checks the category on a fresh read. GitHub
`list_github_issues` lists all assigned issues, and `get_github_issue` reads an ordinary issue;
Epic-only tools remain strict. GitHub sub-issues open their shared detail.

`implementation_start` additionally takes `machine`, `context`, `directories` (1–16 canonical
absolute paths), `provider` (`codex`/`claude`/`opencode`), `model` and `effort`. CLI/MCP require
confirmation for starting, stopping, enrollment, changing allowed roots/Teams ownership and
creating children. GUI button presses supply the corresponding user action; creating children
requires the second confirmation tap on the edited plan.

The private SQLite database `implementation/runs.db` records every execution with its run ID,
work item provider/project/ID/link, execution machine, agent/model/effort, real native `session_id`,
state, request time, actual start time and finish time. Session IDs are captured from CLI events
while the agent runs; a run ID is never substituted for an unavailable native session ID. Native
sessions persist in their CLI, including planning sessions. Successful implementations become
`done`; plans become `plan_ready`. Failures, blocked permissions, cancellations and interruptions
stay distinct. An interrupted run retains its session ID, with no invented finish time. The
coordinator receives durable session updates, including last-known records for offline machines.

`implementation_runs` filters by machine and optional run `id` or `work_item_url`, returns the
latest 100 matching executions within an 800 KB response budget and their session/timing/result fields; `partial` signals an omitted older result. Request a run by `id` to retrieve it individually. `implementation_sessions`
reads the account's session index for up to 100 canonical work item URLs, with up to three recent summaries per item, prioritizing active executions. Work item details show
agent sessions; cards offer a session action when a native session is recorded. The native
`implementation_open_session` operation constructs its URL from the recorded provider/session
and opens only a session on that desktop. A completed local Claude session can use the documented `claude --desktop --resume SESSION_ID` handoff on macOS/x64 Windows with a supported subscription and Claude Code 2.1.285 or later; running sessions cannot be moved. A verified Codex local session can use the documented
`codex://threads/<thread-id>` desktop link. Browser opening cannot confirm app installation;
a missing handler shows a reason. Mobile browsers offer the provider connection sheet for Codex/Claude. Unsupported providers
show an explanation instead of fabricating a cloud URL.
See [Codex links](https://learn.chatgpt.com/docs/reference/commands),
[Claude desktop links](https://support.claude.com/en/articles/14729294-open-claude-desktop-with-a-link),
[Claude mobile links](https://support.claude.com/en/articles/14898120-open-the-claude-mobile-app-with-a-link)
and [Claude Remote Control](https://code.claude.com/docs/en/remote-control).

Mobile/PWA session access uses the provider's own login on the execution machine.
New implementation/request sessions try to enable it automatically. Codex uses its shared Remote
Control daemon so the phone sees the same live thread. Claude publishes a real remote session
whose `session_…` ID differs from the local UUID. The retained Claude process serves mobile for
up to two hours (at most 16 per host); the original PTA run still ends with its initial result.
Internal Teams model calls remain tool-less and ephemeral without remote access.

Use `implementation_mobile_session` with `machine`, recorded run `id`, `action` (`connect`,
`pair`, `close`) and `confirmed: true`. `connect` returns verified `remote` metadata and can
resume a completed Claude session without generating another prompt; it revalidates saved roots.
A running Claude session can use its existing connection but cannot be resumed concurrently.
`pair` is for Codex and returns an expiring `pairing.manual_code` only to the authenticated caller;
never save it as knowledge or include it in logs. `close` terminates a retained Claude connection
after the initial run, preserving its transcript. Codex's shared daemon is managed in Codex.

On a phone, **Open agent session** opens the connection sheet. Claude offers the recorded remote
session in its native iOS/Android app and on the web, as separate real links so iOS preserves the
user gesture from an installed PWA. Codex shows
its connected computer and offers pairing: open Codex in the ChatGPT mobile app, select that
computer and then the conversation. A local Codex UUID has no verified direct mobile URL. Older `exec` sessions get a native mobile
copy with their history; its actual `remote.session_id` is persisted and reused while the original
local session stays recorded.
Do not confuse merged code, a connected provider host and a phone actually paired to that host.

Azure child creation uses the selected source's read catalog and the process's default task type,
with the parent's area/iteration and catalog user's assignment. Use
a valid Azure CLI session (`az login`) on the host performing the write, with the user's Work Items permissions;
legacy activity write credentials do not authorize this flow. GitHub needs Issues write access on
its existing App connection. Child creation is submitted to the coordinator, even from an enrolled worker, and runs in the background after its durable intent is saved. `implementation_creation` takes the plan's `id` and returns its current state and verified child links, or null before submission. Each new plan can be submitted once; repeated submissions only return its recorded outcome. A lost provider response is `uncertain`
and must be inspected in the provider without replaying the plan. A GitHub child created without
its parent link retains its verified URL as `created_unlinked`; review it before proceeding.
Cancellation stops the coding process tree and preserves its current file changes. Restarting a
host never relaunches an interrupted run or resubmits an uncertain child creation.

## Create a request and inspect agent history

Requires host/CLI 0.9.1 or later. In To-Do select Azure or GitHub, then Create request below the filters. Select a project or repository, supply context and select an installed CLI, model and effort. Azure offers HU and bug radios; GitHub creates an Epic. Cancel discards an unsubmitted form. Create explicitly authorizes generation and creation of this one request. The session runs on this host and is retained in the CLI; no coding directories or execution-machine selection are needed.

From 0.9.3, destination sheets start with a name filter and up to three recent destinations. Azure offers all accessible projects in the enabled source's configured organizations; its evidence/To-Do team catalog remains unchanged. GitHub offers the authenticated GitHub CLI user's owned public and private repositories using `gh repo list`, independently of the GitHub App repository list. The host checks catalog membership before generation and again before sending. Azure catalogs fail explicitly above 1,000 projects per organization, and GitHub at 100,000 repositories, instead of silently truncating.

Agents use the same operations through `pta tools call` or MCP:

1. `implementation_request_catalog` with `provider: azure|github` and, for Azure, `source`. It returns exact `targets`, up to three `recent` targets, CLI session/write readiness and available coding CLI providers/models/efforts. Keep a target as returned; arbitrary or edited targets are rejected.
2. After the user approves the context, target, type and agent, call `implementation_request_start` with a new stable UUID `id`, the exact `target`, `kind: epic|hu|bug`, `context`, CLI `provider`, `model`, `effort` and `confirmed: true`. Azure requires the authenticated Azure CLI session on this host; read credentials cannot create requests. GitHub uses the GitHub CLI session that supplied the repository list, including its own assignment; the GitHub App connection does not authorize this flow. The process defines the actual Azure work item type. Creation uses its To-Do developer assignment role (or Assigned To) and description/reproduction field.
3. Inspect `implementation_request_status` with `id` until generation/sending ends. Reusing that ID with the same submission only reads the saved record; another submission cannot reuse it. After any timeout inspect status, never start a replacement automatically. A failed/uncertain send is never retried. An uncertain request may already exist: inspect the provider first. If GitHub silently drops the Epic type or own assignment, `needs_attention` preserves its verified issue URL for review.
4. `implementation_history` takes no arguments and returns the latest 100 recorded local requests and local/cached implementation sessions, bounded to 800 KB; `partial` indicates bounded history. The GUI's Agent history screen is in navigation (More on phones) and To-Do. Run IDs belong to PTA; session IDs belong to the native coding CLI. Missing session IDs and finish times remain empty, including interrupted executions.
5. Use `implementation_open_session` with the recorded `machine` and run `id` on that machine's native desktop. The existing Codex/Claude platform limits apply; OpenCode and unsupported remote/mobile sessions retain their ID with an explanation rather than an invented app URL.

Daily task snapshots, activity runs and all evidence tools remain read-only. This is a distinct explicit request-creation action; it does not authorize autonomous work item creation by the assistant.

## Azure CLI authentication status

From 0.9.2, explicit Azure request and child creation reuse Azure CLI's Entra session instead of a separate PAT. Home lists Azure CLI status and its account. Select an unavailable session to start Microsoft device-code sign-in in the browser; the app shows the code and checks completion automatically. Select an authenticated session to refresh. A stored account whose Azure DevOps session cannot be renewed is unavailable rather than authenticated. The user still needs the project's Work Items permissions; a signed-in state alone does not guarantee them.

Read the shared operation with `printf '%s\n' '{}' | pta --json tools call azure_cli_status` (or MCP `azure_cli_status`). It returns state/authenticated/user/tenant only. The app acquires short-lived credentials internally with Azure CLI and never returns or prints them. Do not run token commands that print tokens. Use `pta auth azure login` to receive the public Microsoft verification URL and user code, complete sign-in in the browser, then `pta auth azure finish --wait` to verify the Azure DevOps session. `pta auth azure cancel` kills the pending login without logging out an existing session; `pta auth azure status` reads its state. Both current and legacy Microsoft verification prompts are supported; the returned public link stays compatible with older clients. Login failures include a safe phase and reason in `pta logs`. The equivalent shared CLI/MCP operations are `begin_azure_cli_login`, `finish_azure_cli_login` and `cancel_azure_cli_login`. Each login is bounded to ten minutes, accepts no terminal input and runs under the host profile. `az login` under the same OS user/profile remains available for manual sign-in. For an isolated web profile use its own `home/.azure`; never copy or reuse the operator's Azure session. The old `AZURE_IMPLEMENTATION_WRITE_TOKEN` remains untouched in storage if present and is retired; do not configure it for this flow. Daily snapshots and evidence remain read-only.

## GitHub CLI authentication status

From 0.9.3, Home lists GitHub CLI installation/authentication and its verified account login. Select its row to refresh, or call the shared read-only `github_cli_status` tool through CLI/MCP. It returns `state`, `authenticated` and `user` only, never credentials. Missing authentication requires `gh auth login` on the machine running the host under the same OS user/profile. Isolated web profiles use their own `home/.config/gh`; never reuse the operator's session. Authentication does not guarantee Issues write permissions. The existing GitHub App connection still supplies evidence, To-Do reads and checkout operations.
