# Shared tools and To-Do

`pta tools list` returns the application's shared tool catalog, descriptions, JSON input schemas
and effects without starting a host. The same catalog is exposed by `pta mcp`. Existing tools
for messages, answers, sources, contexts, cases, people, verified notes, the knowledge index,
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
- `kind`: `hu` or `bug`; omit for both requirement-category HUs and bug-category items.
- `limit`: 1–100, default 10, newest creation date first across the selected projects.

The tool reuses the catalog's organization, projects, author identity and read credential.
`*` retains its meaning: projects of teams the connected user belongs to. It discovers the
process's requirement and bug categories (including custom types), all defined states, developer
identity fields and start/finish/target/closure date fields. It matches the catalog user in
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
app (`open_work_item`, also a CLI/MCP tool taking `{"url":"…"}`). The host accepts only canonical HTTPS Azure
work-item and GitHub issue links. These local reads work with Teams stopped, use no model,
and do not change any source audience or authorize external processing.

Pressing an item opens its mobile detail inside To-Do through `get_azure_hu` or
`get_github_epic`. Back restores the list, filters, scroll and keyboard focus. Type-specific
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
