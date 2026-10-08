# Activity registration

All functions use the same authenticated host operations from the CLI and GUI. No Teams message is sent by this process. Do not enable a schedule or run a write merely to inspect it: start with `pta config get activity_registration`, `pta activity status`, `pta activity pending` and `pta activity history YYYY-MM-DD`. The host and CLI must announce `activity_registration_support`; update both together for this schema.

## Configuration

`activity_registration` is additive and defaults to disabled. In Settings → Activity registration, or through `pta config set activity_registration.FIELD JSON`, choose:

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
pta activity run 2026-10-07          # optionally backfill one of the last 31 days
pta --json activity status           # running flag, today's entries and runs
pta --json activity pending          # all pending decisions, across days
pta --json activity history 2026-10-07
pta activity open                   # separate window with pending activities
pta activity open ENTRY_ID           # separate review of a particular activity
```

Runs and refinements are asynchronous: after the accepted operation, poll `activity status` until `running=false`, then inspect pending/history. Configuration changes and other mutations are refused while the process runs. The host stays open for the scheduler even when Teams reception is stopped. Closing the host stops scheduling; on its next start, today's latest due slot can run once, without replaying missed historical intervals. A slot and its activity claims persist across restarts.

Resolve an entry with JSON on stdin. `parent` is an HU ID, not a task ID. An HU explicitly named outside the suggestions is re-read in the authorized catalog's scope. Fields in `fields` extend the saved additional fields.

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

`refine` passes **Other / more context** to the model and updates the suggestion; it does not write. `create` confirms the exact task details. `dismiss` keeps the decision in history and suppresses the same activity on later runs. Provide only supported facts. After any timeout, refresh the entry instead of repeating a write.

## Evidence, calls and required fields

The process reviews all supported performed-work evidence from the selected sources: commits you authored, pull requests you created, pipeline runs you queued, releases you created, stage approvals you gave, Wiki pages you created or edited, work described in your own messages and call-ended events in authorized Teams chats (including meeting chats). A Wiki change mentioning an HU can still need a task; a verified task mention or existing evidence relation prevents duplication, and unresolved work item mentions require clarification. It keeps the Graph scopes already consented: **no calendar, attendance report or call-record permission** is added. A call event proves a call happened, not that you attended it. Without confirmed participation and purpose, it asks what the call covered and which HU it belongs to. It uses the event's duration as context, not as proof of your time. Generic messages are not tasks. Calendar-only meetings or calls without a readable chat event are outside this coverage; add their context to a pending activity.

Before creation, code re-reads the HU, verifies it is open and in scope, checks the daily effort budget, reads the task type's fields and Completed states, and performs Azure DevOps `validateOnly=true`. The one actual POST includes assignment to the catalog's author, the HU's area and iteration, parent and evidence relations, description and effort. Unavailable or required fields keep the entry pending; review its issue, supply extra field values or correct the saved completed state. Server-side conditional rules may require additional fields beyond the metadata.

History distinguishes `pending`, `refining`, `sending`, `done`, `dismissed`, `already_registered`, `failed` and `uncertain`. A real write is checkpointed before its request and never retried; a restart during creation becomes `uncertain`, and its effort remains reserved until inspected. Verify the task through its recorded URL. Backfilled tasks carry `PTA-activity:YYYY-MM-DD` in Tags when supported, so registering older work does not consume today's budget. Coverage limits and unread sources appear in the run summary. Native notifications depend on the OS notification settings; pending entries also remain visible in the app and CLI. A headless host records the same results without desktop notifications.
