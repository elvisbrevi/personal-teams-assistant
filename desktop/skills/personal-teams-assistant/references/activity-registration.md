# Daily task record

Activities saves a daily list of existing Azure DevOps tasks in the local activity record.
It does not discover work from commits, pull requests, pipelines, Wiki edits, messages or
calls to propose tasks, and never creates, links or changes Azure tasks. No language model,
Microsoft session or write credential is needed for snapshots.

## Schedule and settings

The existing `activity_registration` section and credential names remain compatible.
`enabled` controls scheduled snapshots; manual `activity run` also works when it is off.
A daily schedule uses `at`; an interval schedule uses `interval_minutes`, `window_start`
and `window_end`. `weekdays` uses Monday=1 through Sunday=7. `time_zone` is `local` (the
host's zone) or an IANA name such as `America/Santiago`, including daylight-saving rules.
The host must stay open. Today's latest due slot runs once across restarts; missed past
slots are not replayed.

`sources` selects enabled Azure activity source IDs. An empty selection uses all enabled
Azure sources. Retired Wiki/Teams selections are ignored; at least one selected Azure
source must be enabled for scheduling. Reads use each catalog's user, configured projects
and read credential. A `*` catalog still means only projects of the connected user's teams.
Local reads do not grant Teams audiences or authorize external processing.

`daily_hours` (default 9) is advisory. A snapshot saves every matching task even below or
above that target; it never invents work or caps hours. `effort_field` identifies actual
hours (default `Microsoft.VSTS.Scheduling.CompletedWork`), including custom numeric fields.
`effort_fields` names another field for a work item type, such as bugs that keep their hours
in `Custom.EsfuerzoReal`; types not listed use `effort_field`. Type names compare without
case; the map replaces as a whole and is empty by default.
`auto_register`, `write_source`, `task_kind`, `done_state` and `extra_fields` are legacy
fields retained for profile compatibility. They cannot enable Azure writes. Task types
(the process task and bug categories, so bugs with actual hours count) come from the
project's process.

```sh
pta config set activity_registration.sources '["azure-devops-status"]'
pta config set activity_registration.time_zone '"America/Santiago"'
pta config set activity_registration.at '"18:30"'
pta config set activity_registration.effort_fields '{"Bug":"Custom.EsfuerzoReal"}'
pta config set activity_registration.enabled true
pta activity run
pta activity run 2026-10-14
pta --json activity status
pta --json activity history 2026-10-14
pta --json activity history 2026-10-01..2026-10-20
```

Runs return immediately. Poll `activity status` until `running=false`, then inspect the
record and the latest run's status/diagnostics. A failed run keeps a generic `error` and the
real cause in `detail` (the source and the read's message, such as a missing read credential
or an Azure HTTP status), which Activities shows as «Cause». Failed or interrupted reads preserve the
previous saved day; a successful re-run replaces that day instead of duplicating tasks.
Empty days are saved explicitly and remain different from days never read. Legacy confirmed
tasks remain readable. `activity pending` returns no new decisions and `activity resolve`
refuses task creation; `--context-stdin` is retired and non-empty context is rejected.

## Which tasks belong to a day

A task or bug belongs to the user/day when its actual hours grow while assigned to that user
inside the configured local day. Hours are the net effort delta for that day in the type's
effort field, rather than the item's accumulated hours. A day never holds a row of 0 h:
closing an item, creating it or commenting on it without new hours does not count as work.
A bug with tasks is not recorded itself, even with hours in its own field: its work is in
its tasks, which show the bug as their parent, so each hour counts once. A bug without tasks
counts like a task, with the hours of its own field. Historical assignments are checked: a
later edit or handover does not move work to the new day or owner. A task without a parent
HU can still appear. Title, state and description come from the last revision before the
day's end; full detail is a fresh read.

Bounds: 200 candidate tasks per project and 1,000 revisions per task, with an overall
110-second Azure read deadline. Reaching a bound or failing a project read fails the snapshot
and retains the previous record, rather than saving an apparently complete empty day.
For old dates with many later changes, narrow a manual tool query to a project.

## Read tools and screen

`list_azure_tasks` takes required `day` (`YYYY-MM-DD`) and optional `user` (Azure email/uniqueName), `source` and `project`. Without `user`, it uses the catalog's own identity.
It returns the day/time zone, `tasks`, total `hours`, advisory `target_hours`, and coverage
fields. Each task includes `item` (ID, title, canonical URL, project, type, state and dates),
`source`, organization, user, description, daily hours and optional parent item.

`get_azure_task` takes a positive integer `id`, optional `source` and `project`, and returns
the same complete, safe detail schema as `get_azure_hu`: fields, comments, relations and
history with explicit partial-coverage warnings. Both tools are read-only and available in
CLI and MCP. See [tools-and-todo.md](tools-and-todo.md).

The screen opens with recorded days, newest first, optionally filtered by From/To. Select a
day for its task list, then a task for the reused To-Do detail view. Task cards and detail
headers have a separate link icon opening Azure. Back restores list position and focus.
**Save a day's tasks**, folded at the top of the screen, allows a manual snapshot; **Schedule
settings** opens its settings. The schedule runs in the application's background service (on
the server for the portal), so the page and the app may stay closed.
The daily target is a warning and never blocks navigation or saving.

## Answers from the record

The record stays in `activity-registration.db`, with an additive `task_days` table. It is
local knowledge read by the existing `activity_record` source; the knowledge index does
not need to re-index it. Authorize that source for each conversation before Teams can use
it. A map without an explicit record source retains its Azure-source authorization fallback.
Recaps and task reports read the saved requested dates without live Azure activity reads:
concise tasks, daily hours and verified links, with an HU only when one is available.
A saved empty day says no tasks were recorded; an unsaved day does not prove no work occurred.
