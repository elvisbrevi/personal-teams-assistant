//! The workflow choice of the Implement operation (`implementation_start` with `workflow`):
//! `none` is the plain implementation run, unchanged; a workflow ID starts a workflow run for
//! the item through the engine's start path, with the workflow's machine, checkout
//! directories, agent, model, effort and instructions, so the form does not send them (and a
//! different machine, directories or agent is a plain run without a workflow); `director`
//! only classifies: it asks the workflow director, returns the choice and its reason, and
//! starts nothing, so the person confirms (or picks another workflow) with a second call
//! naming the workflow.
use super::engine::Azure;
use super::*;

/// The Implement item selection.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    provider: String,
    id: u64,
    project: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    task: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    item: Item,
    workflow: String,
    /// What a workflow run takes from its definition: given, it is refused.
    #[serde(default)]
    machine: Option<String>,
    #[serde(default)]
    directories: Vec<PathBuf>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    context: Option<String>,
}

/// The workflow an `implementation_start` request names, when it is not `none`.
pub(in crate::app) fn choice(args: &Value) -> Option<&str> {
    args.get("workflow")
        .and_then(Value::as_str)
        .filter(|w| *w != "none")
}

/// The request as the plain implementation run reads it: without its `workflow: none`.
pub(in crate::app) fn plain(args: &Value) -> Value {
    let mut args = args.clone();
    if let Some(object) = args.as_object_mut() {
        object.remove("workflow");
    }
    args
}

pub(in crate::app) async fn start(
    host: &Arc<Host>,
    args: &Value,
    azure: &Azure,
) -> Result<Value, String> {
    let state = &host.state;
    let a: Args = parse(args)?;
    if a.item.provider != "azure" {
        return Err(invalid(
            "Workflows cover Azure DevOps work items: start a GitHub item without a workflow.",
        ));
    }
    let given = a.machine.is_some()
        || !a.directories.is_empty()
        || a.provider.is_some()
        || a.model.is_some()
        || a.effort.is_some()
        || a.context.as_deref().is_some_and(|c| !c.trim().is_empty());
    if given {
        return Err(invalid(
            "A workflow run takes its machine, directories, agent, model, effort and instructions from the workflow: omit them, or start without a workflow (workflow none) to choose them yourself.",
        ));
    }
    require_coordinator(state)?;
    if a.workflow == "director" {
        let mut choice = super::director::classify_for(
            host,
            "azure",
            a.item.id,
            Some(a.item.project.clone()),
            a.item.source.clone(),
        )
        .await?;
        choice["started"] = json!(false);
        choice["next"] = json!(
            "Nothing was started. Show the user the workflow and its reason; to start it, call implementation_start again with workflow set to that ID (or to another workflow of the item's project, or none)."
        );
        return Ok(choice);
    }
    let started = super::runs::start_confirmed(
        host,
        azure,
        (a.item.id, Some(a.item.project), a.item.source),
        Some(a.workflow.clone()),
        false,
    )
    .await?;
    Ok(json!({"workflow": a.workflow, "started": true, "run": started["run"]}))
}
