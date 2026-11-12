//! Progress summaries of a workflow run, written by the workflow director, and their sending to
//! Teams: `workflow_summary` and `workflow_summary_send`, shared by the CLI, MCP, the portal and
//! the remote endpoint. They run on the owner's coordinator, where the run records live.
//!
//! The input is the run record only (`input`): the item's title and verified link, the
//! workflow's name, every stage with its timestamps and outcome, the children with their links,
//! the state of each coding run and its pull requests, the block outcomes, the director's own
//! classification and the attention or error text. Never a session transcript or launcher
//! output. The director runs in its tool-less, read-only, closed-JSON session on its machine
//! (`implementation::director::ask` with the `summarize` task appended by code to the `direct`
//! prompt, through the same `workflow_direct` machine call when remote, within the same time
//! budget) and returns `{"summary": markdown}`. Code makes what it returns safe to store and
//! send (`compose`): every URL, link and `@[...]` mention marker the model wrote is dropped,
//! the text is bounded and a `Links` list built from the run record (the item always, the
//! children and pull requests the summary names) is appended, so the only links in a stored
//! summary are the verified ones. A malformed or empty answer is an error and stores nothing.
//!
//! A summary is stored on the run (`Run::summaries`: `id` (the generation's ID in Agent
//! history), `text`, `created_at`, `provider`, `model`, `session_id`; at most `MAX_SUMMARIES`,
//! the oldest dropped). Sending posts one stored summary, or the edited text the user gives, to
//! one chat of the user's chat directory through `Graph::post_chat` (the `teams_post` path: the
//! user's own message, Markdown through `teams::html`, `@[Name]` mentions resolved against the
//! chat's members, one POST, never retried, recorded as `teams_post`). Each send leaves a
//! record on the run (`Run::summary_sends`) that is `uncertain` from before the request until
//! its result is known, so an interruption never reads as a send that did not happen.
use super::record::Run;
use super::runs::{found, require_engine};
use super::*;
use crate::adapters::graph::PostRefusal;
use crate::llm::implementation::DirectTask;

pub(in crate::app) const METHODS: [&str; 2] = ["workflow_summary", "workflow_summary_send"];

/// At most this many summaries are kept on a run; the oldest is dropped.
pub(in crate::app) const MAX_SUMMARIES: usize = 20;
/// A stored summary, links included, has at most this many characters.
pub(in crate::app) const MAX_CHARS: usize = 4000;
/// The appended list of links takes at most this many characters.
const LINKS_CHARS: usize = 1200;
/// An edited text to send has at most this many characters.
pub(in crate::app) const MAX_EDITED_CHARS: usize = 8000;
/// At most this many send records are kept on a run; the oldest is dropped.
pub(in crate::app) const MAX_SENDS: usize = 100;
/// At most this many mentions in one send.
const MAX_MENTIONS: usize = 20;
/// How long reading the chat directory may take before a send.
const CHATS_SECONDS: u64 = 60;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SummaryArgs {
    run: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    run: String,
    /// The stored summary's ID.
    summary: String,
    chat: String,
    #[serde(default)]
    mentions: Vec<String>,
    /// The edited text to send instead of the stored summary's.
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    confirmed: bool,
}

