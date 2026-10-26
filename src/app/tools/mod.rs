//! Shared application tools: one catalog and IPC adapters for CLI and MCP.
//! GUI sections call the same host operations; effects and confirmation stay explicit.
use super::{cli, control::Reply};
use serde_json::{Value, json};

pub(super) mod todo;

/// How a tool may run.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Effect {
    /// Reads only.
    Read,
    /// Changes the app's own data or profile; reversible.
    Local,
    /// Acts outside the app or grants access: needs the user's confirmation.
    External,
}

pub(super) struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub effect: Effect,
    pub schema: fn() -> Value,
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}
fn none() -> Value {
    object(json!({}), &[])
}
fn string(description: &str) -> Value {
    json!({"type": "string", "description": description})
}
fn strings(description: &str) -> Value {
    json!({"type": "array", "items": {"type": "string"}, "description": description})
}
fn confirmed() -> Value {
    json!({"type": "boolean", "description": "true only after the user approved this exact action (needed when the client cannot ask the user itself)."})
}

pub(super) const TOOLS: &[Tool] = &[
    Tool {
        name: "github_cli_status",
        title: "GitHub CLI session",
        description: "Read GitHub CLI installation and authentication for this profile. Returns the verified account login only, never tokens. Request creation uses this account independently of the GitHub App.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "begin_azure_cli_login",
        title: "Start Azure CLI sign-in",
        description: "Start Microsoft device-code sign-in on this profile's host. Return only the public verification page and user code; the user completes sign-in in their browser.",
        effect: Effect::Local,
        schema: none,
    },
    Tool {
        name: "finish_azure_cli_login",
        title: "Check Azure CLI sign-in",
        description: "Check pending device-code sign-in and verify the resulting Azure DevOps session. Returns only account metadata, never tokens.",
        effect: Effect::Local,
        schema: none,
    },
    Tool {
        name: "cancel_azure_cli_login",
        title: "Cancel Azure CLI sign-in",
        description: "Stop this profile's pending Azure CLI sign-in process. Does not log out an existing session.",
        effect: Effect::Local,
        schema: none,
    },
    Tool {
        name: "azure_cli_status",
        title: "Azure CLI session",
        description: "Read whether Azure CLI is installed and can obtain an Azure DevOps Entra session for this profile. Returns account and tenant metadata only, never tokens. Authentication does not prove project write permissions.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "implementation_request_catalog",
        title: "Request destinations and agents",
        description: "All accessible projects in the enabled Azure source organizations or public and private repositories owned by the GitHub CLI account, plus recent targets and installed CLIs, models and efforts on this host.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"provider":{"type":"string","enum":["azure","github"]},"source":string("Azure source ID.")}),
                &["provider"],
            )
        },
    },
    Tool {
        name: "implementation_request_start",
        title: "Create a request with an agent",
        description: "Generate and create one GitHub Epic or Azure HU/bug from the confirmed context and exact catalog target. Retains the native session. A stable UUID prevents repeated provider writes; inspect status after any timeout.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"id":string("Stable request UUID."),"target":{"type":"object","description":"Exact target returned by implementation_request_catalog."},"kind":{"type":"string","enum":["epic","hu","bug"]},"context":string("User context."),"provider":{"type":"string","enum":["codex","claude","opencode"]},"model":string("Available model ID."),"effort":string("Supported effort."),"confirmed":confirmed()}),
                &[
                    "id",
                    "target",
                    "kind",
                    "context",
                    "provider",
                    "model",
                    "effort",
                    "confirmed",
                ],
            )
        },
    },
    Tool {
        name: "implementation_request_status",
        title: "Request creation status",
        description: "Read a recorded request execution and its verified created item without retrying creation.",
        effect: Effect::Read,
        schema: || object(json!({"id":string("Request run UUID.")}), &["id"]),
    },
    Tool {
        name: "implementation_history",
        title: "Agent execution history",
        description: "Latest 100 local request runs and implementation sessions, including cached sessions from enrolled machines. Each result has its run ID, native session ID, model, effort, state and dates; partial reports bounded history.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "implementation_sessions",
        title: "Work item agent sessions",
        description: "Persisted native coding-agent session IDs, machines, states and start/finish times for verified work item URLs, including the last reported sessions of offline machines.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"urls":strings("Canonical work item URLs (up to 100).")}),
                &["urls"],
            )
        },
    },
    Tool {
        name: "implementation_open_session",
        title: "Open a native agent session",
        description: "Open a recorded Codex session or hand off a completed local Claude session on a supported desktop. URLs and CLI arguments use the recorded provider/session ID; arbitrary links are rejected. Unsupported apps or remote/local session mismatches return a reason.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"machine":string("This desktop's machine ID."),"id":string("Recorded run ID.")}),
                &["machine", "id"],
            )
        },
    },
    Tool {
        name: "implementation_mobile_session",
        title: "Connect an agent session on mobile",
        description: "Connect a recorded Codex or Claude session through its execution machine's own provider login. Claude returns its verified remote URL; Codex returns its connected host and can issue a short-lived manual pairing code. Close stops a retained Claude connection. Pairing codes are not saved.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"machine":string("Recorded execution machine ID."),"id":string("Recorded run ID."),"action":{"type":"string","enum":["connect","pair","close"]},"confirmed":confirmed()}),
                &["machine", "id", "confirmed"],
            )
        },
    },
    Tool {
        name: "list_github_issues",
        title: "My GitHub issues",
        description: "All issues assigned to the connected GitHub account, including epics. Pull requests are excluded.",
        effect: Effect::Read,
        schema: || {
            let mut schema = todo_schema();
            schema["properties"]
                .as_object_mut()
                .unwrap()
                .remove("source");
            schema["properties"].as_object_mut().unwrap().remove("kind");
            schema["properties"]["state"] = json!({"type":"string","enum":["open","closed"]});
            schema
        },
    },
    Tool {
        name: "machine_list",
        title: "Execution machines",
        description: "Online machines connected to this account and the single selected Teams machine.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "machine_settings",
        title: "This machine",
        description: "Name, coordinator and allowed directory roots of this host.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "machine_browse",
        title: "Choose local directories",
        description: "Browse this host's directories to configure allowed roots or local checkouts. Isolated web profiles remain confined to their own repositories. This read grants no execution access.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"path":string("Absolute directory; empty lists filesystem roots.")}),
                &[],
            )
        },
    },
    Tool {
        name: "machine_save",
        title: "Configure this machine",
        description: "Set this machine's name and allowed implementation roots.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"name":string("Machine name."),"roots":strings("Absolute allowed directory roots."),"confirmed":confirmed()}),
                &["name", "roots"],
            )
        },
    },
    Tool {
        name: "machine_connect",
        title: "Connect an execution machine",
        description: "Pair this machine with your own coordinator using a single-use private pairing file authorized locally there. Teams must be stopped first; GitHub tokens cannot enroll machines.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"url":string("Your coordinator HTTPS origin."),"pairing_token":string("Private single-use pairing credential; provide on stdin, never in arguments or chat."),"confirmed":confirmed()}),
                &["url", "pairing_token"],
            )
        },
    },
    Tool {
        name: "machine_disconnect",
        title: "Disconnect this machine",
        description: "Disconnect this host from its coordinator after stopping Teams.",
        effect: Effect::External,
        schema: || object(json!({"confirmed":confirmed()}), &[]),
    },
    Tool {
        name: "machine_assistant",
        title: "Control a machine's assistant",
        description: "Start or stop the Teams assistant on one enrolled machine. Start still requires Teams ownership and its lease; no ownership change is implied. Refresh machine status after an uncertain result, never retry automatically.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"machine":string("Online machine ID."),"action":{"type":"string","enum":["start","stop"]},"confirmed":confirmed()}),
                &["machine", "action"],
            )
        },
    },
    Tool {
        name: "machine_teams",
        title: "Choose the Teams machine",
        description: "Choose the one account machine allowed to receive and answer Teams. Handover waits for the old lease to expire.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"machine":string("Online machine ID."),"confirmed":confirmed()}),
                &["machine"],
            )
        },
    },
    Tool {
        name: "implementation_prepare",
        title: "Prepare work item implementation",
        description: "Read and verify an Azure HU/bug/task or GitHub Epic/issue before configuring its implementation.",
        effect: Effect::Read,
        schema: || object(json!({"item":implementation_item()}), &["item"]),
    },
    Tool {
        name: "implementation_catalog",
        title: "Machine coding agents",
        description: "Installed coding CLIs, models and efforts on the selected machine.",
        effect: Effect::Read,
        schema: || object(json!({"machine":string("Machine ID.")}), &["machine"]),
    },
    Tool {
        name: "implementation_browse",
        title: "Browse machine directories",
        description: "List directories under the selected machine's allowed roots. Paths are canonicalized and symlink escapes are rejected.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"machine":string("Machine ID."),"path":string("Absolute directory; empty lists allowed roots.")}),
                &["machine"],
            )
        },
    },
    Tool {
        name: "implementation_runs",
        title: "Implementation runs",
        description: "Durable run states, results and child plans on the selected machine.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"machine":string("Machine ID."),"id":string("Optional run ID."),"work_item_url":string("Optional canonical work item URL.")}),
                &["machine"],
            )
        },
    },
    Tool {
        name: "implementation_cancel",
        title: "Stop an implementation run",
        description: "Stop the selected coding run; retain its changes and record the cancellation.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"machine":string("Machine ID."),"id":string("Run ID."),"confirmed":confirmed()}),
                &["machine", "id"],
            )
        },
    },
    Tool {
        name: "implementation_start",
        title: "Start work item implementation",
        description: "HU/bug/Epic: propose children for review. Task/development bug/issue: run the selected coding agent in the selected directories. Never sends Teams messages or automatically publishes changes.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"machine":string("Machine ID."),"item":implementation_item(),"context":string("The user's implementation context."),"directories":strings("Selected absolute working directories."),"provider":{"type":"string","enum":["codex","claude","opencode"]},"model":string("Available model ID."),"effort":string("Supported model effort."),"confirmed":confirmed()}),
                &[
                    "machine",
                    "item",
                    "context",
                    "directories",
                    "provider",
                    "model",
                    "effort",
                ],
            )
        },
    },
    Tool {
        name: "implementation_creation",
        title: "Child creation outcome",
        description: "Read the coordinator's durable outcome of a reviewed child creation, including verified links. A sending or uncertain submission must never be replayed.",
        effect: Effect::Read,
        schema: || object(json!({"id":string("Plan run ID.")}), &["id"]),
    },
    Tool {
        name: "implementation_create",
        title: "Create reviewed child work items",
        description: "Create the explicitly selected and reviewed children of this new implementation plan. Azure requires its authenticated Azure CLI session; GitHub requires Issues write access. Never replays a submitted or uncertain plan.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"machine":string("Plan machine ID."),"id":string("Plan run ID."),"item":implementation_item(),"children":{"type":"array","items":object(json!({"title":string("Reviewed title."),"description":string("Reviewed description.")}), &["title","description"])},"confirmed":confirmed()}),
                &["machine", "id", "item", "children"],
            )
        },
    },
    Tool {
        name: "get_github_issue",
        title: "GitHub issue detail",
        description: "Read a verified GitHub issue, including sub-issues. Rejects pull requests and URL-directed credential use.",
        effect: Effect::Read,
        schema: || detail_schema(true),
    },
    Tool {
        name: "open_work_item",
        title: "Open a work item",
        description: "Opens a verified canonical Azure DevOps HU or GitHub issue URL in the native browser. A headless host returns the URL to open on another device. No item is changed.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"url":string("The canonical HTTPS item URL returned by a To-Do list tool.")}),
                &["url"],
            )
        },
    },
    Tool {
        name: "list_azure_hus",
        title: "My Azure DevOps HUs",
        description: "HUs and bugs assigned to the catalog user, newest first. Includes the task-category type Bug Desarrollo with bug results. Assignment roles are discovered separately for each type; kind optionally filters hu or bug. Discovers the process's developer roles, all HU states and start/finish fields, including separate development dates. Falls back to Assigned To only in processes without developer roles. Local read; no model or Teams audience changes.",
        effect: Effect::Read,
        schema: todo_schema,
    },
    Tool {
        name: "list_github_epics",
        title: "My GitHub epics",
        description: "Issues of type Epic assigned to the connected GitHub user, newest first. State is open or closed; project is an optional OWNER/REPO. Creation and completion dates are verified; an unavailable start date stays null. Uses the existing GitHub connection.",
        effect: Effect::Read,
        schema: || {
            let mut schema = todo_schema();
            schema["properties"]
                .as_object_mut()
                .unwrap()
                .remove("source");
            schema["properties"].as_object_mut().unwrap().remove("kind");
            schema["properties"]["state"] = json!({"type":"string","enum":["open","closed"]});
            schema
        },
    },
    Tool {
        name: "get_azure_hu",
        title: "Azure DevOps HU or bug detail",
        description: "Reads an HU or bug by ID within an enabled Azure activity source's configured projects. Includes all original/custom fields, Assigned To, acceptance criteria or reproduction steps, comments with authors, evidence, attachments, development links and history. Read-only; does not require Teams or a model. Select source/project when ambiguous.",
        effect: Effect::Read,
        schema: || detail_schema(false),
    },
    Tool {
        name: "list_azure_tasks",
        title: "Azure DevOps tasks by user and day",
        description: "Read-only tasks for a user on YYYY-MM-DD in the configured time zone. Uses daily effort changes or completion, historical assignments and process-defined task types. user defaults to the catalog user. Includes verified task/parent links, title, description and daily hours. Never creates tasks; needs no model or Teams session.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"day":string("YYYY-MM-DD."),"user":string("Azure user email (uniqueName); defaults to the catalog user."),"source":string("Enabled Azure source; optional when unambiguous."),"project":string("Configured project, optional.")}),
                &["day"],
            )
        },
    },
    Tool {
        name: "get_azure_task",
        title: "Azure DevOps task detail",
        description: "Reads an existing task by ID within an enabled Azure source's configured projects. Full description, assignment, fields, comments, links and history, using the same safe detail view as To-Do. Read-only.",
        effect: Effect::Read,
        schema: || detail_schema(false),
    },
    Tool {
        name: "get_github_epic",
        title: "GitHub epic detail",
        description: "Reads an Epic issue by its repository-local number (id) and required project OWNER/REPO, through the existing GitHub connection. Returns description, assignees, labels, milestone, dates, comments, sub-issues and history. Read-only; reports incomplete sections.",
        effect: Effect::Read,
        schema: || detail_schema(true),
    },
    Tool {
        name: "status",
        title: "Assistant status",
        description: "Whether the host and the assistant run, the mode (observe or active), the Microsoft account, Teams subscriptions and the version.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "messages_list",
        title: "Recent messages",
        description: "The latest Teams messages addressed to the assistant with their outcome, intent, answer (sent or proposed), sources, context, sender, person profile and rating.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "How many, newest first (default 20)."}}),
                &[],
            )
        },
    },
    Tool {
        name: "message_show",
        title: "One message",
        description: "One message with its full record: text, interpretation, evidence sources, references, answer and step log.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"resource": string("The message's resource, as messages_list gives it.")}),
                &["resource"],
            )
        },
    },
    Tool {
        name: "sources_list",
        title: "Sources",
        description: "Every source (files, URLs, repositories, notes, Wiki, Azure DevOps activity, Teams messages, activity record) with whether it is enabled, authorized for external processing and for which conversations.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "contexts_list",
        title: "Contexts",
        description: "The user's subjects (contexts): keywords, sources, description and instructions.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "questions_list",
        title: "Questions",
        description: "Configured questions, their sources and owner-associated requests. Unmatched messages stay unanswered for review.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "people_list",
        title: "People",
        description: "How the user addresses each person who writes to them.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "notes_search",
        title: "Search verified notes",
        description: "Verified notes, newest first, or by relevance to a query; optionally within a context.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"query": string("Words to search for; empty lists the newest."), "context": string("A context ID."), "limit": {"type": "integer", "minimum": 1, "maximum": 200}}),
                &[],
            )
        },
    },
    Tool {
        name: "knowledge_search",
        title: "Search the knowledge index",
        description: "The sections of repositories and notes the assistant would read for a request, without a model.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"query": string("The request."), "sources": strings("Source IDs to search (default: every repository and notes source)."), "context": string("A context ID."), "limit": {"type": "integer", "minimum": 1, "maximum": 50}}),
                &["query"],
            )
        },
    },
    Tool {
        name: "index_status",
        title: "Knowledge index status",
        description: "What each repository and notes source holds in the index, and whether semantic search is configured.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "activity_status",
        title: "Activity registration status",
        description: "The current activity review run, if any, and today's registration summary.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "activity_history",
        title: "Activity history",
        description: "Recorded days, with counts and hours; the selected day includes its saved task list. Supports an inclusive from/day range.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"day": string("YYYY-MM-DD (default today)."), "from": string("YYYY-MM-DD: with it, every day from here to `day`.")}),
                &[],
            )
        },
    },
    Tool {
        name: "activity_settings",
        title: "Activity registration settings",
        description: "Daily task snapshot settings: enabled, schedule, workdays, time zone, Azure sources, advisory daily hours and effort field. Legacy write settings are ignored.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "chat_simulate",
        title: "Try a question",
        description: "Runs the real pipeline on a test question and returns the answer; never sends to Teams. Uses the configured models' quota.",
        effect: Effect::Read,
        schema: || {
            object(
                json!({"text": string("The question, as a colleague would write it."), "sources": strings("Source IDs to read (enabled ones with external processing)."), "session": string("Keeps a test conversation's context across calls.")}),
                &["text"],
            )
        },
    },
    Tool {
        name: "note_save",
        title: "Save a verified note",
        description: "Creates a note (without id) or replaces one: a fact the user verified, with title, text, contexts and keywords. Origin defaults to agent.",
        effect: Effect::Local,
        schema: || {
            object(
                json!({"note": {"type": "object", "description": "id (to replace), title, text, contexts, keywords, origin (user|agent|answer|import), reference."}}),
                &["note"],
            )
        },
    },
    Tool {
        name: "note_remove",
        title: "Remove a note",
        description: "Removes a verified note.",
        effect: Effect::Local,
        schema: || object(json!({"id": string("The note ID.")}), &["id"]),
    },
    Tool {
        name: "context_save",
        title: "Save a context",
        description: "Adds or replaces a context (subject): id, name, description, keywords, sources (existing IDs) and instructions. It never authorizes a source.",
        effect: Effect::Local,
        schema: || {
            object(
                json!({"context": {"type": "object", "description": "id, name, description, keywords, sources, instructions."}}),
                &["context"],
            )
        },
    },
    Tool {
        name: "context_remove",
        title: "Remove a context",
        description: "Removes a context.",
        effect: Effect::Local,
        schema: || object(json!({"id": string("The context ID.")}), &["id"]),
    },
    Tool {
        name: "question_save",
        title: "Save a question",
        description: "Adds or replaces a configured question: id, text, sources (existing IDs), optional examples. Audiences are never broadened.",
        effect: Effect::Local,
        schema: || object(json!({"question":{"type":"object"}}), &["question"]),
    },
    Tool {
        name: "question_remove",
        title: "Remove a question",
        description: "Removes a configured question.",
        effect: Effect::Local,
        schema: || object(json!({"id":string("Question ID.")}), &["id"]),
    },
    Tool {
        name: "question_associate",
        title: "Associate a message with a question",
        description: "Associates an unanswered message with an existing question, or creates a new question with selected sources. Saves the message as an example in Questions; never sends an answer.",
        effect: Effect::Local,
        schema: || {
            object(
                json!({"resource":string("Message resource."),"question_id":string("Existing question ID; omit to create one."),"text":string("New question text."),"sources":strings("Source IDs for the question.")}),
                &["resource"],
            )
        },
    },
    Tool {
        name: "person_save",
        title: "Save a person",
        description: "Adds or replaces how the user addresses a person: id, name, senders (Teams user IDs), aliases, address, notes. Used only when answering that person.",
        effect: Effect::Local,
        schema: || {
            object(
                json!({"person": {"type": "object", "description": "id, name, senders, aliases, address, notes."}}),
                &["person"],
            )
        },
    },
    Tool {
        name: "person_remove",
        title: "Remove a person",
        description: "Removes a person profile.",
        effect: Effect::Local,
        schema: || object(json!({"id": string("The person ID.")}), &["id"]),
    },
    Tool {
        name: "answer_rate",
        title: "Rate an answer",
        description: "Rates an answer good or bad with a comment; ratings automatically guide answers to the matched question: good suggests its approach, bad rejects its mistakes. Facts always come from fresh evidence. null clears the rating.",
        effect: Effect::Local,
        schema: || {
            object(
                json!({"resource": string("The message's resource."), "rating": {"type": ["string", "null"], "enum": ["good", "bad", null]}, "comment": string("What was right or should change."), "example": {"type": "boolean"}, "context": string("A context ID for the example.")}),
                &["resource", "rating"],
            )
        },
    },
    Tool {
        name: "answer_draft",
        title: "Write an answer without sending it",
        description: "Writes (or rewrites) the answer to a message that was not answered, with the user's context and chosen sources, in the background; nothing is sent. Follow it with message_show.",
        effect: Effect::Local,
        schema: || {
            object(
                json!({"resource": string("The message's resource."), "context": string("The user's context and corrections (at most 4000 characters)."), "sources": strings("Source IDs to read (default: those of the conversation).")}),
                &["resource"],
            )
        },
    },
    Tool {
        name: "index_refresh",
        title: "Refresh the knowledge index",
        description: "Compares repository sources with their checkouts now (all, or one).",
        effect: Effect::Local,
        schema: || object(json!({"source": string("A repository source ID.")}), &[]),
    },
    Tool {
        name: "answer_send",
        title: "Send an answer to Teams",
        description: "Sends the written answer of a message once, as the user (never retried). Needs the user's confirmation.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"resource": string("The message's resource."), "confirmed": confirmed()}),
                &["resource"],
            )
        },
    },
    Tool {
        name: "activity_run",
        title: "Save a day's tasks",
        description: "Reads existing Azure tasks for the catalog user and saves the daily task record locally. No model, proposals or Azure writes. The hours target is only a warning.",
        effect: Effect::Local,
        schema: || {
            object(
                json!({"day": string("YYYY-MM-DD (default today; any past day).")}),
                &[],
            )
        },
    },
    Tool {
        name: "activity_settings_update",
        title: "Change activity registration",
        description: "Changes the read-only daily task schedule, time zone, sources, advisory daily hours and effort field.",
        effect: Effect::Local,
        schema: || {
            object(
                json!({"changes": {"type": "object", "description": "Fields of activity_registration to set, as activity_settings shows them."}}),
                &["changes"],
            )
        },
    },
    Tool {
        name: "source_authorize",
        title: "Authorize a source",
        description: "Enables or disables a source, its external processing (sending its content to the models) and the conversations and senders it may be read for. Needs the user's confirmation.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"id": string("The source ID."), "enabled": {"type": "boolean"}, "external_processing": {"type": "boolean"}, "allowed_conversations": strings("Exact conversation IDs (chats/…) or *."), "allowed_senders": strings("Graph user IDs; empty = anyone in those conversations."), "confirmed": confirmed()}),
                &["id"],
            )
        },
    },
    Tool {
        name: "assistant_mode",
        title: "Observe or answer",
        description: "observe writes answers without sending them; active answers in Teams. Needs the user's confirmation.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"mode": {"type": "string", "enum": ["observe", "active"]}, "confirmed": confirmed()}),
                &["mode"],
            )
        },
    },
    Tool {
        name: "assistant_control",
        title: "Start, stop or restart the assistant",
        description: "Starts, stops or restarts the assistant's Teams service. Needs the user's confirmation.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"action": {"type": "string", "enum": ["start", "stop", "restart"]}, "confirmed": confirmed()}),
                &["action"],
            )
        },
    },
];

