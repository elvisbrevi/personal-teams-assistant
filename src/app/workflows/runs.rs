//! The workflow run operations shared by the GUI, the CLI and MCP: start a run for an Azure
//! work item assigned to the user (a named workflow, or the director chooses), list runs, show
//! one, assign a workflow to an unassigned run, resume a stopped run, cancel one, restart one
//! with the user's handover note, and pause and unpause the runs of every project or of one.
//! They run on the coordinator, where the engine (`engine`) advances the runs; starting,
//! resuming, assigning, cancelling, restarting, pausing and unpausing need the user's
//! confirmation, because a run plans with the workflow's agent and, once the user confirmed the
//! children, codes each one with full permissions on the workflow's machine.
use super::engine::{self, Azure};
use super::record::{self, Item, Run, State as Phase};
use super::*;

pub(in crate::app) const METHODS: [&str; 9] = [
    "workflow_run_start",
    "workflow_runs",
    "workflow_run_status",
    "workflow_run_resume",
    "workflow_run_cancel",
    "workflow_run_assign",
    "workflow_run_restart",
    "workflow_runs_pause",
    "workflow_runs_unpause",
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
    /// The watcher started it (never from a request): the director chooses among the
    /// workflows that start by themselves.
    #[serde(skip)]
    auto: bool,
    /// The ID the run gets (never from a request): the watcher records it in its ledger before
    /// asking, so the row and the run agree.
    #[serde(skip)]
    run_id: Option<String>,
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
struct RestartArgs {
    run: String,
    /// The user's handover note for the new run's coding runs.
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    confirmed: bool,
    /// The note is the workflow supervisor's draft, unedited (never from the arguments).
    #[serde(skip)]
    drafted: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseArgs {
    /// One project's runs (by name, in any organization); every project's when left out.
    #[serde(default)]
    project: Option<String>,
    sessions: record::Sessions,
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnpauseArgs {
    #[serde(default)]
    project: Option<String>,
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
pub(in crate::app) const START_EFFECT: &str = "plan its children (read-only), wait for your review and confirmation of the plan (implementation_create), then code each created child, one after another, with full permissions on that computer and no further prompts, and wait for their pull requests to be completed (a task is coded directly, without a plan); along the way the workflow's Teams notification blocks post once at their stage, as you, and its launcher blocks run the approved launchers of this computer";
/// What starting a run does, as the user confirms it: the workflow by its ID (with its machine
/// and agent when the host knows them), or the director's choice.
pub(in crate::app) fn describe_start(
    workflow: Option<&str>,
    place: Option<&crate::workflows::Workflow>,
    item: u64,
) -> String {
    let what = START_EFFECT;
    let place_text = place
        .map(|w| {
            format!(
                " on machine {} with {} {} {},",
                w.machine, w.agent.provider, w.agent.model, w.agent.effort
            )
        })
        .unwrap_or_default();
    match workflow {
        Some(id) => format!(
            "Start workflow {id} for Azure work item {item}:{place_text} {what}{}",
            place.map(allow_effect).unwrap_or_default()
        ),
        None => format!(
            "Start a workflow run for Azure work item {item}: the workflow director chooses the workflow (or none, and the run waits for you to assign one); then, on that workflow's machine and agent, {what}; its coding agents may also do what that workflow allows (work items, Teams messages, merges or deployments, when it lists them in allow)"
        ),
    }
}
/// What a workflow's coding agents may do beyond the fixed rules, as a clause a start's
/// confirmation ends with; empty when it allows nothing.
pub(in crate::app) fn allow_effect(workflow: &crate::workflows::Workflow) -> String {
    let lifted = crate::workflows::describe_allow(&workflow.allow);
    if lifted.is_empty() {
        String::new()
    } else {
        format!("; beyond the fixed rules, {lifted}")
    }
}

/// Where the runs stand in the line for a slot and which pauses are in force.
struct Places {
    places: HashMap<String, usize>,
    pauses: Vec<record::Pause>,
}
/// The place in the line for a slot of every run waiting for one (1 is next), and the pauses.
fn places(state: &DesktopState) -> Result<Places, String> {
    let driven: Vec<Phase> = Phase::ALL.into_iter().filter(|s| s.driven()).collect();
    let mut runs = state.workflows.db.list(&driven, None, -1).map_err(fail)?;
    runs.reverse();
    let pauses = state.workflows.db.pauses().map_err(fail)?;
    Ok(Places {
        places: record::positions(&runs, now(), &pauses),
        pauses,
    })
}
/// A run's view or summary with whether it waits for a slot, its place in the line, and
/// whether a pause holds it (a run that is not done or cancelled, in a paused project).
fn placed(mut value: Value, run: &Run, places: &Places) -> Value {
    let place = places.places.get(&run.id);
    value["waiting_for_slot"] = json!(place.is_some());
    value["queue_position"] = json!(place);
    value["paused"] =
        json!(!run.state.finished() && record::paused(&places.pauses, &run.item.project));
    value
}
/// The pauses in force, as the operations show them.
pub(in crate::app) fn pauses_view(state: &DesktopState) -> Result<Value> {
    Ok(json!(state.workflows.db.pauses()?))
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
            let run = found(&host.state, &a.run)?;
            let mut view = placed(run.view(), &run, &places(&host.state)?);
            sessions(host, &mut view).await;
            view["supervising"] = json!(super::supervisor::queued(&host.state, &run.id));
            Ok(json!({ "run": view }))
        }
        "workflow_run_resume" => resume(host, parse(args)?).await,
        "workflow_run_cancel" => cancel(host, parse(args)?).await,
        "workflow_run_assign" => assign(host, parse(args)?).await,
        "workflow_run_restart" => restart(host, parse(args)?, azure).await,
        "workflow_runs_pause" => pause(host, parse(args)?).await,
        "workflow_runs_unpause" => unpause(&host.state, parse(args)?),
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
    let item = read_item(
        state,
        azure,
        args.source.as_deref(),
        args.project.as_deref(),
        args.id,
    )
    .await?;
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
    run.auto_start = args.auto;
    if let Some(id) = args.run_id {
        run.id = id;
    }
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
    let paused = record::paused(&db.pauses().map_err(fail)?, &run.item.project);
    Ok(json!({"run": run.view(), "started": true, "paused": paused}))
}

/// The work item a run is for, read fresh: an HU, bug or task assigned to the user.
async fn read_item(
    state: &DesktopState,
    azure: &Azure,
    source: Option<&str>,
    project: Option<&str>,
    id: u64,
) -> Result<Item, String> {
    let detail = azure.item(state, source, project, id).await?;
    let organization = crate::workflows::director::item_organization(&detail.item.url)
        .ok_or_else(|| invalid("The work item has no Azure DevOps link."))?;
    if !["requirement", "bug", "task"].contains(&detail.item.category.as_str()) {
        return Err(invalid(format!(
            "Work item {id} is a {}, not an HU, bug or task.",
            detail.item.kind
        )));
    }
    if !azure.assigned(state, source, &detail)? {
        return Err(invalid(format!(
            "Work item {id} is not assigned to you; a workflow runs only items assigned to you."
        )));
    }
    Ok(Item {
        provider: "azure".into(),
        id: detail.item.id,
        project: detail.item.project.clone(),
        organization: format!("https://dev.azure.com/{organization}"),
        url: detail.item.url.clone(),
        title: detail.item.title.chars().take(500).collect(),
        kind: detail.item.kind.clone(),
        category: detail.item.category.clone(),
        state: detail.item.state.clone(),
        source: source.map(str::to_owned),
    })
}

/// Start a run for an item on behalf of a caller that already holds the user's confirmation
/// (the watcher, which a configured ready state and an `auto_start` workflow authorize, and the
/// Implement entry): a named workflow, or the director's choice (`auto`: among the workflows that
/// start by themselves), under `run_id` when the caller recorded one.
pub(in crate::app) async fn start_confirmed(
    host: &Arc<Host>,
    azure: &Azure,
    item: (u64, Option<String>, Option<String>),
    workflow: Option<String>,
    auto: bool,
    run_id: Option<String>,
) -> Result<Value, String> {
    let (id, project, source) = item;
    start(
        host,
        StartArgs {
            provider: "azure".into(),
            id,
            project,
            source,
            director: workflow.is_none(),
            workflow,
            confirmed: true,
            auto,
            run_id,
        },
        azure,
    )
    .await
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
    let places = places(state)?;
    Ok(json!({
        "runs": runs.iter().map(|r| placed(r.summary(), r, &places)).collect::<Vec<_>>(),
        "attention": attention(state).map_err(fail)?,
        "paused": places.pauses,
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

/// Mark each implementation run in a run's view with whether its agent session is alive and
/// was handed to the user (`implementation::session_life`); unknown ones stay unmarked.
async fn sessions(host: &Arc<Host>, view: &mut Value) {
    async fn mark(host: &Arc<Host>, target: &mut Value) {
        let id = target["id"].as_str().unwrap_or_default().to_owned();
        if let Some(life) = implementation::session_life(host, &id).await {
            target["session_alive"] = life["session_alive"].clone();
            target["handed_to_user"] = life["handed_to_user"].clone();
        }
    }
    if view["planning"].is_object() {
        mark(host, &mut view["planning"]).await;
    }
    for index in 0..view["children"].as_array().map_or(0, Vec::len) {
        if view["children"][index]["coding"].is_object() {
            mark(host, &mut view["children"][index]["coding"]).await;
        }
    }
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
            "Confirm with the user, then cancel with confirmed: true. Cancel workflow run {}{}? It stops advancing for good and stops every session it started that is still alive; the file changes stay.",
            run.id,
            active
                .as_ref()
                .map(|a| format!(" and its active implementation run {}", a.id))
                .unwrap_or_default()
        )));
    }
    let engine::Ended {
        cancelled,
        stopped,
        warning,
    } = engine::end_sessions(host, &run).await;
    run.cancel(now());
    state.workflows.db.save(&run).map_err(fail)?;
    Ok(
        json!({"run": run.view(), "cancelled": true, "implementation_cancelled": cancelled, "sessions_stopped": stopped, "warning": warning}),
    )
}

/// The user's handover note as a restart stores it: trimmed, none when empty, refused when
/// longer than `record::MAX_HANDOVER` characters or holding control characters other than line
/// breaks and tabs.
pub(in crate::app) fn handover_note(note: Option<&str>) -> Result<Option<String>, String> {
    let Some(note) = note.map(|n| n.replace("\r\n", "\n")) else {
        return Ok(None);
    };
    let note = note.trim();
    if note.is_empty() {
        return Ok(None);
    }
    let length = note.chars().count();
    if length > record::MAX_HANDOVER {
        return Err(invalid(format!(
            "The handover note has {length} characters; it holds at most {}.",
            record::MAX_HANDOVER
        )));
    }
    if note
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(invalid(
            "The handover note holds control characters; give plain text.",
        ));
    }
    Ok(Some(note.to_owned()))
}
/// What restarting a run does, as the user confirms it: the run by its ID, its workflow (with
/// its machine and agent when the host knows them) and item, and the note's length.
pub(in crate::app) fn describe_restart(
    run: &str,
    workflow: Option<&crate::workflows::Workflow>,
    item: Option<u64>,
    note: Option<usize>,
) -> String {
    let workflow_text = match workflow {
        Some(w) => format!(
            "workflow {} on machine {} with {} {} {}",
            w.id, w.machine, w.agent.provider, w.agent.model, w.agent.effort
        ),
        None => "the same workflow".into(),
    };
    let item_text = item.map_or("the same work item".into(), |id| {
        format!("Azure work item {id}")
    });
    let note_text = match note {
        Some(n) => format!(
            "Each of its coding runs gets your handover note ({n} characters) under «{}», next to the item's evidence; it is stored with the new run and posted nowhere",
            engine::HANDOVER
        ),
        None => "No handover note is given".into(),
    };
    format!(
        "Restart workflow run {run}: it is cancelled (its active implementation run is cancelled and every session it started that is still alive is stopped; the file changes stay) and a new run of {workflow_text} starts for {item_text} from its first stage, which will {START_EFFECT}{}. {note_text}",
        workflow.map(allow_effect).unwrap_or_default()
    )
}

async fn restart(host: &Arc<Host>, args: RestartArgs, azure: &Azure) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let note = handover_note(args.note.as_deref())?;
    let lock = state.workflows.lock(&args.run);
    let _guard = lock.lock().await;
    let mut run = found(state, &args.run)?;
    if run.state.finished() {
        return Err(conflict(format!(
            "Workflow run {} is already {}; start a new run for its work item instead.",
            run.id,
            run.state.word()
        )));
    }
    let Some(workflow) = run.workflow.clone() else {
        return Err(conflict(format!(
            "Workflow run {} has no workflow yet ({}); assign one to it, or cancel it and start its work item again.",
            run.id,
            run.state.word()
        )));
    };
    let map = state.settings.map().map_err(fail)?;
    let workflow = usable(&map, &workflow, Some(&run.item))?;
    if !args.confirmed {
        return Err(invalid(format!(
            "Confirm with the user, then restart with confirmed: true. {}.",
            describe_restart(
                &run.id,
                Some(workflow),
                Some(run.item.id),
                note.as_ref().map(|n| n.chars().count())
            )
        )));
    }
    // Read before anything changes: an item that cannot be read, or is no longer the user's,
    // leaves the run as it is.
    let item = read_item(
        state,
        azure,
        run.item.source.as_deref(),
        Some(&run.item.project),
        run.item.id,
    )
    .await?;
    let ended = engine::end_sessions(host, &run).await;
    let now = now();
    let mut next = Run::new(item, now);
    next.continues = Some(run.id.clone());
    next.handover_drafted = args.drafted && note.is_some();
    next.handover = note;
    next.classification = Some(json!({"workflow":workflow.id,"outcome":"restarted"}));
    engine::assign(&mut next, workflow, &map.workflow_blocks, now, "restarted");
    run.restarted_as = Some(next.id.clone());
    run.cancel_as(now, "restarted");
    let db = &state.workflows.db;
    db.save(&run).map_err(fail)?;
    if !db.insert(&next).map_err(fail)? {
        return Err(conflict(format!(
            "Workflow run {} was cancelled, but work item {} already has another active run, so none was started for it.",
            run.id, next.item.id
        )));
    }
    let paused = record::paused(&db.pauses().map_err(fail)?, &next.item.project);
    Ok(json!({
        "run": next.view(),
        "restarted": true,
        "previous": run.id,
        "implementation_cancelled": ended.cancelled,
        "sessions_stopped": ended.stopped,
        "warning": ended.warning,
        "paused": paused,
    }))
}

/// Restart a run with the workflow supervisor's drafted handover, as the person confirmed it
/// unedited: its coding runs get the note as the supervisor's text.
pub(super) async fn restart_drafted(
    host: &Arc<Host>,
    run: &str,
    note: &str,
    azure: &Azure,
) -> Result<Value, String> {
    let args = RestartArgs {
        run: run.to_owned(),
        note: Some(note.to_owned()),
        confirmed: true,
        drafted: true,
    };
    restart(host, args, azure).await
}

/// A pause's project as given: trimmed, none for every project.
fn pause_project(project: Option<String>) -> Result<Option<String>, String> {
    let project = project
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty());
    if project.as_ref().is_some_and(|p| p.chars().count() > 256) {
        return Err(invalid("Give the project's name."));
    }
    Ok(project)
}
/// What pausing does, as the user confirms it.
pub(in crate::app) fn describe_pause(project: Option<&str>, sessions: record::Sessions) -> String {
    let scope = match project {
        Some(p) => format!("the workflow runs of project {p}"),
        None => "every workflow run".into(),
    };
    let sessions = match sessions {
        record::Sessions::Stop => {
            "every agent session of those runs that is still alive is stopped now (the file changes stay), and a run whose active implementation run is stopped becomes interrupted, to restart with a handover note or resume"
        }
        record::Sessions::Finish => {
            "the agent sessions running now are left to finish, and their runs record how they end"
        }
    };
    format!(
        "Pause {scope}: no run takes a slot, asks the director, launches a planning or coding run or runs an acting block, and the watcher starts no run, until you unpause; {sessions}. The runs keep their order in the line."
    )
}
/// What unpausing does, as the user confirms it.
pub(in crate::app) fn describe_unpause(project: Option<&str>) -> String {
    let scope = match project {
        Some(p) => format!("the workflow runs of project {p}"),
        None => "every paused workflow run".into(),
    };
    format!(
        "Unpause {scope}: the runs continue on their own in the order they were created, each coding run with full permissions on its workflow's machine, and the watcher starts runs again for items in a ready state."
    )
}

