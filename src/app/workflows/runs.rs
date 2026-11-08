//! The workflow run operations shared by the GUI, the CLI and MCP: start a run for an Azure
//! work item assigned to the user (a named workflow, or the director chooses), list runs, show
//! one, assign a workflow to an unassigned run, resume a stopped run and cancel one. They run
//! on the coordinator, where the engine (`engine`) advances the runs; starting, resuming,
//! assigning and cancelling need the user's confirmation, because a run plans with the
//! workflow's agent and, once the user confirmed the children, codes each one with full
//! permissions on the workflow's machine.
use super::engine::{self, Azure};
use super::record::{Item, Run, State as Phase};
use super::*;

pub(in crate::app) const METHODS: [&str; 6] = [
    "workflow_run_start",
    "workflow_runs",
    "workflow_run_status",
    "workflow_run_resume",
    "workflow_run_cancel",
    "workflow_run_assign",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartArgs {
    provider: String,
    id: u64,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    workflow: Option<String>,
    #[serde(default)]
    director: bool,
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    workflow: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    run: String,
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignArgs {
    run: String,
    workflow: String,
    #[serde(default)]
    confirmed: bool,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// What a started run does, as the user confirms it.
pub(in crate::app) const START_EFFECT: &str = "plan its children (read-only), wait for your review and confirmation of the plan (implementation_create), then code each created child, one after another, with full permissions on that computer and no further prompts, and wait for their pull requests to be completed (a task is coded directly, without a plan)";
/// What starting a run does, as the user confirms it: the workflow by its ID (with its machine
/// and agent when the host knows them), or the director's choice.
pub(in crate::app) fn describe_start(
    workflow: Option<&str>,
    place: Option<&crate::workflows::Workflow>,
    item: u64,
) -> String {
    let what = START_EFFECT;
    let place = place
        .map(|w| {
            format!(
                " on machine {} with {} {} {},",
                w.machine, w.agent.provider, w.agent.model, w.agent.effort
            )
        })
        .unwrap_or_default();
    match workflow {
        Some(id) => format!("Start workflow {id} for Azure work item {item}:{place} {what}"),
        None => format!(
            "Start a workflow run for Azure work item {item}: the workflow director chooses the workflow (or none, and the run waits for you to assign one); then, on that workflow's machine and agent, {what}"
        ),
    }
}

pub(super) fn found(state: &DesktopState, id: &str) -> Result<Run, String> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(invalid("Give the workflow run's ID."));
    }
    state
        .workflows
        .db
        .get(id)
        .map_err(fail)?
        .ok_or_else(|| invalid(format!("There is no workflow run {id}.")))
}

/// Runs advance only on the owner's coordinator.
pub(super) fn require_engine(state: &DesktopState) -> Result<(), String> {
    require_coordinator(state)?;
    if !engine::may_run(state) {
        return Err("[not_ready] Workflow runs advance only on the account owner's coordinator; start them there.".into());
    }
    Ok(())
}

/// An enabled workflow of the item's project.
fn usable<'a>(
    map: &'a KnowledgeMap,
    id: &str,
    item: Option<&Item>,
) -> Result<&'a crate::workflows::Workflow, String> {
    let workflow = map
        .workflows
        .iter()
        .find(|w| w.id == id)
        .ok_or_else(|| invalid(format!("There is no workflow {id}.")))?;
    if !workflow.enabled {
        return Err(conflict(format!(
            "Workflow {id} is disabled, and a disabled workflow starts no run; enable it first."
        )));
    }
    if let Some(item) = item
        && !engine::same_project(workflow, item)
    {
        return Err(invalid(format!(
            "Workflow {id} covers project {} of {}; work item {} belongs to project {} of {}.",
            workflow.project, workflow.organization, item.id, item.project, item.organization
        )));
    }
    Ok(workflow)
}

pub(super) async fn operation(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
    azure: &Azure,
) -> Result<Value, String> {
    match method {
        "workflow_run_start" => start(host, parse(args)?, azure).await,
        "workflow_runs" => list(&host.state, parse(args)?),
        "workflow_run_status" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct StatusArgs {
                run: String,
            }
            let a: StatusArgs = parse(args)?;
            Ok(json!({"run": found(&host.state, &a.run)?.view()}))
        }
        "workflow_run_resume" => resume(host, parse(args)?).await,
        "workflow_run_cancel" => cancel(host, parse(args)?).await,
        "workflow_run_assign" => assign(host, parse(args)?).await,
        _ => Err(invalid("Unknown workflow operation.")),
    }
}