fn todo_schema() -> Value {
    object(
        json!({
            "source": string("Azure DevOps activity source ID; optional when exactly one is enabled."),
            "project": string("One configured Azure project, or GitHub OWNER/REPO."),
            "kind": {"type":"string", "enum":["hu","bug"], "description":"Azure only: omit for HUs, bugs and Bug Desarrollo tasks."},
            "state": string("Exact state returned by the tool; omit to include all states."),
            "limit": {"type":"integer","minimum":1,"maximum":100,"default":10}
        }),
        &[],
    )
}
fn implementation_item() -> Value {
    object(
        json!({"provider":{"type":"string","enum":["azure","github"]},"id":{"type":"integer","minimum":1},"project":string("Azure project or GitHub OWNER/REPO."),"source":string("Enabled Azure source ID."),"task":{"type":"boolean","description":"Azure task-category item (including Bug Desarrollo)."}}),
        &["provider", "id", "project"],
    )
}

fn detail_schema(github: bool) -> Value {
    let mut properties = json!({"id":{"type":"integer","minimum":1,"maximum":2147483647}, "project":string("Azure project, or required GitHub OWNER/REPO; issue numbers are local to their repository.")});
    if !github {
        properties["source"] =
            string("Enabled Azure activity source ID; optional when exactly one is enabled.");
    }
    object(
        properties,
        if github { &["id", "project"] } else { &["id"] },
    )
}

