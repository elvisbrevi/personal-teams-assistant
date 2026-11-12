//! The workflow operations shared by the GUI, the CLI and MCP: definitions and blocks
//! (`crate::workflows`) listed, checked, saved and removed on the account's coordinator, and
//! the project's Git repository catalog they are chosen from.
//!
//! Saving a workflow checks it against what the host can read: the project in the enabled Azure
//! source's configured organizations, every repository in the project's catalog, every checkout
//! inside the machine's allowed roots (the machine inspects it once with `checkout_inspect`), its
//! `origin` naming that repository, and the coding agent installed there. What cannot be read
//! (an offline or older machine, a checkout without an origin) is a warning in the reply; what
//! is wrong refuses the save. Blocks that act are authorized when saved: a Teams notification
//! needs confirmation and the reply echoes its chat and template; a launcher block needs the
//! owner's authority (this computer's local page or the owner's own portal session, as approving
//! a launcher does) and an approved launcher of the coordinator with that program digest.
//! Workflows are saved only on the coordinator, where their runs live; they reach the account's
//! other computers with its portable configuration. Other settings editors keep them as stored
//! (`save_settings`). The workflow director's settings and its dry-run classification are in
//! `director`.
use super::*;
use crate::workflows::{self as core, Block, Facts, Inspection, Kind, Workflow};
use serde_json::{Value, json};

pub(super) const METHODS: [&str; 8] = [
    "workflows_list",
    "workflow_show",
    "workflow_save",
    "workflow_remove",
    "workflow_blocks_list",
    "workflow_block_save",
    "workflow_block_remove",
    "workflow_repositories",
];

pub(super) mod acting;
mod director;
pub(super) mod engine;
pub(super) mod implement;
pub(super) mod record;
#[cfg(all(test, unix))]
mod rehearsal;
pub(super) mod runs;
pub(super) mod summary;
#[cfg(test)]
pub(super) mod synthetic;
pub(super) mod watcher;

pub(super) fn handles(method: &str) -> bool {
    METHODS.contains(&method)
        || director::METHODS.contains(&method)
        || runs::METHODS.contains(&method)
        || summary::METHODS.contains(&method)
}
/// The workflow run operations, and the summaries of a run (`workflow_runs_support`).
pub(super) fn runs_handles(method: &str) -> bool {
    runs::METHODS.contains(&method) || summary::METHODS.contains(&method)
}
/// The workflow director's operations (`workflow_director_support`).
pub(super) fn director_handles(method: &str) -> bool {
    director::METHODS.contains(&method)
}

