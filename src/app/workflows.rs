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
//! (`save_settings`).
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

pub(super) fn handles(method: &str) -> bool {
    METHODS.contains(&method)
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
        Err(message) => {
            let (code, exit, prefix) = if message.starts_with("[invalid_input] ") {
                ("invalid_input", 2, "[invalid_input] ")
            } else if message.starts_with("[conflict] ") {
                ("state_conflict", 6, "[conflict] ")
            } else if message.starts_with("[not_ready] ") {
                ("not_ready", 3, "[not_ready] ")
            } else if message.starts_with("[network] ") {
                ("dependency_or_network", 5, "[network] ")
            } else {
                ("operation_failed", 1, "")
            };
            control::Reply::error(code, exit, message.strip_prefix(prefix).unwrap_or(&message))
        }
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
                "At stage {stage}, post «{}» to the Teams chat {} ({}), mentioning {mentions}, once per run",
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
    match method {
        "workflows_list" => {
            ensure_no_arguments(args)?;
            let map = state.settings.map().map_err(fail)?;
            Ok(json!({"workflows":map.workflows,"editable":editable(state)?}))
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
            Ok(json!({"workflow":workflow,"blocks":blocks,"editable":editable(state)?}))
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
            // Workflow runs do not exist yet; once they do, an active run refuses this.
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
        // Enabling or disabling changes nothing else.
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
            Ok(json!({"workflow":workflow,"saved":true,"warnings":[]}))
        }
        (Some(value), None, None) => {
            let mut workflow: Workflow = serde_json::from_value(value)
                .map_err(|e| invalid(format!("Invalid workflow: {e}.")))?;
            workflow.updated_at = now();
            core::validate_workflow(&workflow).map_err(invalid)?;
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
            Ok(json!({"workflow":saved,"saved":true,"warnings":warnings}))
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
    let (project_known, catalog) = match tools::todo::repositories(
        state,
        None,
        Some(&workflow.organization),
        &workflow.project,
    )
    .await
    {
        Ok(listed) => {
            // The project's own spelling.
            if let Some(project) = listed["project"].as_str() {
                workflow.project = project.to_owned();
            }
            let catalog = serde_json::from_value(listed["repositories"].clone()).map_err(fail)?;
            (true, catalog)
        }
        Err(error) if error.starts_with("[invalid_input] invalid project") => (false, Vec::new()),
        Err(error) => return Err(error),
    };
    let reachable = machine.own || machine.online;
    let agents = if reachable {
        implementation::machine_call(host, &workflow.machine, "implementation_catalog", json!({}))
            .await
            .map_err(|_| "the machine did not answer".to_owned())
    } else {
        Err("the machine is offline".to_owned())
    };
    let mut checkouts = BTreeMap::new();
    if project_known {
        for repository in &workflow.repositories {
            let Some((_, directory)) = workflow
                .checkouts
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(&repository.id))
            else {
                continue;
            };
            let inspection = inspect(host, &workflow.machine, &machine, directory).await;
            checkouts.insert(repository.id.to_lowercase(), inspection);
        }
    }
    core::check(
        workflow,
        &Facts {
            project_known,
            catalog,
            roots: machine.roots,
            agents,
            checkouts,
        },
    )
    .map_err(invalid)
}

/// One checkout as the machine sees it: asked once with `checkout_inspect`, or unknown when the
/// machine is offline or its version predates the call (it would not answer it).
async fn inspect(
    host: &Arc<Host>,
    id: &str,
    machine: &implementation::MachineFacts,
    directory: &Path,
) -> Inspection {
    if !(machine.own || machine.online) {
        return Inspection::Unknown("the machine is offline".into());
    }
    if !machine.inspect {
        return Inspection::Unknown(
            "the machine's version cannot inspect checkouts; update it".into(),
        );
    }
    match implementation::machine_call(host, id, "checkout_inspect", json!({"path":directory}))
        .await
    {
        Ok(answer) => implementation::checkout::inspection(&answer),
        Err(_) => Inspection::Unknown("the machine did not answer".into()),
    }
}

async fn save_block(host: &Arc<Host>, args: BlockSaveArgs) -> Result<Value, String> {
    let state = &host.state;
    require_coordinator(state)?;
    let mut block: Block =
        serde_json::from_value(args.block).map_err(|e| invalid(format!("Invalid block: {e}.")))?;
    block.updated_at = now();
    core::validate_block(&block).map_err(invalid)?;
    match block.kind {
        Kind::Instruction => {}
        Kind::TeamsNotification => {
            if !args.confirmed {
                return Err(invalid(format!(
                    "Confirm this Teams notification block with the user, then save it with confirmed: {}.",
                    describe(&block)
                )));
            }
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
                    "Only the owner saves a launcher block, in this computer's application (the local page pta app open shows) or the owner's own portal, as when approving a launcher.",
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
    let mut reply = json!({"block":saved,"saved":true});
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