async fn start(host: &Arc<Host>, args: StartArgs, azure: &Azure) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    if args.provider != "azure" {
        return Err(invalid(
            "Workflows cover Azure DevOps work items: give provider azure.",
        ));
    }
    if args.workflow.is_some() == args.director {
        return Err(invalid(
            "Give the workflow to run, or director: true for the workflow director to choose it.",
        ));
    }
    let map = state.settings.map().map_err(fail)?;
    let named = args
        .workflow
        .as_deref()
        .map(|id| usable(&map, id, None))
        .transpose()?;
    if !args.confirmed {
        return Err(invalid(format!(
            "Confirm with the user, then start with confirmed: true. {}.",
            describe_start(named.map(|w| w.id.as_str()), named, args.id)
        )));
    }
    let detail = azure
        .item(
            state,
            args.source.as_deref(),
            args.project.as_deref(),
            args.id,
        )
        .await?;
    let organization = crate::workflows::director::item_organization(&detail.item.url)
        .ok_or_else(|| invalid("The work item has no Azure DevOps link."))?;
    if !["requirement", "bug", "task"].contains(&detail.item.category.as_str()) {
        return Err(invalid(format!(
            "Work item {} is a {}, not an HU, bug or task.",
            args.id, detail.item.kind
        )));
    }
    if !azure.assigned(state, args.source.as_deref(), &detail)? {
        return Err(invalid(format!(
            "Work item {} is not assigned to you; a workflow runs only items assigned to you.",
            args.id
        )));
    }
    let item = Item {
        provider: "azure".into(),
        id: detail.item.id,
        project: detail.item.project.clone(),
        organization: format!("https://dev.azure.com/{organization}"),
        url: detail.item.url.clone(),
        title: detail.item.title.chars().take(500).collect(),
        kind: detail.item.kind.clone(),
        category: detail.item.category.clone(),
        state: detail.item.state.clone(),
        source: args.source.clone(),
    };
    if let Some(workflow) = named {
        usable(&map, &workflow.id, Some(&item))?;
    }
    let db = &state.workflows.db;
    if let Some(active) = db.active_for(&item.url).map_err(fail)? {
        return Err(conflict(format!(
            "Work item {} already has an active workflow run {} ({}); cancel it or let it finish before starting another.",
            item.id,
            active.id,
            active.state.word()
        )));
    }
    let now = now();
    let mut run = Run::new(item, now);
    if let Some(workflow) = named {
        run.classification = Some(json!({"workflow":workflow.id,"outcome":"named"}));
        engine::assign(&mut run, workflow, &map.workflow_blocks, now, "named");
    }
    if !db.insert(&run).map_err(fail)? {
        return Err(conflict(format!(
            "Work item {} already has an active workflow run.",
            run.item.id
        )));
    }
    Ok(json!({"run": run.view(), "started": true}))
}

fn list(state: &DesktopState, args: ListArgs) -> Result<Value, String> {
    let states = match &args.state {
        Some(word) => vec![Phase::parse(word).ok_or_else(|| {
            invalid(format!(
                "Unknown state {word}; use one of {}.",
                Phase::ALL.map(Phase::word).join(", ")
            ))
        })?],
        None => Vec::new(),
    };
    let limit = args.limit.unwrap_or(20);
    if !(1..=100).contains(&limit) {
        return Err(invalid("limit is between 1 and 100."));
    }
    let runs = state
        .workflows
        .db
        .list(&states, args.workflow.as_deref(), limit)
        .map_err(fail)?;
    Ok(json!({
        "runs": runs.iter().map(Run::summary).collect::<Vec<_>>(),
        "attention": attention(state).map_err(fail)?,
    }))
}

/// The runs waiting on the user (a plan to review, a workflow to assign, something to look at,
/// a resume): the attention items Home shows.
pub(in crate::app) fn attention(state: &DesktopState) -> Result<Vec<Value>> {
    let waiting: Vec<Phase> = Phase::ALL
        .into_iter()
        .filter(|s| s.waits_on_user())
        .collect();
    Ok(state
        .workflows
        .db
        .list(&waiting, None, 50)?
        .iter()
        .map(Run::summary)
        .collect())
}

