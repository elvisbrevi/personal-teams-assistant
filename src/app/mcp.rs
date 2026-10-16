//! `pta mcp`: a Model Context Protocol server over stdio, so an agent (Codex, Claude Code…) can
//! operate the app with the same operations as the GUI and `pta`. Every tool goes through the
//! host's IPC contract, so audiences, checks and records are the host's own.
//!
//! Tools that act outside the app or grant access — sending to Teams, writing to Azure DevOps,
//! authorizing a source, changing the mode, the assistant's state or activity registration —
//! need the user's confirmation: through MCP elicitation when the client offers it (the user
//! answers the client, not the agent), otherwise with `confirmed: true`, which the agent may
//! set only after the user approved that exact action.
//!
//! stdout carries only protocol messages; nothing else is printed.

use super::{cli, control::Reply};
use serde_json::{Value, json};
use std::collections::VecDeque;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines, Stdin, Stdout};

const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Personal Teams Assistant answers Microsoft Teams messages as the user, only from the sources the user authorized for each conversation, and registers the user's work in Azure DevOps. Read before you write: status, messages_list, sources_list, contexts_list, cases_list, notes_search, knowledge_search. Local changes (notes, contexts, cases, people, ratings, answer drafts) are reversible. Tools marked as needing confirmation act outside the app or grant access; the server asks the user, or requires confirmed: true, which you may set only after the user approved that exact action in this conversation. Ask the user whenever a fact is ambiguous, could be someone else's opinion, would replace what they said before, or when the HU, effort or day of an activity is unclear. Never put secrets, emails, phone numbers or IDs in notes or comments; the server refuses them. Answers are never sent to Teams unless answer_send is confirmed.";

/// How a tool may run.
#[derive(Clone, Copy, PartialEq)]
enum Effect {
    /// Reads only.
    Read,
    /// Changes the app's own data or profile; reversible.
    Local,
    /// Acts outside the app or grants access: needs the user's confirmation.
    External,
}

struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    effect: Effect,
    schema: fn() -> Value,
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