pub(super) async fn operation(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> Result<Value, String> {
    match method {
        "workflow_summary" => summarize(host, parse(args)?).await,
        "workflow_summary_send" => send(host, parse(args)?).await,
        _ => Err(invalid("Unknown workflow operation.")),
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn time(at: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(at, 0).map(|t| t.to_rfc3339())
}
fn bounded(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// What the director reads: the run record, bounded, and nothing else (untrusted input to it:
/// titles, branch names and the attention text come from providers and agents).
pub(super) fn input(run: &Run, language: crate::config::Language, now: i64) -> Value {
    let stage = |s: &super::record::Stage| {
        json!({"stage": s.stage.word(), "started_at": time(s.started_at),
            "finished_at": s.finished_at.and_then(time), "outcome": s.outcome})
    };
    let child = |c: &super::record::Child| {
        json!({
            "id": c.id, "title": bounded(&c.title, 200), "url": c.url, "type": c.kind,
            "repository": c.repository.as_ref().map(|r| r.name.clone()),
            "coding_run": c.coding.as_ref().map(|i| json!({"started": i.started, "state": i.state})),
            "started_at": c.started_at.and_then(time), "finished_at": c.finished_at.and_then(time),
            "branch": c.branch, "outcome": c.outcome,
            "pull_requests": c.pull_requests.iter().take(20).map(|p| json!({
                "id": p.id, "title": bounded(&p.title, 200), "status": p.status, "url": p.url,
                "created_at": p.created_at, "closed_at": p.closed_at, "accepted": p.accepted,
            })).collect::<Vec<_>>(),
        })
    };
    let item = &run.item;
    json!({
        "language": language,
        "generated_at": time(now),
        "workflow": run.definition.as_ref().map(|w| json!({"id": w.id, "name": w.name})),
        "item": {"id": item.id, "title": bounded(&item.title, 500), "url": item.url,
            "type": item.kind, "category": item.category, "project": item.project,
            "state_when_started": item.state},
        "state": run.state.word(),
        "stage": run.stopped_in.unwrap_or(run.state).word(),
        "created_at": time(run.created_at),
        "finished_at": run.finished_at.and_then(time),
        "stages": run.timeline.iter().take(100).map(stage).collect::<Vec<_>>(),
        "classification": run.classification.as_ref().map(|c| json!({
            "outcome": c["outcome"], "reason": c["reason"].as_str().map(|r| bounded(r, 300)),
        })),
        "children": run.children.iter().take(50).map(child).collect::<Vec<_>>(),
        "blocks": run.block_outcomes.iter().take(50).map(|b| json!({
            "block": b.block, "kind": b.kind, "stage": b.stage, "outcome": bounded(&b.outcome, 300),
            "at": time(b.at),
        })).collect::<Vec<_>>(),
        "attention": run.attention.as_ref().map(|a| bounded(a, 1000)),
        "error": run.error.as_ref().map(|e| bounded(e, 1000)),
    })
}

/// One verified link of the run record: a work item or a pull request.
struct Link {
    id: u64,
    pull_request: bool,
    label: String,
    title: String,
    url: String,
}

/// The links the run record verifies: the item, its children and their pull requests.
fn links(run: &Run) -> Vec<Link> {
    let mut found: Vec<Link> = Vec::new();
    let mut work_item = |id: u64, title: &str, url: &str| {
        if !found.iter().any(|l| l.url == url) {
            found.push(Link {
                id,
                pull_request: false,
                label: format!("#{id} {}", bounded(title, 80)),
                title: title.to_owned(),
                url: url.to_owned(),
            });
        }
    };
    work_item(run.item.id, &run.item.title, &run.item.url);
    for child in &run.children {
        work_item(child.id, &child.title, &child.url);
    }
    for pull in run.children.iter().flat_map(|c| &c.pull_requests) {
        if !found.iter().any(|l| l.url == pull.url) {
            found.push(Link {
                id: pull.id,
                pull_request: true,
                label: format!("PR !{} {}", pull.id, bounded(&pull.title, 80)),
                title: pull.title.clone(),
                url: pull.url.clone(),
            });
        }
    }
    found
}

fn boundary(text: &str, at: usize) -> bool {
    !text[..at]
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
        && !text[at..].chars().next().is_some_and(char::is_alphanumeric)
}
/// Whether `lower` has `needle` as whole words.
fn names(lower: &str, needle: &str) -> bool {
    !needle.is_empty()
        && lower
            .match_indices(needle)
            .any(|(at, hit)| boundary(lower, at) && boundary(lower, at + hit.len()))
}

/// Whether the summary names the item or pull request: by its number the way people write it
/// (`#41`, `HU 41`, `PR 12`, `!12`) or by its title when that is long enough to be its own.
fn mentioned(text: &str, link: &Link) -> bool {
    let lower = text.to_lowercase();
    let id = link.id;
    let number = if link.pull_request {
        format!(
            r"(?:!|\bpr\s*[#!]?|\bpull request\s*[#!]?|\bsolicitud de extracci[oó]n\s*[#!]?)\s*{id}\b"
        )
    } else {
        format!(r"(?:#|\b(?:hu|work item|task|tarea|bug|item)\s*#?)\s*{id}\b")
    };
    if regex::Regex::new(&number).is_ok_and(|re| re.is_match(&lower)) {
        return true;
    }
    let title = link.title.trim().to_lowercase();
    title.chars().count() >= 8 && names(&lower, &title)
}

/// What the model wrote, made safe to store and send: no URL, link, HTML tag or mention marker
/// of its own, no control characters; at most `limit` characters.
fn clean(text: &str, limit: usize) -> String {
    let patterns: [(&str, &str); 7] = [
        // Images and links keep their words.
        (
            r"!\[([^\]\n]*)\]\((?:[^()\s]|\([^()\s]*\))*(?:\s+[^)\n]*)?\)",
            "$1",
        ),
        (
            r"\[([^\]\n]+)\]\((?:[^()\s]|\([^()\s]*\))*(?:\s+[^)\n]*)?\)",
            "$1",
        ),
        // Reference-style link definitions, autolinks, bare URLs of any scheme and www hosts.
        (r"(?m)^[ \t]*\[[^\]\n]+\]:[ \t]*\S+.*$", ""),
        (r"(?i)<?\b[a-z][a-z0-9+.\-]{1,20}://[^\s<>)\]]*>?", ""),
        (r"(?i)<?\b(?:www\.|mailto:)[^\s<>)\]]+>?", ""),
        // HTML tags.
        (r"</?[a-zA-Z][^>\n]*>", ""),
        // The marker Teams mentions are written with: a model grants no mention.
        (r"@\[([^\]\n]*)\]", "@$1"),
    ];
    let mut text = text.replace(['\u{e000}', '\u{e001}'], "");
    for (pattern, replacement) in patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            text = re.replace_all(&text, replacement).into_owned();
        }
    }
    let text: String = text
        .replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let text = regex::Regex::new(r"\n{3,}")
        .map(|re| re.replace_all(&text, "\n\n").into_owned())
        .unwrap_or(text);
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    // Cut where a word ends (the last break that fits, unless the cut already falls between
    // words), and say so.
    let room = limit.saturating_sub(1);
    let cut: String = text.chars().take(room).collect();
    let whole_word = text
        .chars()
        .nth(room)
        .is_none_or(|next| !next.is_alphanumeric());
    let at = if whole_word {
        cut.len()
    } else {
        cut.rfind(char::is_whitespace).unwrap_or(cut.len())
    };
    format!("{}…", cut[..at].trim_end())
}

fn label(text: &str) -> String {
    text.replace(['[', ']', '\n', '\r'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The stored text: the model's summary cleaned and bounded, then the list of verified links of
/// the run record (the item always; the children and pull requests the final text names).
pub(super) fn compose(summary: &str, run: &Run, language: crate::config::Language) -> String {
    let header = language.say("Links", "Enlaces");
    let all = links(run);
    // The links of a text: the item, then what the text names, within their share.
    let section_for = |text: &str| -> String {
        let mut list = String::new();
        for link in all
            .iter()
            .enumerate()
            .filter(|(n, l)| *n == 0 || mentioned(text, l))
            .map(|(_, l)| l)
        {
            let entry = format!("- [{}]({})\n", label(&link.label), link.url);
            // The header and its separator are counted once, with the first entry.
            let used = if list.is_empty() { header.len() + 4 } else { 0 };
            if list.len() + used + entry.len() > LINKS_CHARS {
                break;
            }
            list.push_str(&entry);
        }
        if list.is_empty() {
            String::new()
        } else {
            format!("**{header}**\n{}", list.trim_end())
        }
    };
    let join = |body: &str, section: &str| match (body.is_empty(), section.is_empty()) {
        (_, true) => body.to_owned(),
        (true, false) => section.to_owned(),
        (false, false) => format!("{body}\n\n{section}"),
    };
    // The list for the whole text fixes the room the text leaves; the links kept are then those
    // of the text as it is stored, which can only name less than the whole.
    let whole = section_for(&clean(summary, MAX_CHARS));
    let body = clean(summary, MAX_CHARS.saturating_sub(whole.chars().count() + 2));
    let stored = join(&body, &section_for(&body));
    if stored.chars().count() <= MAX_CHARS {
        stored
    } else {
        join(&body, &whole)
    }
}

/// The summary in the director's answer: exactly `{"summary": text}`.
fn written(answer: Option<&Value>) -> Option<&str> {
    let object = answer?.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object.get("summary")?.as_str()
}

fn failed(text: &str) -> String {
    let redactor = security::Redactor::new(&[], Vec::new()).expect("built-in redaction patterns");
    format!("[not_ready] {}", redactor.redact(text))
}

/// `workflow_summary`: ask the director for a progress summary of the run and store it.
async fn summarize(host: &Arc<Host>, args: SummaryArgs) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let run = found(state, &args.run)?;
    let map = state.settings.map().map_err(fail)?;
    // The same director that classifies the run's project.
    let director = super::director::director_for(&map, &run.item.organization, &run.item.project)
        .ok_or_else(|| super::director::no_director(&run.item.project))?
        .director;
    let machine = implementation::machine_facts(host, &director.machine)
        .await
        .map_err(fail)?
        .ok_or_else(|| {
            invalid(format!(
                "The workflow director's machine {} is not an execution machine of this account; choose it again (workflow_settings_update).",
                director.machine
            ))
        })?;
    if !(machine.own || machine.online) {
        return Err("[not_ready] The workflow director's machine is offline.".into());
    }
    let language = state.settings.config().map_err(fail)?.llm.language;
    let json_input = input(&run, language, now()).to_string();
    let instructions =
        crate::agent_prompts::resolve(&map.agent_prompts, crate::agent_prompts::DIRECT);
    let item = crate::tools::todo::Item {
        id: run.item.id,
        title: run.item.title.clone(),
        url: run.item.url.clone(),
        project: run.item.project.clone(),
        state: run.item.state.clone(),
        kind: run.item.kind.clone(),
        category: run.item.category.clone(),
        created_at: None,
        changed_at: None,
        dates: Vec::new(),
    };
    let answer = Box::pin(implementation::director::ask(
        host,
        implementation::director::Ask {
            machine: &director.machine,
            task: DirectTask::Summarize,
            director: &director,
            instructions: &instructions,
            input: json_input,
            item: &item,
        },
    ))
    .await
    .map_err(|error| failed(&error.to_string()))?;
    let session = &answer.run;
    let _ = implementation::director::annotate(host, session, "workflow_run", json!(run.id));
    let problem = match (
        written(answer.direction.answer.as_ref()),
        answer.direction.problem,
    ) {
        (Some(text), None) if !clean(text, MAX_CHARS).is_empty() => None,
        (Some(_), None) => Some("The workflow director's summary was empty."),
        (_, Some(problem)) => Some(problem),
        (None, None) => Some("The workflow director's answer is not the JSON it was asked for."),
    };
    if let Some(problem) = problem {
        let _ = implementation::director::annotate(host, session, "state", json!("failed"));
        let _ = implementation::director::annotate(host, session, "output", json!(problem));
        return Err(failed(&format!("{problem} No summary was stored.")));
    }
    let text = compose(
        written(answer.direction.answer.as_ref()).unwrap_or_default(),
        &run,
        language,
    );
    let stored = json!({
        "id": session,
        "text": text,
        "created_at": now(),
        "provider": director.provider,
        "model": director.model,
        "session_id": answer.session_id,
    });
    let _ = implementation::director::annotate(
        host,
        session,
        "summary",
        json!({"chars": text.chars().count()}),
    );
    {
        let lock = state.workflows.lock(&run.id);
        let _guard = lock.lock().await;
        let mut run = found(state, &run.id)?;
        run.summaries.push(stored.clone());
        while run.summaries.len() > MAX_SUMMARIES {
            run.summaries.remove(0);
        }
        state.workflows.db.save(&run).map_err(fail)?;
    }
    Ok(json!({"workflow_run": run.id, "summary": stored, "run": session}))
}

/// What a person confirms before a summary is sent: the run, summary, chat and mentions and the
/// exact text that will be posted (`text`).
fn describe_send(
    run: &str,
    summary: &str,
    chat: &str,
    mentions: &[String],
    text: &str,
    edited: bool,
) -> String {
    format!(
        "Post the {} of workflow run {run} (summary {summary}) in the Teams chat {chat}, as you, mentioning {}: one message, never retried. The text posted is:\n\n{text}",
        if edited {
            "edited text"
        } else {
            "stored summary"
        },
        if mentions.is_empty() {
            "no one".to_owned()
        } else {
            mentions.join(", ")
        }
    )
}

/// What the person confirms, from the arguments of the operation and the stored summary's text
/// (the text posted is the edited one when `text` is given).
pub(in crate::app) fn describe_args(args: &Value, stored: Option<&str>) -> String {
    let mentions: Vec<String> = args["mentions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str().map(str::to_owned))
        .collect();
    let edited = args["text"].as_str().filter(|t| !t.trim().is_empty());
    describe_send(
        args["run"].as_str().unwrap_or("?"),
        args["summary"].as_str().unwrap_or("?"),
        args["chat"].as_str().unwrap_or("?"),
        &mentions,
        edited.or(stored).unwrap_or("(the stored summary's text)"),
        edited.is_some(),
    )
}

/// The stored text of a summary in the record `workflow_run_status` returns.
pub(in crate::app) fn stored_text<'a>(status: &'a Value, summary: &str) -> Option<&'a str> {
    status["run"]["summaries"]
        .as_array()?
        .iter()
        .find(|s| s["id"] == summary)?["text"]
        .as_str()
}

/// What a failed record of the send, after the request, must not hide.
fn unrecorded(run: &str, send: &str, outcome: &str, why: &str) -> String {
    format!(
        "The send's record could not be saved on workflow run {run} (send {send}, {outcome}): {why} Its first record there, written before the request, still says uncertain."
    )
}

/// `workflow_summary_send`: post one stored summary (or an edited text) to one chat.
async fn send(host: &Arc<Host>, args: SendArgs) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    // An agent sends a stored summary as written; editing it is for the person, in the portal or
    // at this computer's page, like writing any message of their own.
    if args.text.is_some()
        && authority::invocation(state)
            .is_ok_and(|i| matches!(i.transport, authority::Transport::Agent))
    {
        return Err(invalid(
            "A remote agent sends a stored summary as it is written; editing the text is for the person, in the portal or at this computer's page.",
        ));
    }
    let run = found(state, &args.run)?;
    let summary = run
        .summaries
        .iter()
        .find(|s| s["id"] == args.summary.as_str())
        .ok_or_else(|| {
            invalid(format!(
                "Workflow run {} has no summary {}; workflow_summary writes one.",
                run.id, args.summary
            ))
        })?;
    let chat = args.chat.trim();
    if chat.is_empty() || chat.len() > 512 || chat.chars().any(char::is_control) {
        return Err(invalid("Name the chat: an ID from teams_chats."));
    }
    if chat == "self" || chat == "48:notes" {
        return Err(invalid(
            "A summary goes to a person's or a group's chat from the chat directory (teams_chats); your personal chat is not in it.",
        ));
    }
    let marker = regex::Regex::new(r"@\[([^\]\n]{1,100})\]").map_err(fail)?;
    let edited = args
        .text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    if args.text.is_some() && edited.is_none() {
        return Err(invalid("The edited text is empty."));
    }
    let body = edited
        .map(str::to_owned)
        .unwrap_or_else(|| summary["text"].as_str().unwrap_or_default().to_owned());
    if body.chars().count() > MAX_EDITED_CHARS {
        return Err(invalid(format!(
            "The text to send has at most {MAX_EDITED_CHARS} characters."
        )));
    }
    if body.trim().is_empty() {
        return Err(invalid("There is nothing to send."));
    }
    // The people to mention: the ones named, then any written as `@[Name]` in an edited text.
    let mut wanted: Vec<String> = Vec::new();
    for name in args.mentions.iter().map(String::as_str).chain(
        edited
            .iter()
            .flat_map(|t| marker.captures_iter(t))
            .filter_map(|c| c.get(1).map(|m| m.as_str())),
    ) {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > 100
            || name.contains(['[', ']', '\n', '\r'])
            || name.chars().any(char::is_control)
        {
            return Err(invalid(
                "A mention is a member's display name (at most 100 characters, without brackets).",
            ));
        }
        if !wanted
            .iter()
            .any(|w| w.to_lowercase() == name.to_lowercase())
        {
            wanted.push(name.to_owned());
        }
    }
    if wanted.len() > MAX_MENTIONS {
        return Err(invalid(format!("At most {MAX_MENTIONS} mentions.")));
    }
    let earlier_uncertain = run.summary_sends.iter().any(|s| {
        s["summary"] == args.summary.as_str()
            && s["chat"]["id"] == chat
            && s["outcome"] == "uncertain"
    });
    if !args.confirmed {
        return Err(invalid(format!(
            "Confirm with the user, then send with confirmed: true. {}{}",
            describe_send(
                &run.id,
                &args.summary,
                chat,
                &wanted,
                &body,
                edited.is_some()
            ),
            if earlier_uncertain {
                "\n\nAn earlier send of this summary to this chat is uncertain: check Teams first, since this posts again."
            } else {
                ""
            }
        )));
    }
    // The destination must be one of the user's chats in the directory; its members, read as
    // the post reads them, resolve the mentions before anything is posted.
    let graph = teams_graph(state).await.map_err(fail)?;
    let directory = tokio::time::timeout(
        std::time::Duration::from_secs(CHATS_SECONDS),
        graph.chat_directory(None),
    )
    .await
    .map_err(|_| {
        "[network] The chat directory could not be read in time; nothing was sent.".to_owned()
    })?
    .map_err(fail)?;
    let entry = directory.iter().find(|c| c["id"] == chat).ok_or_else(|| {
        invalid(format!(
            "The chat {chat} is not in your chat directory; choose one that teams_chats lists. Nothing was sent."
        ))
    })?;
    let mut resolved: Vec<(String, String)> = Vec::new();
    if !wanted.is_empty() {
        let members = graph.chat_members(chat).await.map_err(fail)?;
        for name in &wanted {
            let (user, shown) = crate::adapters::graph::mention_target(&members, name)
                .map_err(|refusal| invalid(format!("The mention «{name}»: {refusal}")))?;
            if !resolved.iter().any(|(u, _)| *u == user) {
                resolved.push((user, shown));
            }
        }
    }
    // Mentions not written in the text open the message, one line.
    let inline: Vec<String> = edited
        .iter()
        .flat_map(|t| marker.captures_iter(t))
        .filter_map(|c| c.get(1).map(|m| m.as_str().trim().to_lowercase()))
        .collect();
    let mut opening: Vec<String> = Vec::new();
    for mention in args.mentions.iter().map(|m| m.trim()) {
        let lower = mention.to_lowercase();
        if !mention.is_empty()
            && !inline.contains(&lower)
            && !opening
                .iter()
                .any(|o| o.to_lowercase() == format!("@[{lower}]"))
        {
            opening.push(format!("@[{mention}]"));
        }
    }
    let message = if opening.is_empty() {
        body.clone()
    } else {
        format!("{}\n\n{body}", opening.join(" "))
    };
    if message.chars().count() > 20_000 {
        return Err(invalid("The message is too long."));
    }
    // The record is `uncertain` from before the request until its result is known. The text is
    // kept when it is an edit; otherwise the summary's ID says what was sent.
    let id = uuid::Uuid::new_v4().to_string();
    let mut record = json!({
        "id": id,
        "summary": args.summary,
        "chat": {"id": chat, "topic": entry["topic"], "type": entry["type"]},
        "mentions": resolved.iter().map(|(_, shown)| shown.clone()).collect::<Vec<_>>(),
        "chars": body.chars().count(),
        "edited": edited.is_some(),
        "outcome": "uncertain",
        "at": now(),
    });
    if edited.is_some() {
        record["text"] = json!(body);
    }
    {
        let lock = state.workflows.lock(&run.id);
        let _guard = lock.lock().await;
        let mut run = found(state, &run.id)?;
        run.summary_sends.push(record.clone());
        while run.summary_sends.len() > MAX_SENDS {
            run.summary_sends.remove(0);
        }
        state.workflows.db.save(&run).map_err(fail)?;
    }
    // One request, never retried. `post_chat` reports every failure after the request as
    // uncertain; whatever else fails happened before it, and nothing was sent.
    let result = graph.post_chat(chat, &message, &[]).await;
    let (outcome, detail, message_id) = match &result {
        Ok(sent) => ("sent", None, sent["message_id"].as_str().map(str::to_owned)),
        Err(error) => match error.downcast_ref::<PostRefusal>() {
            Some(PostRefusal::Uncertain) => {
                ("uncertain", Some(PostRefusal::Uncertain.to_string()), None)
            }
            Some(refusal) => ("refused", Some(refusal.to_string()), None),
            None => (
                "refused",
                Some("Teams could not be reached before the request; nothing was sent.".to_owned()),
                None,
            ),
        },
    };
    let mut finished = record.clone();
    finished["outcome"] = json!(outcome);
    finished["finished_at"] = json!(now());
    if let Some(detail) = &detail {
        finished["detail"] = json!(detail);
    }
    if let Some(message_id) = &message_id {
        finished["message_id"] = json!(message_id);
    }
    // The post's own outcome is the answer, whether or not it could be written down.
    let saved: Result<(), String> = async {
        let lock = state.workflows.lock(&run.id);
        let _guard = lock.lock().await;
        let mut run = found(state, &run.id)?;
        if let Some(entry) = run
            .summary_sends
            .iter_mut()
            .find(|s| s["id"] == id.as_str())
        {
            *entry = finished.clone();
        }
        state.workflows.db.save(&run).map_err(fail)
    }
    .await;
    let unsaved = saved
        .err()
        .map(|why| unrecorded(&run.id, &id, outcome, &why));
    match outcome {
        "sent" => {
            let mut reply = json!({"workflow_run": run.id, "sent": true, "send": finished});
            if let Some(warning) = unsaved {
                reply["record_saved"] = json!(false);
                reply["warning"] = json!(format!(
                    "The message was sent to Teams (message {}). {warning}",
                    message_id.unwrap_or_default()
                ));
            }
            Ok(reply)
        }
        _ => Err(format!(
            "[error] {} {}",
            detail.unwrap_or_default(),
            unsaved.unwrap_or_else(|| format!(
                "The attempt is recorded on workflow run {} (send {id}, {outcome}).",
                run.id
            ))
        )),
    }
}

#[cfg(all(test, unix))]
mod tests;
