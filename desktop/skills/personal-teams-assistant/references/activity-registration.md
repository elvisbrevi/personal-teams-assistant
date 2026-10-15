# Activity registration

All functions use the same authenticated host operations from the CLI and GUI. No Teams message is sent by this process. Do not enable a schedule or run a write merely to inspect it: start with `pta config get activity_registration`, `pta activity status`, `pta activity pending` and `pta activity history YYYY-MM-DD`. The host and CLI must announce `activity_registration_support`; update both together for this schema.

## Configuration

`activity_registration` is additive and defaults to disabled. In Settings → Activity registration (workdays are toggles there), or through `pta config set activity_registration.FIELD JSON`, choose:

| Field | Meaning / default |
| --- | --- |
| `enabled` | Enable scheduled runs while the host is open; false. Manual `activity run` does not require it. |
| `auto_register` | Create complete, confident proposals automatically; true. False keeps every proposal for review. |
| `daily_hours` | Daily effort target in hours; 9. The process deducts work already registered, and never fabricates work or time to reach it. |
| `schedule` | `daily` or `interval`. |
| `at` | Daily local time, `18:00`. |
| `interval_minutes`, `window_start`, `window_end` | Every 60 minutes, anchored to `09:00`, through `18:00`. The window must not cross midnight. |
| `weekdays` | 1 = Monday … 7 = Sunday; `[1,2,3,4,5]`. |
| `time_zone` | `local` follows the computer, or an IANA zone such as `America/Santiago`. |
| `sources` | Explicit resource IDs: Azure DevOps activity (own commits, PRs, queued pipelines, created releases and stage approvals), own Wiki creation/edits and/or Teams work/calls. Sources must be enabled with `external_processing=true`. Selection authorizes local processing, without granting any new Teams audience. |
| `write_source` | An included Azure DevOps activity source with its own `link_secret_ref`; its catalog selects one organization. Read and write credentials stay separate. |
| `task_kind` | `Task` or `Tarea`; never an HU/backlog/feature type. |
| `done_state` | `Done`, checked against the project's actual `Completed` states. Use `Closed` or the localized name when the process requires it. |
| `effort_field` | `Microsoft.VSTS.Scheduling.CompletedWork`; must represent hours. A custom numeric field can be used. RemainingWork is zero when supported. |
| `extra_fields` | JSON object of additional task fields and scalar values, e.g. `{"Custom.ActivityType":"Documentation"}`. Protected identity/type/path/state fields cannot be overridden. |

Configure the sources and write credential through the existing [source commands](auth-and-knowledge.md). They start disabled and without external processing. Reuse saved settings and credentials; never expose their values. Change a whole section in one operation when required fields depend on one another, for example:

```sh
pta config set activity_registration.sources '["azure-devops-status","azure-devops-wikis","own-teams-messages"]'
pta config set activity_registration.write_source '"azure-devops-status"'
pta config set activity_registration.time_zone '"America/Santiago"'
pta config set activity_registration.schedule '"interval"'
pta config set activity_registration.interval_minutes 60
pta config set activity_registration.enabled true
```

`policy.dry_run=true` also prevents these Azure DevOps writes. Enabling `auto_register` in an active configuration authorizes scheduled creation when the plan is complete; an ambiguous HU, description, effort or call is always left pending. The personal-chat link-confirmation flow keeps its existing explicit confirmation.

## Run and resolve

```sh
pta activity run                     # starts today's review and returns its run ID
pta activity run 2026-10-07          # optionally backfill one of the last 14 days
pta activity run 2026-10-07 --context-stdin <<'CONTEXT'
I reviewed the payment guide for HU 4321 for one hour.
CONTEXT
pta --json activity status           # running flag, today's entries and runs
pta --json activity pending          # all pending decisions, across days
pta --json activity history 2026-10-07
pta activity open                   # separate window with pending activities
pta activity open ENTRY_ID           # separate review of a particular activity
```

Runs and refinements are asynchronous: after the accepted operation, poll `activity status` until `running=false`, then inspect pending/history. Configuration changes and other mutations are refused while the process runs. The host stays open for the scheduler even when Teams reception is stopped. Closing the host stops scheduling; on its next start, today's latest due slot can run once, without replaying missed historical intervals. A slot and its activity claims persist across restarts.