fn invalid(text: impl std::fmt::Display) -> String {
    format!("[invalid_input] {text}")
}
fn conflict(text: impl std::fmt::Display) -> String {
    format!("[conflict] {text}")
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> control::Reply {
    match operation(host, &request.method, &request.args).await {
        Ok(data) => control::Reply::success(data),
        Err(message) => control::failure(&message, false),
    }
}

fn parse<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, String> {
    serde_json::from_value(args.clone()).map_err(|e| invalid(format!("Invalid arguments: {e}.")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    #[serde(default)]
    workflow: Option<Value>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    /// The user confirmed what the workflow's `allow` lets its coding agents do.
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveArgs {
    id: String,
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockSaveArgs {
    block: Value,
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoriesArgs {
    project: String,
    #[serde(default)]
    organization: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Whether workflows are saved here: on the account's coordinator, never on a computer signed
/// in to one.
fn editable(state: &DesktopState) -> Result<bool, String> {
    Ok(implementation::link(state).map_err(fail)?.is_none())
}
fn require_coordinator(state: &DesktopState) -> Result<(), String> {
    if editable(state)? {
        Ok(())
    } else {
        Err("[not_ready] This computer is signed in to the account's coordinator, where workflows are saved and run: change them there (its local page, its portal or its pta). They reach this computer with the account's configuration.".into())
    }
}

/// A workflow as the operations show it: `allow` always present, empty when it allows nothing.
fn shown(workflow: &Workflow) -> Value {
    let mut value = serde_json::to_value(workflow).unwrap_or_default();
    value["allow"] = json!(workflow.allow);
    value
}

/// What a person confirms before a workflow whose coding agents may do more than the fixed
/// rules allow is saved; none when it allows nothing.
pub(super) fn describe_workflow(workflow: &Workflow) -> Option<String> {
    let lifted = core::describe_allow(&workflow.allow);
    (!lifted.is_empty()).then(|| {
        format!(
            "Save workflow {}? In every run it starts (also those that start by themselves), {lifted}, when its instructions ask for it. Its planning run, and every run without this workflow, keep every prohibition.",
            workflow.id
        )
    })
}

/// What a person confirms before a block that acts is saved.
pub(super) fn describe(block: &Block) -> String {
    let stage = block
        .stage
        .and_then(|s| serde_json::to_value(s).ok())
        .and_then(|s| s.as_str().map(str::to_owned))
        .unwrap_or_default();
    match block.kind {
        Kind::TeamsNotification => {
            let chat = block.chat.clone().unwrap_or_default();
            let mentions = if block.mentions.is_empty() {
                "no one".to_owned()
            } else {
                block.mentions.join(", ")
            };
            format!(
                "At stage {stage}, post «{}» to the Teams chat {} ({}), mentioning {mentions}, once each time that stage occurs in a run",
                block.template, chat.name, chat.id
            )
        }
        Kind::Launcher => {
            let launcher = block.launcher.clone().unwrap_or_default();
            format!(
                "At stage {stage}, run the coordinator's approved launcher {} (program digest {})",
                launcher.id, launcher.digest
            )
        }
        Kind::Instruction => format!("The agents read the instruction «{}»", block.name),
    }
}

async fn operation(host: &Arc<Host>, method: &str, args: &Value) -> Result<Value, String> {
    let state = &host.state;
    if director::METHODS.contains(&method) {
        return director::operation(host, method, args).await;
    }
    if runs::METHODS.contains(&method) {
        return runs::operation(host, method, args, &engine::Azure::Live).await;
    }
    if summary::METHODS.contains(&method) {
        return summary::operation(host, method, args).await;
    }
    match method {
        "workflows_list" => {
            ensure_no_arguments(args)?;
            let map = state.settings.map().map_err(fail)?;
            let workflows: Vec<Value> = map.workflows.iter().map(shown).collect();
            Ok(json!({"workflows":workflows,"editable":editable(state)?}))
        }
        "workflow_show" => {
            let a: IdArgs = parse(args)?;
            let map = state.settings.map().map_err(fail)?;
            let workflow = map
                .workflows
                .iter()
                .find(|w| w.id == a.id)
                .ok_or_else(|| invalid(format!("There is no workflow {}.", a.id)))?;
            let blocks: Vec<&Block> = workflow
                .blocks
                .iter()
                .filter_map(|id| map.workflow_blocks.iter().find(|b| &b.id == id))
                .collect();
            Ok(json!({"workflow":shown(workflow),"blocks":blocks,"editable":editable(state)?}))
        }
        "workflow_blocks_list" => {
            ensure_no_arguments(args)?;
            let map = state.settings.map().map_err(fail)?;
            let used_by: BTreeMap<&str, Vec<&str>> = map
                .workflow_blocks
                .iter()
                .map(|b| (b.id.as_str(), core::using(&map.workflows, &b.id)))
                .collect();
            Ok(json!({"blocks":map.workflow_blocks,"used_by":used_by,"editable":editable(state)?}))
        }
        "workflow_repositories" => {
            let a: RepositoriesArgs = parse(args)?;
            if a.project.trim().is_empty() || a.project.len() > 120 {
                return Err(invalid("Give the Azure DevOps project's name."));
            }
            tools::todo::repositories(
                state,
                a.source.as_deref(),
                a.organization.as_deref(),
                &a.project,
            )
            .await
        }
        "workflow_save" => save(host, parse(args)?).await,
        "workflow_remove" => {
            let a: RemoveArgs = parse(args)?;
            require_coordinator(state)?;
            if !a.confirmed {
                return Err(invalid(format!(
                    "Confirm removing workflow {}: its definition cannot be recovered.",
                    a.id
                )));
            }
            if runs::has_active(state, &a.id).map_err(fail)? {
                return Err(conflict(format!(
                    "Workflow {} has a run that is not done or cancelled; let it finish or cancel it first.",
                    a.id
                )));
            }
            change(host, |map| {
                let before = map.workflows.len();
                map.workflows.retain(|w| w.id != a.id);
                if map.workflows.len() == before {
                    return Err(invalid(format!("There is no workflow {}.", a.id)));
                }
                Ok(())
            })
            .await?;
            Ok(json!({"removed":a.id}))
        }
        "workflow_block_save" => save_block(host, parse(args)?).await,
        "workflow_block_remove" => {
            let a: IdArgs = parse(args)?;
            require_coordinator(state)?;
            change(host, |map| {
                let users = core::using(&map.workflows, &a.id);
                if !users.is_empty() {
                    return Err(conflict(format!(
                        "Block {} is used by workflow {}; take it out of them first.",
                        a.id,
                        users.join(", ")
                    )));
                }
                let before = map.workflow_blocks.len();
                map.workflow_blocks.retain(|b| b.id != a.id);
                if map.workflow_blocks.len() == before {
                    return Err(invalid(format!("There is no workflow block {}.", a.id)));
                }
                Ok(())
            })
            .await?;
            Ok(json!({"removed":a.id}))
        }
        _ => Err(invalid("Unknown workflow operation.")),
    }
}

fn ensure_no_arguments(args: &Value) -> Result<(), String> {
    if args.as_object().is_some_and(|a| !a.is_empty()) {
        return Err(invalid("This operation takes no arguments."));
    }
    Ok(())
}

/// Change the map's workflows or blocks and commit it with the settings commit, under the
/// operations lock, from the map as it is now.
async fn change<T>(
    host: &Arc<Host>,
    edit: impl FnOnce(&mut KnowledgeMap) -> Result<T, String>,
) -> Result<T, String> {
    let state = &host.state;
    let _guard = state.operations.lock().await;
    if activity::active(state).await {
        return Err(conflict(
            "Activity registration is running. Wait for it to finish before changing workflows.",
        ));
    }
    let config = state.settings.config().map_err(fail)?;
    let mut map = state.settings.map().map_err(fail)?;
    let tunnel = state.settings.tunnel().map_err(fail)?;
    let out = edit(&mut map)?;
    core::validate(&map.workflows, &map.workflow_blocks).map_err(invalid)?;
    commit_settings(host, config, map, tunnel).await?;
    Ok(out)
}

async fn save(host: &Arc<Host>, args: SaveArgs) -> Result<Value, String> {
    let state = &host.state;
    require_coordinator(state)?;
    match (args.workflow, args.id, args.enabled) {
        // Enabling or disabling changes nothing else (what it allows was confirmed when saved).
        (None, Some(id), Some(enabled)) => {
            let workflow = change(host, |map| {
                let workflow = map
                    .workflows
                    .iter_mut()
                    .find(|w| w.id == id)
                    .ok_or_else(|| invalid(format!("There is no workflow {id}.")))?;
                workflow.enabled = enabled;
                Ok(workflow.clone())
            })
            .await?;
            Ok(json!({"workflow":shown(&workflow),"saved":true,"warnings":[]}))
        }
        (Some(value), None, None) => {
            let mut workflow: Workflow = serde_json::from_value(value)
                .map_err(|e| invalid(format!("Invalid workflow: {e}.")))?;
            workflow.updated_at = now();
            // Refuses checkouts keyed twice by one repository, then keys them in lowercase.
            core::validate_workflow(&workflow).map_err(invalid)?;
            workflow.allow.sort();
            // Lifting a fixed rule of its coding agents is the user's decision, confirmed with
            // what it lets them do.
            if let Some(question) = describe_workflow(&workflow)
                && !args.confirmed
            {
                return Err(invalid(format!(
                    "Confirm with the user, then save with confirmed: true. {question}"
                )));
            }
            workflow.checkouts = std::mem::take(&mut workflow.checkouts)
                .into_iter()
                .map(|(id, directory)| (id.to_lowercase(), directory))
                .collect();
            // Everything the map alone can refuse, before any machine or catalog is read.
            let mut candidate = state.settings.map().map_err(fail)?;
            upsert(&mut candidate.workflows, workflow.clone(), |w| &w.id);
            core::validate(&candidate.workflows, &candidate.workflow_blocks).map_err(invalid)?;
            let warnings = verify(host, &mut workflow).await?;
            let saved = workflow.clone();
            change(host, |map| {
                upsert(&mut map.workflows, workflow, |w| &w.id);
                Ok(())
            })
            .await?;
            Ok(json!({"workflow":shown(&saved),"saved":true,"warnings":warnings}))
        }
        _ => Err(invalid(
            "Give the workflow to save, or its id and enabled to enable or disable it.",
        )),
    }
}

fn upsert<T>(items: &mut Vec<T>, item: T, id: impl Fn(&T) -> &String) {
    match items.iter().position(|i| id(i) == id(&item)) {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
}

/// How long reading the project's catalog may take while a workflow is saved: with the one
/// round of machine calls after it (at most 40 seconds, run together), the save answers within
/// the control channel's 120-second client deadline, so nothing is committed after a client
/// gave up waiting.
const CATALOG_SECONDS: u64 = 60;

/// Gather what the workflow is checked against and check it; returns the warnings.
async fn verify(host: &Arc<Host>, workflow: &mut Workflow) -> Result<Vec<String>, String> {
    let state = &host.state;
    let machine = implementation::machine_facts(host, &workflow.machine)
        .await
        .map_err(fail)?
        .ok_or_else(|| {
            invalid(format!(
                "Machine {} is not an execution machine of this account (machine_list).",
                workflow.machine
            ))
        })?;
    if machine.own {
        // The shared server runs no coding agent for another account.
        authority::require_execution(state).map_err(|_| invalid(authority::EXECUTION_DENIED))?;
    }
    let listed = tokio::time::timeout(
        std::time::Duration::from_secs(CATALOG_SECONDS),
        tools::todo::repositories(state, None, Some(&workflow.organization), &workflow.project),
    )
    .await
    .map_err(|_| {
        "[network] The project's Git repositories could not be read in time.".to_owned()
    })?;
    let mut facts = Facts {
        project_known: false,
        catalog: Vec::new(),
        os: machine.os.clone(),
        roots: machine.roots.clone(),
        agents: Err("not asked".into()),
        checkouts: BTreeMap::new(),
    };
    match listed {
        Ok(listed) => {
            // The project's own spelling.
            if let Some(project) = listed["project"].as_str() {
                workflow.project = project.to_owned();
            }
            facts.catalog = serde_json::from_value(listed["repositories"].clone()).map_err(fail)?;
            facts.project_known = true;
        }
        // Refused at once, without asking the machine anything.
        Err(error) if error.starts_with("[invalid_input] invalid project") => {
            return core::check(workflow, &facts).map_err(invalid);
        }
        Err(error) => return Err(error),
    }
    let directories: Vec<(String, PathBuf)> = workflow
        .repositories
        .iter()
        .filter_map(|r| {
            let key = r.id.to_lowercase();
            workflow.checkouts.get(&key).map(|d| (key, d.clone()))
        })
        .collect();
    let paths: Vec<PathBuf> = directories.iter().map(|(_, d)| d.clone()).collect();
    let agents = async {
        if machine.own || machine.online {
            implementation::machine_call(
                host,
                &workflow.machine,
                "implementation_catalog",
                json!({}),
            )
            .await
            .map_err(|_| "the machine did not answer".to_owned())
        } else {
            Err("the machine is offline".to_owned())
        }
    };
    let (agents, inspections) =
        tokio::join!(agents, inspect(host, &workflow.machine, &machine, &paths));
    facts.agents = agents;
    facts.checkouts = directories
        .into_iter()
        .map(|(key, _)| key)
        .zip(inspections)
        .collect();
    core::check(workflow, &facts).map_err(invalid)
}

/// Every checkout as the machine sees it, asked in one `checkout_inspect` call, or unknown when
/// the machine is offline, its version predates the call (it would not answer it) or its
/// answer cannot be read.
async fn inspect(
    host: &Arc<Host>,
    id: &str,
    machine: &implementation::MachineFacts,
    directories: &[PathBuf],
) -> Vec<Inspection> {
    let unknown = |reason: &str| vec![Inspection::Unknown(reason.into()); directories.len()];
    if !(machine.own || machine.online) {
        return unknown("the machine is offline");
    }
    if !machine.inspect {
        return unknown("the machine's version cannot inspect checkouts; update it");
    }
    let answer = match implementation::machine_call(
        host,
        id,
        "checkout_inspect",
        json!({"paths":directories}),
    )
    .await
    {
        Ok(answer) => answer,
        Err(_) => return unknown("the machine did not answer"),
    };
    match answer["checkouts"].as_array() {
        Some(answers) if answers.len() == directories.len() => answers
            .iter()
            .map(implementation::checkout::inspection)
            .collect(),
        _ => unknown("the machine's answer could not be read"),
    }
}

/// The personal chat (Teams notes) is always the user's own.
const PERSONAL_CHAT: &str = "48:notes";
/// How long reading the user's chats may take while a block is saved.
const CHATS_SECONDS: u64 = 60;

/// Whether the user belongs to the chat, from the same chat list `pta teams chats` shows: a
/// refusal when the list says no, a warning when it cannot be read here (no Microsoft session
/// on this computer, or Graph did not answer). Mentions are resolved when a notification is
/// posted, against that chat's members.
async fn verify_chat(state: &DesktopState, chat: &str) -> Result<Option<String>, String> {
    if chat == PERSONAL_CHAT {
        return Ok(None);
    }
    let read = tokio::time::timeout(std::time::Duration::from_secs(CHATS_SECONDS), async {
        let graph = teams_graph(state).await?;
        graph.chat_directory(None).await
    })
    .await;
    match read {
        Ok(Ok(chats)) if chats.iter().any(|c| c["id"] == chat) => Ok(None),
        Ok(Ok(_)) => Err(invalid(format!(
            "You are not a member of the chat {chat}; choose one that pta teams chats lists."
        ))),
        _ => Ok(Some(format!(
            "The chat {chat} could not be checked against your chats (no Microsoft session could read them here); it is checked again when the block posts."
        ))),
    }
}

async fn save_block(host: &Arc<Host>, args: BlockSaveArgs) -> Result<Value, String> {
    let state = &host.state;
    require_coordinator(state)?;
    let mut block: Block =
        serde_json::from_value(args.block).map_err(|e| invalid(format!("Invalid block: {e}.")))?;
    block.updated_at = now();
    core::validate_block(&block).map_err(invalid)?;
    let mut warnings = Vec::new();
    // A block that acts is confirmed with what it does, on every surface.
    if block.kind != Kind::Instruction && !args.confirmed {
        return Err(invalid(format!(
            "Confirm this block with the user, then save it with confirmed: {}.",
            describe(&block)
        )));
    }
    match block.kind {
        Kind::Instruction => {}
        Kind::TeamsNotification => {
            let chat = block.chat.clone().unwrap_or_default();
            warnings.extend(verify_chat(state, &chat.id).await?);
        }
        Kind::Launcher => {
            let invocation = authority::invocation(state)
                .map_err(|_| conflict(authority::ACCOUNT_UNAVAILABLE))?;
            if !(invocation.scope.owner
                && matches!(
                    invocation.transport,
                    authority::Transport::Window | authority::Transport::Browser
                ))
            {
                return Err(conflict(
                    "Only the owner saves a launcher block, in this computer's application (the local page pta app open shows) or the owner's own portal.",
                ));
            }
            let reference = block.launcher.clone().unwrap_or_default();
            match consumers::launcher_approval(state, &reference.id) {
                None => {
                    return Err(invalid(format!(
                        "There is no launcher {} on the coordinator.",
                        reference.id
                    )));
                }
                Some((_, false)) => {
                    return Err(conflict(format!(
                        "Launcher {} is not approved yet; approve it first.",
                        reference.id
                    )));
                }
                Some((digest, true)) if digest != reference.digest => {
                    return Err(conflict(format!(
                        "Launcher {} is approved with another program digest; review it again.",
                        reference.id
                    )));
                }
                Some(_) => {}
            }
        }
    }
    let saved = block.clone();
    change(host, |map| {
        upsert(&mut map.workflow_blocks, block, |b| &b.id);
        Ok(())
    })
    .await?;
    let mut reply = json!({"block":saved,"saved":true,"warnings":warnings});
    if saved.kind != Kind::Instruction {
        reply["confirmation"] = json!(describe(&saved));
    }
    if saved.kind == Kind::TeamsNotification {
        reply["chat"] = json!(saved.chat);
        reply["mentions"] = json!(saved.mentions);
        reply["template"] = json!(saved.template);
    }
    Ok(reply)
}

#[cfg(test)]
mod tests;