pub(super) fn catalog() -> Value {
    json!(TOOLS.iter().map(|tool| json!({"name":tool.name,"title":tool.title,"description":tool.description,
        "inputSchema":(tool.schema)(),"effect":match tool.effect { Effect::Read => "read", Effect::Local => "local", Effect::External => "needs confirmation" }
    })).collect::<Vec<_>>())
}

/// Reject unknown tool names and arguments before contacting or starting a host.
pub(super) fn validate(name: &str, args: &Value) -> Result<(), String> {
    let tool = TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .ok_or("Unknown tool.")?;
    let args = args
        .as_object()
        .ok_or("Tool arguments must be an object.")?;
    let schema = (tool.schema)();
    let properties = schema["properties"]
        .as_object()
        .ok_or("Invalid tool schema.")?;
    for key in args.keys() {
        if !properties.contains_key(key) {
            return Err(format!("Unknown tool argument: {key}"));
        }
        if let Some(allowed) = properties[key]["enum"].as_array()
            && !allowed.contains(&args[key])
        {
            return Err(format!("Invalid tool argument: {key}"));
        }
    }
    for required in schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !args.contains_key(required) {
            return Err(format!("Missing tool argument: {required}"));
        }
    }
    if matches!(
        name,
        "list_azure_hus" | "list_github_epics" | "list_github_issues"
    ) {
        let filters: crate::tools::todo::Filters =
            serde_json::from_value(json!(args)).map_err(|_| "Invalid To-Do filters.")?;
        filters.validate().map_err(|e| e.to_string())?;
    }
    if name == "list_azure_tasks" {
        let query: crate::tools::tasks::Query =
            serde_json::from_value(json!(args)).map_err(|_| "Invalid daily task arguments.")?;
        query.validate().map_err(|e| e.to_string())?;
    }
    if matches!(
        name,
        "get_azure_hu" | "get_azure_task" | "get_github_epic" | "get_github_issue"
    ) {
        let detail: crate::tools::todo::DetailRequest =
            serde_json::from_value(json!(args)).map_err(|_| "Invalid work item arguments.")?;
        detail
            .validate(name.starts_with("get_github_"))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The value of a successful reply, or its message as the tool's error.
fn data(reply: anyhow::Result<Reply>) -> Result<Value, String> {
    let reply = reply.map_err(|_| "The host could not be reached; check pta status.".to_owned())?;
    if reply.ok {
        Ok(reply.data)
    } else {
        Err(reply
            .message
            .unwrap_or_else(|| format!("The operation failed ({}).", reply.code)))
    }
}
async fn ipc(method: &str, args: Value) -> Result<Value, String> {
    data(cli::call(method, args, None, true).await)
}
/// Change the profile through the host: read a snapshot, apply `change` to its configuration
/// and map, and save them with the snapshot's revision (a concurrent edit is refused).
async fn edit(change: impl FnOnce(&mut Value) -> Result<(), String>) -> Result<Value, String> {
    let snap = cli::snapshot()
        .await
        .map_err(|_| "The host could not be reached; check pta status.".to_owned())?;
    if !snap.ok {
        return Err(snap
            .message
            .unwrap_or_else(|| "The profile could not be read.".into()));
    }
    let mut profile = json!({"config": snap.data["config"], "map": snap.data["map"], "tunnel_config": snap.data["tunnel_config"]});
    change(&mut profile)?;
    data(cli::call("save_settings", profile, snap.revision, true).await)?;
    Ok(json!({"saved": true}))
}
/// A list of the map (`contexts`, `people`) with `item` added or replaced by its `id`.
fn upsert(profile: &mut Value, list: &str, item: Value) -> Result<(), String> {
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("An id is required.")?
        .to_owned();
    let map = profile["map"].as_object_mut().ok_or("Invalid profile.")?;
    let items = map.entry(list).or_insert_with(|| json!([]));
    let items = items.as_array_mut().ok_or("Invalid profile.")?;
    items.retain(|i| i["id"] != id.as_str());
    items.push(item);
    Ok(())
}
fn remove(profile: &mut Value, list: &str, id: &str) -> Result<(), String> {
    let items = profile["map"][list]
        .as_array_mut()
        .ok_or_else(|| format!("Unknown {list} entry."))?;
    let before = items.len();
    items.retain(|i| i["id"] != id);
    if items.len() == before {
        return Err(format!("Unknown {list} entry."));
    }
    Ok(())
}
fn text<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{name} is required."))
}