Manual runs accept up to 6000 bytes of non-sensitive context through `--context-stdin` or the GUI's **Context for the agents** field. Discovery matches explicit artifact links (including taskboard work item links and Wiki page ID links) and work item IDs against verified own activity in the selected sources. Mentioned builds and releases remain available to review even when related code is already linked to a work item; existing direct task links and saved decisions still prevent duplication. The existing configured provider chain receives this context as untrusted data for task planning. It stays with the run and its pending entries so later clarification retains it. Context does not replace verified evidence or change registration permissions; an authentication failure must be resolved before planning can run. A context that identifies no new verified activity gets a visible warning rather than silently finishing as if it was considered.

Automatically discovered activity belongs to the selected day. Explicitly mentioned activity is an exception: its artifact may be dated on another day. The review keeps that candidate, displays the artifact date and requested work day, and asks why the work belongs to the selected day. It remains pending even with automatic registration enabled. Fill **Why this work belongs to the selected day**, or supply `date_attribution` in CLI resolution JSON, to confirm that attribution. Ordinary refinement context does not confirm the date. The explanation is saved with the entry; the artifact timestamp and date are preserved, and repeated reviews on either day reuse its existing decision. Discovery retains its bounded lookback and existing source authorizations.

On a server, `local` means the server's time zone. For Chilean work days and scheduling, set `activity_registration.time_zone` to `"America/Santiago"`; this follows seasonal offset changes. The requested day and daily effort budget use that zone.

The panel shows the selected sources, provider order, daily target, creation mode and schedule. **Process runs** shows the last step, redacted failure details, duration, results and source/model coverage warnings. `activity history` returns the same additive `diagnostics` object, containing `stage`, `error`, `finished_at`, `context`, `provider`, `settings` and `observation_mode`. A run without a completed result has `summary: null`; progress remains visible without formatting unavailable effort totals, including interrupted runs with a legacy empty summary. Older runs explicitly lack failure details; their discarded errors cannot be recovered. **Copy diagnostic prompt** includes the recorded diagnostics and any additional context for use in Codex, Claude or another agent; it does not invoke a provider or register work.

