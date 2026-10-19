# Shared tools and To-Do

`pta tools list` returns the application's shared tool catalog, descriptions, JSON input schemas
and effects without starting a host. The same catalog is exposed by `pta mcp`. Existing tools
for messages, answers, sources, contexts, cases, people, verified notes, the knowledge index,
activities and assistant control live together with the To-Do tools. They use the same host
operations as their GUI sections and dedicated CLI commands.

`pta tools call NAME` reads a JSON object from stdin. Read and local tools run directly.
Tools with effects outside the app or that grant access require `confirmed: true`, only after
the user has approved the exact action. MCP retains its client-mediated confirmation flow.

## List the user's Azure DevOps HUs

```sh
printf '%s\n' '{}' | pta --json tools call list_azure_hus
printf '%s\n' '{"source":"azure-devops-status","state":"Developing","limit":25}' | pta --json tools call list_azure_hus
```

- `source`: an enabled Azure DevOps activity source ID. Optional when exactly one is enabled.
- `project`: optionally narrow to a project in that source's catalog.
- `state`: an exact state returned by the tool. Omit it for all states.
- `limit`: 1–100, default 10, newest creation date first across the selected projects.

The tool reuses the catalog's organization, projects, author identity and read credential.
`*` retains its meaning: projects of teams the connected user belongs to. It discovers the
process's requirement category (including custom HU types), all defined states, developer
identity fields and start/finish/target/closure date fields. It matches the catalog user in
any developer role, rather than the current responsible owner. Only processes without
developer identity fields fall back to `System.AssignedTo`. Returned assignments are checked
again after reading the items. Completed items are included; there is no activity time window.

Results include `items`, `states` (including states without matching items), `projects`,
`assignment_fields`, `has_more`, `partial` and `warnings`. Each item has `id`, `title`, a verified
provider `url`, `project`, `state`, `created_at` and `dates` with the original field reference,
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

## GUI

To-Do is a daily-work section. It calls `list_azure_hus` and `list_github_epics` directly through
the host contract, with provider, state and item-count selectors (10 by default). Multiple
enabled Azure sources add a source selector. It keeps state names from the provider and
shows missing dates explicitly. The existing sticker design and device theme are preserved.
On phones, four daily destinations plus More keep the floating bar at five targets; More
reveals Knowledge and Test chat, and closes on selection, outside click or Escape. Read failures and partial coverage appear inline. Changing
filters discards stale responses.

The link icon opens a new browser tab in the portal and the default browser in the native
app (`open_work_item`, also a CLI/MCP tool taking `{"url":"…"}`). The host accepts only canonical HTTPS Azure
work-item and GitHub issue links. These local reads work with Teams stopped, use no model,
and do not change any source audience or authorize external processing.
