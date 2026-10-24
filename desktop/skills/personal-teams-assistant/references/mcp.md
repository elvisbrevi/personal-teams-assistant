`pta mcp` is a Model Context Protocol server over stdio: an agent (Claude Code, Codex or any MCP client) operates the app with the same operations as the GUI and `pta`, through the host's IPC contract, so audiences, checks and records stay the host's own. It starts the host when needed and replaces one left from an update, keeping the assistant's state.

**Activate it.** `pta mcp config` prints the exact command for this installation. Claude Code: `claude mcp add --scope user personal-teams-assistant -- pta mcp` (then `/mcp` shows it). Codex: add to `~/.codex/config.toml`:

```toml
[mcp_servers.personal-teams-assistant]
command = "pta"
args = ["mcp"]
```

Other clients take `{"mcpServers":{"personal-teams-assistant":{"command":"pta","args":["mcp"]}}}`. Use the full path from `pta mcp config` when the client's `PATH` lacks Cargo's `bin`. A web-portal profile is selected with `PTA_PROFILE_DIR` in the server's environment.

**Tools.** The catalog is also available through `pta tools list` and `pta tools call NAME` (JSON on stdin). See [To-Do and shared tools](tools-and-todo.md) for filters and result schemas. Read: `list_azure_hus`, `list_github_epics`, `get_azure_hu`, `list_azure_tasks`, `get_azure_task`, `get_github_epic`, `open_work_item`, `status`, `messages_list`, `message_show`, `sources_list`, `contexts_list`, `cases_list`, `people_list`, `notes_search`, `knowledge_search`, `index_status`, `activity_status`, `activity_history`, `activity_settings`, `chat_simulate` (the real pipeline on a test question; never sends, uses model quota). Local and reversible: `note_save`/`note_remove`, `context_save`/`context_remove`, `case_update`/`case_reset`, `person_save`/`person_remove`, `answer_rate`, `answer_draft` (writes an answer to an unanswered or held message without sending it; follow it with `message_show`), `index_refresh`, `activity_run` (saves existing tasks locally), `activity_settings_update` (read-only schedule and advisory hours). Needing the user's confirmation: `answer_send`, `source_authorize` (enabled, external processing, conversations, senders), `assistant_mode` (observe/active) and `assistant_control` (start/stop/restart).

**Confirmation.** When the client supports MCP elicitation, the server itself asks the user to confirm each of those actions, whatever the agent sends; a declined or cancelled question does nothing. Otherwise the tool refuses until it is called with `confirmed: true`, which the agent may set only after the user approved that exact action in the conversation: say what will happen («enviar a Teams esta respuesta…», «autorizar esta fuente para esta conversación»), wait for an explicit yes, then call. Never batch confirmations or reuse one for another action.

**When to ask the user first.** A fact that is ambiguous, may be someone else's opinion or contradicts a note; which HU, how many hours or which day an activity belongs to; whether a call happened or what was done in it; which conversations may read a source; whether an answer should become an example (examples guide any chat). The server refuses secrets, emails, phone numbers and IDs in notes and comments; do not work around it.

**Workflows.**
- *Save knowledge:* `notes_search` and `knowledge_search` first; then `note_save` with `origin: "agent"`, the contexts it belongs to and keywords; a subject of its own is a context (`context_save` with its sources and instructions).
- *Improve answers:* read `messages_list` (ratings in `feedback`, `bad` ones first) and `message_show`; propose to the user a change to a case's instructions or keywords (`cases_list` shows the defaults), a context, a person profile or a note; apply it once they agree; check with `chat_simulate`.
- *Answer a message:* `answer_draft` with the user's context and sources, `message_show` until the answer is ready, show it to the user, `answer_send` only with their confirmation.
- *Read daily tasks:* `list_azure_tasks` with day and optional user/source/project; `get_azure_task` with ID for full detail. `activity_run` saves the catalog user's existing tasks locally; `activity_history` lists recorded days and the selected day's tasks. Schedule snapshots with `activity_settings_update`. No task creation or Azure writes are available.

Implementation tools and their confirmation requirements are described in [work item implementation](tools-and-todo.md#implement-work-items-and-inspect-agent-sessions). `implementation_runs` and `implementation_sessions` expose persistent native session IDs, work item associations, machine, state and start/finish times. Daily activity tools remain read-only.