/// What a tool does, run against the host.
pub(super) async fn run(name: &str, args: &Value) -> Result<Value, String> {
    validate(name, args)?;
    if super::implementation::handles(name) {
        let mut args = args.clone();
        if ![
            "implementation_create",
            "implementation_request_start",
            "implementation_mobile_session",
        ]
        .contains(&name)
        {
            args.as_object_mut().unwrap().remove("confirmed");
        }
        return ipc(name, args).await;
    }
    match name {
        "list_azure_hus" | "list_azure_tasks" | "list_github_epics" | "list_github_issues" | "get_azure_hu" | "get_azure_task" | "get_github_epic" | "get_github_issue" | "open_work_item" => ipc(name, args.clone()).await,
        "azure_cli_status" | "github_cli_status" | "begin_azure_cli_login" | "cancel_azure_cli_login" => ipc(name, args.clone()).await,
        "finish_azure_cli_login" => {
            let reply = cli::call(name, args.clone(), None, true).await.map_err(|_| "The host could not be reached; check pta status.".to_string())?;
            if reply.code == "authorization_pending" { Ok(reply.data) } else { data(Ok(reply)) }
        }
        "status" => {
            let s = ipc("snapshot", json!({})).await?;
            Ok(json!({
                "host_version": s["host_version"], "host_running": s["host_running"], "headless": s["headless"],
                "assistant_running": s["running"], "mode": if s["config"]["policy"]["dry_run"] == true { "observe" } else { "active" },
                "microsoft_connected": s["microsoft_connected"], "active_subscriptions": s["active_subscriptions"],
                "subscription_issue": s["subscription_issue"], "tunnel_running": s["tunnel_running"],
            }))
        }
        "messages_list" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100);
            let rows = ipc("audit", json!({"limit": limit, "resource": null, "content": true})).await?;
            let rows = rows.as_array().cloned().unwrap_or_default();
            Ok(json!(rows.iter().map(|r| {
                let a = &r["audit"];
                let answer = if a["sent"].is_null() { &a["proposed"] } else { &a["sent"] };
                json!({"resource": r["resource"], "status": r["status"], "created_at": r["created_at"], "reason": a["reason"],
                    "intent": a["intent"], "question": a["question"], "answer": answer,
                    "sent": !a["sent"].is_null(), "source": a["source"], "context": a["context"], "sender_name": a["sender_name"],
                    "person": a["person"], "feedback": a["feedback"], "identified_question": a["identified_question"], "available_sources": a["available_sources"], "selected_sources": a["selected_sources"]})
            }).collect::<Vec<_>>()))
        }
        "message_show" => {
            let rows = ipc("audit", json!({"limit": 1, "resource": text(args, "resource")?, "content": true})).await?;
            rows.get(0).cloned().ok_or_else(|| "Unknown message.".into())
        }
        "sources_list" => Ok(ipc("snapshot", json!({})).await?["map"]["resources"].clone()),
        "contexts_list" => Ok(ipc("snapshot", json!({})).await?["map"]["contexts"].clone()),
        "people_list" => Ok(ipc("snapshot", json!({})).await?["map"]["people"].clone()),
        "questions_list" => Ok(ipc("snapshot", json!({})).await?["map"]["questions"].clone()),
        "question_save" => {
            let question = args.get("question").cloned().filter(Value::is_object).ok_or("question must be an object.")?;
            edit(|p| upsert(p, "questions", question)).await
        }
        "question_remove" => {
            let id = text(args, "id")?.to_owned();
            edit(|p| remove(p, "questions", &id)).await
        }
        "question_associate" => ipc("question_associate", args.clone()).await,
        "cases_list" => {
            let s = ipc("snapshot", json!({})).await?;
            Ok(json!({"built_in": s["cases"], "changes": s["map"]["cases"]}))
        }
        "notes_search" => ipc("notes_list", json!({"query": args.get("query"), "context": args.get("context"), "limit": args.get("limit")})).await,
        "knowledge_search" => ipc("knowledge_search", json!({"query": text(args, "query")?, "sources": args.get("sources"), "context": args.get("context"), "limit": args.get("limit")})).await,
        "index_status" => ipc("index_status", json!({})).await,
        "activity_status" => ipc("activity_status", json!({})).await,
        "activity_history" => ipc("activity_history", json!({"day": args.get("day"), "from": args.get("from")})).await,
        "activity_settings" => Ok(ipc("snapshot", json!({})).await?["config"]["activity_registration"].clone()),
        "chat_simulate" => {
            let input = json!({"session": args.get("session").and_then(Value::as_str).unwrap_or("mcp"), "text": text(args, "text")?,
                "group": false, "mentioned": false, "sources": args.get("sources").cloned().unwrap_or(json!([]))});
            ipc("chat", json!({"input": input})).await
        }
        "note_save" => {
            let mut note = args.get("note").cloned().filter(Value::is_object).ok_or("note must be an object.")?;
            if note.get("origin").is_none() {
                note["origin"] = json!("agent");
            }
            ipc("notes_save", json!({"note": note})).await
        }
        "note_remove" => ipc("notes_remove", json!({"id": text(args, "id")?})).await,
        "context_save" => {
            let context = args.get("context").cloned().filter(Value::is_object).ok_or("context must be an object.")?;
            edit(|p| upsert(p, "contexts", context)).await
        }
        "context_remove" => {
            let id = text(args, "id")?.to_owned();
            edit(|p| remove(p, "contexts", &id)).await
        }
        "case_update" => {
            let id = text(args, "id")?.to_owned();
            let mut change = args.get("change").cloned().filter(Value::is_object).ok_or("change must be an object.")?;
            change["id"] = json!(id);
            edit(|p| upsert(p, "cases", change)).await
        }
        "case_reset" => {
            let id = text(args, "id")?.to_owned();
            edit(|p| {
                if let Some(cases) = p["map"]["cases"].as_array_mut() {
                    cases.retain(|c| c["id"] != id.as_str());
                }
                Ok(())
            })
            .await
        }
        "person_save" => {
            let person = args.get("person").cloned().filter(Value::is_object).ok_or("person must be an object.")?;
            edit(|p| upsert(p, "people", person)).await
        }
        "person_remove" => {
            let id = text(args, "id")?.to_owned();
            edit(|p| remove(p, "people", &id)).await
        }
        "answer_rate" => ipc("answer_feedback", json!({"resource": text(args, "resource")?, "rating": args.get("rating"),
            "comment": args.get("comment"), "example": args.get("example"), "context": args.get("context")})).await,
        "answer_draft" => ipc("answer_refine", json!({"resource": text(args, "resource")?, "context": args.get("context").and_then(Value::as_str).unwrap_or(""),
            "sources": args.get("sources").cloned().unwrap_or(json!([]))})).await,
        "index_refresh" => ipc("index_refresh", json!({"source": args.get("source")})).await,
        "answer_send" => ipc("answer_send", json!({"resource": text(args, "resource")?})).await,
        "activity_run" => ipc("activity_run", json!({"day": args.get("day")})).await,
        "activity_settings_update" => {
            let changes = args.get("changes").and_then(Value::as_object).ok_or("changes must be an object.")?.clone();
            edit(|p| {
                let settings = p["config"]["activity_registration"].as_object_mut().ok_or("Invalid profile.")?;
                for (key, value) in changes {
                    settings.insert(key, value);
                }
                Ok(())
            })
            .await
        }
        "source_authorize" => {
            let id = text(args, "id")?.to_owned();
            let fields: Vec<(String, Value)> = ["enabled", "external_processing", "allowed_conversations", "allowed_senders"]
                .iter()
                .filter_map(|k| args.get(*k).map(|v| ((*k).to_owned(), v.clone())))
                .collect();
            if fields.is_empty() {
                return Err("Say what to change: enabled, external_processing, allowed_conversations or allowed_senders.".into());
            }
            edit(|p| {
                let source = p["map"]["resources"]
                    .as_array_mut()
                    .and_then(|r| r.iter_mut().find(|r| r["id"] == id.as_str()))
                    .ok_or("Unknown source.")?;
                for (key, value) in fields {
                    source[key] = value;
                }
                Ok(())
            })
            .await
        }
        "assistant_mode" => {
            let observe = match text(args, "mode")? {
                "observe" => true,
                "active" => false,
                _ => return Err("mode is observe or active.".into()),
            };
            edit(|p| {
                p["config"]["policy"]["dry_run"] = json!(observe);
                Ok(())
            })
            .await
        }
        "assistant_control" => match text(args, "action")? {
            "start" => ipc("start_assistant", json!({})).await,
            "stop" => ipc("stop_assistant", json!({})).await,
            "restart" => ipc("restart", json!({})).await,
            _ => Err("action is start, stop or restart.".into()),
        },
        _ => Err("Unknown tool.".into()),
    }
}