The Activities tab is ordered as the work is done: (1) review a day, with the optional context and the day's effort against the target; (2) the activities that need input; the selected activity's form (what was detected, the assistant's question with **Answer the assistant**, then HU, title, effort, description and **Create task in Done**); (3) the day's activities; and the collapsed **Process runs and diagnostics**. On a phone the form replaces the list until **Back**. The form checks the HU, title, effort and description before asking the host, keeps what you typed if creation fails, shows progress and the result right above its buttons, and closes on success with a link to the created task. Dismissing asks for a second tap.

In the web portal, **Review** opens the selected pending activity in the same page and scrolls to its form; the **Review activities** notice on Home opens the first pending activity, and links open in a new tab. The native app keeps **Review in new window**. Opening either form only selects existing work; it does not run a review or create a task.

Resolve an entry with JSON on stdin. `parent` is an HU or bug ID, not a task ID. One explicitly named outside the suggestions is re-read in the authorized catalog's scope. The parent can be a user story, backlog item, Basic-process issue or a bug (defect, incident) in any state that is not closed, so an HU or bug in development or QA works; a task, feature, epic, impediment or a done, closed, resolved, removed or aborted item is not written to: the entry stays pending with the edited title, description and effort and an issue naming its type and state, and when that work item's parent can hold tasks it is offered and preselected for the next `create`. Suggestions also include your recent bugs. An ID outside the scope is reported as not found. Fields in `fields` extend the saved additional fields.

```sh
pta activity resolve ENTRY_ID <<'JSON'
{"action":"refine","context":"I attended this call for one hour. We reviewed the payment guide for HU 4321."}
JSON

pta activity resolve ENTRY_ID <<'JSON'
{"action":"create","parent":4321,"title":"Document the payment guide","description":"Created the payment flow guide and reviewed it with the team.","hours":1.5,"fields":{"Custom.ActivityType":"Documentation"}}
JSON

pta activity resolve ENTRY_ID <<'JSON'
{"action":"dismiss"}
JSON
```

For an explicitly selected artifact published on a different day:

```sh
pta activity resolve ENTRY_ID <<'JSON'
{"action":"refine","date_attribution":"I did the work before midnight and published the artifact afterward."}
JSON
```

This confirms the date attribution and updates the draft without creating a task.
Review the draft, HU, effort and required fields before the normal `create` action.

`refine` passes **Answer the assistant** (`context`) to the model and updates the suggestion; it does not write. `create` confirms the exact task details. `dismiss` keeps the decision in history and suppresses the same activity on later runs. Provide only supported facts. After any timeout, refresh the entry instead of repeating a write.

## Evidence, calls and required fields

Registration searches only the user's own activity in the last 14 days. Task/HU discovery is filtered in Azure DevOps to items assigned to, created by or last changed by the catalog author within that window. Parent HUs reached through those recent tasks, or explicitly named in verified activity or context, are read by ID for validation even when older. The two-week window supplies context; automatic discovery registers the selected day's verified work, while an explicitly mentioned other-day artifact needs confirmed date attribution. Effort is charged to the selected work day. Runs and creation confirmations reject selected work days outside the last 14 local dates. History and existing decisions remain available.

The process reviews all supported performed-work evidence from the selected sources: commits you authored, pull requests you created, pipeline runs you queued, releases you created, stage approvals you gave, Wiki pages you created or edited, work described in your own messages and call-ended events in authorized Teams chats (including meeting chats). A Wiki change mentioning an HU can still need a task; a verified task mention or existing evidence relation prevents duplication, and unresolved work item mentions require clarification. It keeps the Graph scopes already consented: **no calendar, attendance report or call-record permission** is added. A call event proves a call happened, not that you attended it. Without confirmed participation and purpose, it asks what the call covered and which HU it belongs to. It uses the event's duration as context, not as proof of your time. Generic messages are not tasks. Calendar-only meetings or calls without a readable chat event are outside this coverage; add their context to a pending activity.

Before creation, code re-reads the HU, verifies it is open and in scope, checks the daily effort budget, reads the task type's fields and Completed states, and checks the process rules with Azure DevOps `validateOnly=true` without writing. When the process only creates tasks in an initial state (for example «En espera»), the completed state's rules are read too: fields it requires become inputs (such as «Evidencias de finalización» or «URL Commit»): a link field is prefilled with the activity's Azure DevOps commit, pull request or run, and an evidence field with the agent's `draft.evidence` (the result and the artifacts by name, never chat content) plus those Azure DevOps links only; **Write it with the assistant** asks the agent for new evidence, and fields it requires empty (Remaining Work) are cleared when completing. The actual write is one POST, with assignment to the catalog's author, the HU's area and iteration, parent and evidence relations, description and effort, followed in such processes by one PATCH to the completed state. Neither is retried: a task created but not completed stays `uncertain` with its link and Azure DevOps' reason, to be completed by hand; other rule errors appear as Azure DevOps words them. Unavailable or required fields keep the entry pending; review its issue, supply extra field values or correct the saved completed state. The first attempt in a project stores the task type's required fields on the entry as `required` (reference, friendly name, allowed values, help text and Azure DevOps type), and the other pending activities with an HU in that project get the same list; the GUI shows one input per field (a list for allowed values, a number, a date or text). From the CLI, pass them in `fields` by reference name, e.g. `{"action":"create",…,"fields":{"Microsoft.VSTS.Common.Activity":"Development","Custom.Mes":"Octubre"}}`. Fields with a process default and the ones code sets (effort, remaining work, system fields) are never asked for. A field value may have up to 8000 characters (Azure DevOps keeps 255 in plain text fields); a refused resolution names its exact reason (too long, not a scalar, a reserved System field, sensitive data or context over 6000 bytes). `effort_field` must exist in the task type: Scrum and many custom processes have no `Microsoft.VSTS.Scheduling.CompletedWork`, and then the issue lists the type's hour fields; set the one your tasks use for real hours with `pta config set activity_registration.effort_field '"Custom.EsfuerzoReal"'` (it is also the field the daily total is read from). Issues, the fallback question and the drafted titles, descriptions and questions are written in `llm.language`; run stages and diagnostics stay in English for agents. An invalid edit (links or sensitive data in the description, effort outside 0.01–24 h, a title over 160 characters) is rejected with the exact reason and is not stored. Server-side conditional rules may require additional fields beyond the metadata.

History distinguishes `pending`, `refining`, `sending`, `done`, `dismissed`, `already_registered`, `failed` and `uncertain`. A real write is checkpointed before its request and never retried; a restart during creation becomes `uncertain`, and its effort remains reserved until inspected. Verify the task through its recorded URL. Backfilled tasks carry `PTA-activity:YYYY-MM-DD` in Tags when supported, so registering older work does not consume today's budget. Coverage limits and unread sources appear in the run summary. Native notifications depend on the OS notification settings; pending entries also remain visible in the app and CLI. A headless host records the same results without desktop notifications.