async fn resume(host: &Arc<Host>, args: RunArgs) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let lock = state.workflows.lock(&args.run);
    let _guard = lock.lock().await;
    let mut run = found(state, &args.run)?;
    if !run.state.stopped() {
        return Err(conflict(format!(
            "Workflow run {} is {}; only a failed, interrupted or needs_attention run resumes.",
            run.id,
            run.state.word()
        )));
    }
    let stage = run.stopped_in.map(Phase::word).unwrap_or("unknown");
    if !args.confirmed {
        return Err(invalid(format!(
            "Confirm with the user, then resume with confirmed: true. Resume workflow run {} from stage {stage} ({}): what stopped it is accepted and the run continues on its own; no plan is submitted again and a started coding run is not started again.",
            run.id,
            run.attention.as_deref().unwrap_or("no reason recorded")
        )));
    }
    let accepted = engine::resume(host, &mut run, now()).await.map_err(fail)?;
    state.workflows.db.save(&run).map_err(fail)?;
    Ok(json!({"run": run.view(), "resumed": true, "accepted": accepted}))
}

async fn cancel(host: &Arc<Host>, args: RunArgs) -> Result<Value, String> {
    let state = &host.state;
    require_coordinator(state)?;
    let lock = state.workflows.lock(&args.run);
    let _guard = lock.lock().await;
    let mut run = found(state, &args.run)?;
    if run.state.finished() {
        return Err(conflict(format!(
            "Workflow run {} is already {}.",
            run.id,
            run.state.word()
        )));
    }
    let active = run.active_implementation().cloned();
    if !args.confirmed {
        return Err(invalid(format!(
            "Confirm with the user, then cancel with confirmed: true. Cancel workflow run {}{}? It stops advancing for good; the file changes stay.",
            run.id,
            active
                .as_ref()
                .map(|a| format!(" and its active implementation run {}", a.id))
                .unwrap_or_default()
        )));
    }
    let mut cancelled = None;
    let mut warning = None;
    if let Some(active) = &active {
        // Only a run still in flight is cancelled. When its machine cannot be asked, the run
        // may be running there (even one whose launch was not confirmed yet): the cancellation
        // is tried, and when it does not reach the machine the reply says where to cancel it.
        let present = match implementation::observe(host, &active.machine, &active.id).await {
            Ok(Some(observed)) => {
                matches!(observed["state"].as_str(), Some("running" | "planning"))
            }
            Ok(None) => false,
            Err(_) => true,
        };
        if present {
            match implementation::cancel_run(host, &active.machine, &active.id).await {
                Ok(()) => cancelled = Some(active.id.clone()),
                Err(_) => {
                    warning = Some(format!(
                        "Implementation run {} may still be running on machine {}, which could not be reached; once it is reachable, cancel it there with implementation_cancel (machine {}, id {}).",
                        active.id, active.machine, active.machine, active.id
                    ))
                }
            }
        }
    }
    run.cancel(now());
    state.workflows.db.save(&run).map_err(fail)?;
    Ok(
        json!({"run": run.view(), "cancelled": true, "implementation_cancelled": cancelled, "warning": warning}),
    )
}

async fn assign(host: &Arc<Host>, args: AssignArgs) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let lock = state.workflows.lock(&args.run);
    let _guard = lock.lock().await;
    let mut run = found(state, &args.run)?;
    if run.state != Phase::Unassigned {
        return Err(conflict(format!(
            "Workflow run {} is {}; only an unassigned run gets a workflow.",
            run.id,
            run.state.word()
        )));
    }
    let map = state.settings.map().map_err(fail)?;
    let workflow = usable(&map, &args.workflow, Some(&run.item))?;
    if !args.confirmed {
        return Err(invalid(format!(
            "Confirm with the user, then assign with confirmed: true. Assign workflow {} to run {}: {}.",
            workflow.id,
            run.id,
            describe_start(Some(&workflow.id), Some(workflow), run.item.id)
        )));
    }
    engine::assign(
        &mut run,
        workflow,
        &map.workflow_blocks,
        now(),
        &format!("assigned: {}", workflow.id),
    );
    state.workflows.db.save(&run).map_err(fail)?;
    Ok(json!({"run": run.view(), "assigned": true}))
}

/// Whether a workflow has a run that is not done or cancelled (it cannot be removed then).
pub(in crate::app) fn has_active(state: &DesktopState, workflow: &str) -> Result<bool> {
    let active: Vec<Phase> = Phase::ALL.into_iter().filter(|s| !s.finished()).collect();
    Ok(!state
        .workflows
        .db
        .list(&active, Some(workflow), 1)?
        .is_empty())
}

#[cfg(all(test, unix))]
mod tests;