/// The user's confirmation for an action with effects outside the app, in words they read.
pub(super) fn confirmation(name: &str, args: &Value) -> Option<String> {
    Some(match name {
        "implementation_mobile_session" => "Connect or manage this recorded agent session in your provider account for mobile access?".into(),
        "implementation_request_start" => format!(
            "Generate and create one {} in {} using the supplied context and selected agent?",
            args["kind"], args["target"]["project"]
        ),
        "implementation_start" => format!(
            "Start the selected coding agent for item {} on machine {} in the selected directories?",
            args["item"]["id"], args["machine"]
        ),
        "implementation_create" => {
            "Create the reviewed children of this implementation plan in Azure DevOps or GitHub?"
                .into()
        }
        "implementation_cancel" => {
            "Stop this implementation run, retaining its current file changes?".into()
        }
        "machine_connect" => {
            format!(
                "Register this machine with coordinator {} for this account?",
                args["url"].as_str().unwrap_or_default()
            )
        }
        "machine_disconnect" => "Disconnect this machine from its coordinator?".into(),
        "machine_assistant" => format!(
            "{} the Teams assistant on machine {}?",
            args["action"].as_str().unwrap_or_default(),
            args["machine"].as_str().unwrap_or_default()
        ),
        "machine_teams" => {
            "Select this account's only machine for Teams reading and replies?".into()
        }
        "machine_save" => {
            "Save this machine's name and allowed implementation directory roots?".into()
        }
        "answer_send" => format!(
            "Send the written answer of {} to Teams, as you?",
            args["resource"].as_str().unwrap_or("this message")
        ),
        "source_authorize" => format!(
            "Change who may read the source {}: {}?",
            args["id"].as_str().unwrap_or(""),
            json!({"enabled": args.get("enabled"), "external_processing": args.get("external_processing"),
                "allowed_conversations": args.get("allowed_conversations"), "allowed_senders": args.get("allowed_senders")})
        ),
        "assistant_mode" => format!(
            "Switch the assistant to {} mode?",
            args["mode"].as_str().unwrap_or("")
        ),
        "assistant_control" => format!(
            "{} the assistant?",
            args["action"].as_str().unwrap_or("Change")
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_calls_are_closed_before_contacting_a_host() {
        assert!(
            validate(
                "list_azure_tasks",
                &json!({"day":"2026-10-14","user":"person@example.test"})
            )
            .is_ok()
        );
        assert!(validate("list_azure_tasks", &json!({"day":"yesterday"})).is_err());
        assert!(
            validate(
                "list_azure_tasks",
                &json!({"day":"2026-10-14","organization":"evil"})
            )
            .is_err()
        );
        assert!(validate("get_azure_task", &json!({"id":7})).is_ok());
        assert!(validate("get_azure_task", &json!({"id":0})).is_err());

        assert!(validate("get_azure_hu", &json!({"id": 7})).is_ok());
        assert!(validate("get_azure_hu", &json!({"id": 0})).is_err());
        assert!(validate("get_azure_hu", &json!({"id": "7"})).is_err());
        assert!(validate("get_github_epic", &json!({"id": 42})).is_err());
        assert!(
            validate(
                "get_github_epic",
                &json!({"id": 42, "project":"example/work"})
            )
            .is_ok()
        );
        assert!(
            validate(
                "get_github_epic",
                &json!({"id": 42, "project":"example/.."})
            )
            .is_err()
        );
        assert!(validate("list_azure_hus", &json!({"kind":"bug"})).is_ok());
        assert!(validate("list_azure_hus", &json!({"kind":"Task"})).is_err());
        assert!(validate("list_github_epics", &json!({"kind":"bug"})).is_err());
        assert!(validate("list_azure_hus", &json!({})).is_ok());
        assert!(validate("list_azure_hus", &json!({"limit":101})).is_err());
        assert!(validate("list_azure_hus", &json!({"limit":"10"})).is_err());
        assert!(validate("list_github_epics", &json!({"source":"other"})).is_err());
        assert!(validate("list_github_epics", &json!({"state":"Developing"})).is_err());
        assert!(
            validate(
                "list_azure_hus",
                &json!({"organization":"https://evil.test"})
            )
            .is_err()
        );
        assert!(validate("unknown", &json!({})).is_err());
        assert!(validate("answer_send", &json!({})).is_err());
        assert!(confirmation("list_azure_hus", &json!({})).is_none());
    }

    #[test]
    fn every_tool_has_a_closed_schema_and_external_ones_ask_first() {
        let mut names = std::collections::BTreeSet::new();
        for tool in TOOLS {
            assert!(names.insert(tool.name), "{}", tool.name);
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{}", tool.name);
            assert_eq!(schema["additionalProperties"], false, "{}", tool.name);
            for required in schema["required"].as_array().unwrap() {
                assert!(
                    schema["properties"]
                        .get(required.as_str().unwrap())
                        .is_some(),
                    "{}",
                    tool.name
                );
            }
            let ask = confirmation(
                tool.name,
                &json!({"resolution": {"action": "create"}, "mode": "active", "action": "start"}),
            );
            assert_eq!(
                ask.is_some(),
                tool.effect == Effect::External,
                "{}",
                tool.name
            );
        }
        assert!(confirmation("activity_run", &json!({})).is_none());
        assert!(
            confirmation(
                "machine_connect",
                &json!({"url":"https://assistant.example.com"})
            )
            .unwrap()
            .contains("https://assistant.example.com")
        );
        assert!(validate("activity_resolve", &json!({})).is_err());
    }

    #[test]
    fn map_lists_are_upserted_by_id() {
        let mut profile = json!({"map": {"people": [{"id": "ana", "name": "Ana"}]}});
        upsert(
            &mut profile,
            "people",
            json!({"id": "ana", "name": "Ana Pérez"}),
        )
        .unwrap();
        upsert(
            &mut profile,
            "contexts",
            json!({"id": "cobros", "name": "Cobros"}),
        )
        .unwrap();
        assert_eq!(
            profile["map"]["people"],
            json!([{"id": "ana", "name": "Ana Pérez"}])
        );
        assert_eq!(profile["map"]["contexts"][0]["id"], "cobros");
        assert!(upsert(&mut profile, "people", json!({"name": "Sin ID"})).is_err());
        remove(&mut profile, "people", "ana").unwrap();
        assert!(remove(&mut profile, "people", "ana").is_err());
    }
}