async fn pause(host: &Arc<Host>, args: PauseArgs) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let project = pause_project(args.project)?;
    if !args.confirmed {
        return Err(invalid(format!(
            "Confirm with the user, then pause with confirmed: true. {}",
            describe_pause(project.as_deref(), args.sessions)
        )));
    }
    let db = &state.workflows.db;
    let pause = record::Pause {
        project,
        sessions: args.sessions,
        paused_at: now(),
    };
    db.pause(&pause).map_err(fail)?;
    let (mut interrupted, mut stopped, mut warnings) = (Vec::new(), Vec::new(), Vec::new());
    if pause.sessions == record::Sessions::Stop {
        let open: Vec<Phase> = Phase::ALL.into_iter().filter(|s| !s.finished()).collect();
        for listed in db.list(&open, None, -1).map_err(fail)? {
            if !pause.covers(&listed.item.project) {
                continue;
            }
            // Under the run's lock, so a step in flight (a launch) ends first and is seen.
            let lock = state.workflows.lock(&listed.id);
            let _guard = lock.lock().await;
            let Some(mut run) = db.get(&listed.id).map_err(fail)? else {
                continue;
            };
            if run.state.finished() {
                continue;
            }
            let ended = engine::end_sessions(host, &run).await;
            if let Some(cancelled) = &ended.cancelled
                && run.state.driven()
            {
                run.stop(
                    Phase::Interrupted,
                    now(),
                    format!(
                        "The workflow runs were paused with their sessions stopped, so implementation run {cancelled} was stopped; restart this run with a handover note (workflow_run_restart), or resume it to continue past that run."
                    ),
                );
                // The person stopped it: the supervisor does not look at this stop.
                run.cause = Some("paused".into());
                db.save(&run).map_err(fail)?;
                interrupted.push(run.id.clone());
            }
            stopped.extend(ended.cancelled);
            stopped.extend(ended.stopped);
            warnings.extend(ended.warning);
        }
    }
    Ok(json!({
        "pause": pause,
        "paused": pauses_view(state).map_err(fail)?,
        "interrupted": interrupted,
        "sessions_stopped": stopped,
        "warning": warnings.into_iter().reduce(|all, next| format!("{all} {next}")),
    }))
}

fn unpause(state: &DesktopState, args: UnpauseArgs) -> Result<Value, String> {
    require_engine(state)?;
    let project = pause_project(args.project)?;
    if !args.confirmed {
        return Err(invalid(format!(
            "Confirm with the user, then unpause with confirmed: true. {}",
            describe_unpause(project.as_deref())
        )));
    }
    let db = &state.workflows.db;
    if db.unpause(project.as_deref()).map_err(fail)? == 0 {
        let paused = db.pauses().map_err(fail)?;
        return Err(conflict(match (&project, paused.is_empty()) {
            (_, true) => "No workflow run is paused.".to_owned(),
            (Some(p), false) => format!(
                "Project {p} has no pause of its own; {} paused. Unpause without a project to end every pause.",
                paused
                    .iter()
                    .map(|p| p
                        .project
                        .as_deref()
                        .map_or("every project is".to_owned(), |p| format!("{p} is")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            (None, false) => "No workflow run is paused.".to_owned(),
        }));
    }
    Ok(json!({"unpaused": true, "paused": pauses_view(state).map_err(fail)?}))
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
pub(super) mod tests;