const TOOLS: &[Tool] = &[
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
        name: "cases_list",
        title: "Cases",
        description: "The built-in kinds of request with their default texts (how the model recognizes each, how it is answered) and the user's changes.",
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
        name: "activity_pending",
        title: "Activities needing input",
        description: "Detected work waiting for the user's decision (HU, effort, description, required fields), across days.",
        effect: Effect::Read,
        schema: none,
    },
    Tool {
        name: "activity_history",
        title: "Activity history",
        description: "Entries and runs of one day, or the approved tasks of every day from `from` to `day`.",
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
        description: "Scheduled and automatic registration settings: enabled, schedule, workdays, time zone, sources, daily hours, task fields.",
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
        name: "case_update",
        title: "Change a case",
        description: "Replaces the user's change to a built-in case: enabled, description (how the model recognizes it), keywords, sources and instructions (how to answer it). Omitted fields keep their defaults.",
        effect: Effect::Local,
        schema: || {
            object(
                json!({"id": string("general, status_round, recap, activity_review, documentation, personal_request or statement."), "change": {"type": "object", "description": "enabled, description, keywords, sources, instructions."}}),
                &["id", "change"],
            )
        },
    },
    Tool {
        name: "case_reset",
        title: "Restore a case",
        description: "Restores a built-in case's defaults.",
        effect: Effect::Local,
        schema: || object(json!({"id": string("The case ID.")}), &["id"]),
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
        description: "Rates an answer good or bad with a comment; a good one can become an example for similar requests (structure and tone only), within a context. null clears the rating.",
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
        title: "Review a day's activity",
        description: "Reviews and, when automatic registration is on, registers a day's work as completed tasks in Azure DevOps. Needs the user's confirmation.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"day": string("YYYY-MM-DD (default today, within the last 14 days)."), "context": string("Context for the agents (at most 6000 bytes)."), "confirmed": confirmed()}),
                &[],
            )
        },
    },
    Tool {
        name: "activity_resolve",
        title: "Decide on a pending activity",
        description: "create writes the completed task in Azure DevOps; refine rewrites the draft with context; dismiss discards it for good. create and dismiss need the user's confirmation.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"id": string("The entry ID."), "resolution": {"type": "object", "description": "action (create|refine|dismiss), parent, title, description, hours, context, fields, date_attribution."}, "confirmed": confirmed()}),
                &["id", "resolution"],
            )
        },
    },
    Tool {
        name: "activity_settings_update",
        title: "Change activity registration",
        description: "Changes activity registration settings (schedule, workdays, time zone, sources, automatic registration, task fields). Needs the user's confirmation.",
        effect: Effect::External,
        schema: || {
            object(
                json!({"changes": {"type": "object", "description": "Fields of activity_registration to set, as activity_settings shows them."}, "confirmed": confirmed()}),
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
async fn run(name: &str, args: &Value) -> Result<Value, String> {
    match name {
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
                    "person": a["person"], "feedback": a["feedback"]})
            }).collect::<Vec<_>>()))
        }
        "message_show" => {
            let rows = ipc("audit", json!({"limit": 1, "resource": text(args, "resource")?, "content": true})).await?;
            rows.get(0).cloned().ok_or_else(|| "Unknown message.".into())
        }
        "sources_list" => Ok(ipc("snapshot", json!({})).await?["map"]["resources"].clone()),
        "contexts_list" => Ok(ipc("snapshot", json!({})).await?["map"]["contexts"].clone()),
        "people_list" => Ok(ipc("snapshot", json!({})).await?["map"]["people"].clone()),
        "cases_list" => {
            let s = ipc("snapshot", json!({})).await?;
            Ok(json!({"built_in": s["cases"], "changes": s["map"]["cases"]}))
        }
        "notes_search" => ipc("notes_list", json!({"query": args.get("query"), "context": args.get("context"), "limit": args.get("limit")})).await,
        "knowledge_search" => ipc("knowledge_search", json!({"query": text(args, "query")?, "sources": args.get("sources"), "context": args.get("context"), "limit": args.get("limit")})).await,
        "index_status" => ipc("index_status", json!({})).await,
        "activity_status" => ipc("activity_status", json!({})).await,
        "activity_pending" => ipc("activity_pending", json!({})).await,
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
        "activity_run" => ipc("activity_run", json!({"day": args.get("day"), "context": args.get("context").and_then(Value::as_str).unwrap_or("")})).await,
        "activity_resolve" => ipc("activity_resolve", json!({"id": text(args, "id")?, "resolution": args.get("resolution")})).await,
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
fn confirmation(name: &str, args: &Value) -> Option<String> {
    let resolution = args["resolution"]["action"].as_str().unwrap_or("");
    Some(match name {
        "answer_send" => format!(
            "Send the written answer of {} to Teams, as you?",
            args["resource"].as_str().unwrap_or("this message")
        ),
        "activity_run" => format!(
            "Review your activity of {} (with automatic registration on, completed tasks may be created in Azure DevOps)?",
            args["day"].as_str().unwrap_or("today")
        ),
        "activity_resolve" if resolution == "refine" => return None,
        "activity_resolve" if resolution == "dismiss" => {
            "Dismiss this pending activity for good?".to_owned()
        }
        "activity_resolve" => format!(
            "Create the completed task «{}» in Azure DevOps?",
            args["resolution"]["title"].as_str().unwrap_or("")
        ),
        "activity_settings_update" => format!("Change activity registration: {}?", args["changes"]),
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

pub(super) struct Server {
    lines: Lines<BufReader<Stdin>>,
    out: Stdout,
    /// Messages read while waiting for the user's answer, handled next.
    queued: VecDeque<Value>,
    /// The client can ask the user (MCP elicitation).
    elicitation: bool,
    asked: u64,
}

impl Server {
    pub(super) fn new() -> Self {
        Self {
            lines: BufReader::new(tokio::io::stdin()).lines(),
            out: tokio::io::stdout(),
            queued: VecDeque::new(),
            elicitation: false,
            asked: 0,
        }
    }
    async fn next(&mut self) -> Option<Value> {
        if let Some(message) = self.queued.pop_front() {
            return Some(message);
        }
        loop {
            let line = self.lines.next_line().await.ok()??;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&line) {
                Ok(message) => return Some(message),
                Err(_) => {
                    let _ = self
                        .send(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}}))
                        .await;
                }
            }
        }
    }
    async fn send(&mut self, message: Value) -> std::io::Result<()> {
        let mut text = serde_json::to_string(&message).unwrap_or_default();
        text.push('\n');
        self.out.write_all(text.as_bytes()).await?;
        self.out.flush().await
    }
    /// Ask the user, through the client, to confirm `question`; messages that arrive meanwhile
    /// are kept for later.
    async fn ask(&mut self, question: &str) -> bool {
        self.asked += 1;
        let id = format!("confirm-{}", self.asked);
        let request = json!({"jsonrpc": "2.0", "id": id, "method": "elicitation/create", "params": {
            "message": question,
            "requestedSchema": {"type": "object", "properties": {"confirm": {"type": "boolean", "title": "Confirm", "description": "Yes, do it"}}, "required": ["confirm"]},
        }});
        if self.send(request).await.is_err() {
            return false;
        }
        while let Some(message) = self.next_from_client().await {
            if message.get("id").and_then(Value::as_str) == Some(id.as_str())
                && message.get("method").is_none()
            {
                return message["result"]["action"] == "accept"
                    && message["result"]["content"]["confirm"] == true;
            }
            self.queued.push_back(message);
        }
        false
    }
    /// The next message from the client itself, not from the queue.
    async fn next_from_client(&mut self) -> Option<Value> {
        loop {
            let line = self.lines.next_line().await.ok()??;
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                return Some(message);
            }
        }
    }
    async fn call_tool(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params["name"].as_str().unwrap_or_default();
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let tool = TOOLS
            .iter()
            .find(|t| t.name == name)
            .ok_or((-32602, format!("Unknown tool: {name}")))?;
        if !args.is_object() {
            return Err((-32602, "arguments must be an object".into()));
        }
        if tool.effect == Effect::External
            && let Some(question) = confirmation(name, &args)
        {
            let approved = if self.elicitation {
                self.ask(&question).await
            } else {
                args["confirmed"] == true
            };
            if !approved {
                let text = if self.elicitation {
                    "The user did not confirm this action; nothing was done.".to_owned()
                } else {
                    format!(
                        "This needs the user's confirmation: ask them «{question}» and, only if they approve this exact action, call again with confirmed: true. Nothing was done."
                    )
                };
                return Ok(json!({"content": [{"type": "text", "text": text}], "isError": true}));
            }
        }
        Ok(match run(name, &args).await {
            Ok(value) => {
                json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default()}], "isError": false})
            }
            Err(message) => {
                json!({"content": [{"type": "text", "text": message}], "isError": true})
            }
        })
    }
    pub(super) async fn serve(mut self) -> i32 {
        while let Some(message) = self.next().await {
            let Some(method) = message
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                continue; // A response nobody waits for.
            };
            let Some(id) = message.get("id").cloned() else {
                continue; // A notification (initialized, cancelled).
            };
            let params = message.get("params").cloned().unwrap_or(json!({}));
            let result: Result<Value, (i64, String)> = match method.as_str() {
                "initialize" => {
                    self.elicitation = params["capabilities"].get("elicitation").is_some();
                    let requested = params["protocolVersion"].as_str().unwrap_or(PROTOCOLS[0]);
                    let version = PROTOCOLS
                        .iter()
                        .find(|v| **v == requested)
                        .copied()
                        .unwrap_or(PROTOCOLS[0]);
                    Ok(
                        json!({"protocolVersion": version, "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "personal-teams-assistant", "title": "Personal Teams Assistant", "version": env!("CARGO_PKG_VERSION")},
                        "instructions": INSTRUCTIONS}),
                    )
                }
                "ping" => Ok(json!({})),
                "tools/list" => Ok(json!({"tools": TOOLS.iter().map(|t| {
                    json!({"name": t.name, "title": t.title, "description": t.description,
                        "inputSchema": (t.schema)(),
                        "annotations": {"title": t.title, "readOnlyHint": t.effect == Effect::Read,
                            "destructiveHint": t.effect == Effect::External, "openWorldHint": t.effect == Effect::External}})
                }).collect::<Vec<_>>()})),
                "tools/call" => self.call_tool(&params).await,
                _ => Err((-32601, format!("Method not found: {method}"))),
            };
            let reply = match result {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, message)) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
                }
            };
            if self.send(reply).await.is_err() {
                return 1;
            }
        }
        0
    }
}

/// How to add the server to Claude Code and Codex.
pub(super) fn config() -> Reply {
    let pta = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_else(|| "pta".into());
    Reply::success(json!({
        "command": pta, "args": ["mcp"],
        "claude_code": format!("claude mcp add --scope user personal-teams-assistant -- {pta} mcp"),
        "codex": format!("[mcp_servers.personal-teams-assistant]\ncommand = \"{pta}\"\nargs = [\"mcp\"]\n"),
        "json": {"mcpServers": {"personal-teams-assistant": {"command": pta, "args": ["mcp"]}}},
        "tools": TOOLS.iter().map(|t| json!({"name": t.name, "effect": match t.effect { Effect::Read => "read", Effect::Local => "local", Effect::External => "needs confirmation" }})).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Refining a pending activity is local; creating or dismissing it is not.
        assert!(
            confirmation(
                "activity_resolve",
                &json!({"resolution": {"action": "refine"}})
            )
            .is_none()
        );
        assert!(
            confirmation(
                "activity_resolve",
                &json!({"resolution": {"action": "dismiss"}})
            )
            .is_some()
        );
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
