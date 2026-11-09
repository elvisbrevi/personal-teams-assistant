//! The workflow run engine: a tracked task of the coordinator's control loop that, every 30
//! seconds, advances the workflow runs (`record`) through the existing implementation
//! operations and observes their records. It never starts a provider process itself (it asks
//! for implementation runs with `implementation_launch`, the call `implementation_start`
//! makes), never creates children (the user does, with `implementation_create`; the engine
//! reads the creation ledger), and never writes to Azure DevOps or merges, completes or changes
//! a pull request: it reads them.
//!
//! Each step is durable before it acts: the ID of an implementation run is recorded before the
//! machine is asked to launch it, and the launch is idempotent by that ID, so an interruption
//! anywhere resumes from the last recorded stage without a second start. Only the coordinator
//! advances runs (never a computer signed in to one, never a shared portal account that is not
//! the owner's), at most `concurrency` of them in the stages that take a slot.
use super::acting;
use super::record::{Child, Implementation, Item, Run, State as Phase, Store};
use super::*;
use crate::ado::workflow::PullRequest;
use crate::tools::todo::Detail;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// How often the engine looks at the runs.
pub(in crate::app) const TICK_SECONDS: u64 = 30;
/// How often one run's pull requests are read while it waits for them.
pub(in crate::app) const PULL_REQUEST_POLL_SECONDS: i64 = 300;
/// At most this many steps of one run in one tick.
const STEPS: usize = 16;
/// A coding session reports the branch it pushed on a line of its report that starts so.
const BRANCH: &str = "Branch:";
const CONTEXT: usize = 32_000;

/// The engine's state on the profile: the runs' store and what advances now.
pub(in crate::app) struct State {
    pub(in crate::app) db: Store,
    /// The ready-state watcher: its seen ledger and last poll (`watcher`).
    pub(in crate::app) watcher: super::watcher::Watcher,
    locks: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
    admission: Mutex<()>,
    advancing: AtomicUsize,
    recovered: AtomicBool,
    /// The attempts left without a result were settled once after a start.
    settled: AtomicBool,
    /// The runs whose advance is running now, each in its own task.
    inflight: std::sync::Mutex<std::collections::HashSet<String>>,
}
impl State {
    pub(in crate::app) fn new(profile: &Path) -> Result<Self> {
        let root = profile.join("implementation");
        security::private_dir(&root)?;
        Ok(Self {
            db: Store::open(&root.join("workflows.db"))?,
            watcher: super::watcher::Watcher::open(&root.join("workflows.db"))?,
            locks: std::sync::Mutex::new(HashMap::new()),
            admission: Mutex::new(()),
            advancing: AtomicUsize::new(0),
            recovered: AtomicBool::new(false),
            settled: AtomicBool::new(false),
            inflight: std::sync::Mutex::new(std::collections::HashSet::new()),
        })
    }
    /// The lock every change of one run holds: a step of the engine, a cancel, a resume.
    pub(in crate::app) fn lock(&self, id: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_default()
            .clone()
    }
    /// Whether a run is advancing a stage now (the updater waits).
    pub(in crate::app) fn advancing(&self) -> bool {
        self.advancing.load(Ordering::SeqCst) > 0
    }
    /// Whether the look after a start found every active implementation run.
    pub(in crate::app) fn recovered(&self) -> bool {
        self.recovered.load(Ordering::SeqCst)
    }
}
/// A run's advance in flight; dropped when its task ends.
struct Inflight(Arc<Host>, String);
impl Drop for Inflight {
    fn drop(&mut self) {
        self.0
            .state
            .workflows
            .inflight
            .lock()
            .unwrap()
            .remove(&self.1);
    }
}
struct Advancing<'a>(&'a AtomicUsize);
impl<'a> Advancing<'a> {
    fn new(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}
impl Drop for Advancing<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Where the engine reads Azure DevOps: the enabled source with its read credential, or a
/// test's synthetic catalog. Neither has a way to write.
#[derive(Clone)]
pub(in crate::app) enum Azure {
    Live,
    #[cfg(test)]
    Synthetic(Arc<super::synthetic::Catalog>),
}
impl Azure {
    /// An item of any category, read fresh.
    pub(in crate::app) async fn item(
        &self,
        state: &DesktopState,
        source: Option<&str>,
        project: Option<&str>,
        id: u64,
    ) -> Result<Detail, String> {
        match self {
            Self::Live => {
                let body = json!({"id":id,"project":project,"source":source});
                let value = tools::todo::read(state, "get_azure_item", &body).await?;
                serde_json::from_value(value).map_err(fail)
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => catalog.item(id),
        }
    }
    pub(in crate::app) fn assigned(
        &self,
        state: &DesktopState,
        source: Option<&str>,
        detail: &Detail,
    ) -> Result<bool, String> {
        match self {
            Self::Live => tools::todo::assigned(state, source, detail),
            #[cfg(test)]
            Self::Synthetic(catalog) => Ok(catalog.assigned(detail.item.id)),
        }
    }
    async fn pull_requests(
        &self,
        state: &DesktopState,
        run: &Run,
        repository: &crate::workflows::Repository,
        branch: &str,
    ) -> Result<Vec<PullRequest>, String> {
        match self {
            Self::Live => {
                tools::todo::pull_requests(
                    state,
                    run.item.source.as_deref(),
                    &run.item.organization,
                    &run.item.project,
                    (&repository.id, &repository.name),
                    branch,
                )
                .await
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => catalog.pull_requests(&repository.id, branch),
        }
    }
}

/// Whether this installation advances workflow runs: the account's coordinator, with the
/// owner's authority.
pub(in crate::app) fn may_run(state: &DesktopState) -> bool {
    implementation::link(state).is_ok_and(|l| l.is_none())
        && authority::invocation(state).is_ok_and(|i| i.scope.owner)
}

/// The workflow a run is assigned now: its definition and blocks as they are, and the stage it
/// begins with (planning, or coding for a task-category item, which is its own child).
pub(in crate::app) fn assign(
    run: &mut Run,
    workflow: &crate::workflows::Workflow,
    blocks: &[crate::workflows::Block],
    now: i64,
    outcome: &str,
) {
    run.workflow = Some(workflow.id.clone());
    run.definition = Some(workflow.clone());
    run.blocks = workflow
        .blocks
        .iter()
        .filter_map(|id| blocks.iter().find(|b| &b.id == id).cloned())
        .collect();
    if run.task() {
        run.children = vec![Child {
            id: run.item.id,
            title: run.item.title.clone(),
            url: run.item.url.clone(),
            kind: run.item.kind.clone(),
            ..Default::default()
        }];
        run.advance(Phase::Coding, now, outcome);
    } else {
        run.advance(Phase::Planning, now, outcome);
    }
}

/// One look at every run: recover once after a start, then advance each run the engine drives.
pub(in crate::app) async fn tick(
    host: &Arc<Host>,
    azure: &Azure,
    now: i64,
    poll: i64,
) -> Result<()> {
    let state = &host.state.workflows;
    if !may_run(&host.state) {
        return Ok(());
    }
    // Until every active implementation run has been looked at once, the runs whose run could
    // not be asked wait: the normal step would read an interruption as an ending.
    let unrecovered = if state.recovered() {
        std::collections::HashSet::new()
    } else {
        let unrecovered = recover(host).await?;
        if unrecovered.is_empty() {
            state.recovered.store(true, Ordering::SeqCst);
        }
        unrecovered
    };
    // After a start, every acting attempt left without a result is settled, in every run.
    if !state.settled.load(Ordering::SeqCst) {
        acting::settle_all(host, now).await?;
        state.settled.store(true, Ordering::SeqCst);
    }
    // A failed run is not advanced, but its `failed` blocks may be due (the host stopped before
    // they ran, or a launcher was busy): they stay in the sweep until each has an outcome.
    let mut tasks = Vec::new();
    for run in state.db.list(&[Phase::Failed], None, -1)? {
        if acting::has_due(&run) && state.inflight.lock().unwrap().insert(run.id.clone()) {
            let guard = Inflight(host.clone(), run.id.clone());
            let (host, id) = (host.clone(), run.id.clone());
            tasks.push(tokio::spawn(async move {
                let _guard = guard;
                let _ = acting::execute(&host, &id, now).await;
            }));
        }
    }
    let driven: Vec<Phase> = Phase::ALL.into_iter().filter(|s| s.driven()).collect();
    let mut runs = state.db.list(&driven, None, -1)?;
    // The oldest first: they take the free slots first.
    runs.reverse();
    for run in runs {
        if unrecovered.contains(&run.id) || !state.inflight.lock().unwrap().insert(run.id.clone()) {
            continue;
        }
        // Each run in its own task: a slow step (a director session) never holds the others.
        let guard = Inflight(host.clone(), run.id.clone());
        let (host, azure, id) = (host.clone(), azure.clone(), run.id.clone());
        tasks.push(tokio::spawn(async move {
            let _guard = guard;
            if advance(&host, &azure, &id, now, poll).await.is_err() {
                tracing::warn!(event = "workflow_run_not_advanced");
            }
        }));
    }
    // A tick waits for its steps at most until the next one; a run still advancing then is
    // skipped by the next tick and keeps going on its own.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(TICK_SECONDS),
        futures_util::future::join_all(tasks),
    )
    .await;
    Ok(())
}

/// After a restart: a run whose active implementation run was interrupted is `interrupted`.
/// Returns the runs whose implementation run could not be asked (its machine is unreachable).
async fn recover(host: &Arc<Host>) -> Result<std::collections::HashSet<String>> {
    let state = &host.state.workflows;
    let mut unrecovered = std::collections::HashSet::new();
    for run in state.db.list(&[Phase::Planning, Phase::Coding], None, -1)? {
        let lock = state.lock(&run.id);
        let _guard = lock.lock().await;
        let Some(mut run) = state.db.get(&run.id)? else {
            continue;
        };
        let Some(active) = run.active_implementation().cloned() else {
            continue;
        };
        let observed = match implementation::observe(host, &active.machine, &active.id).await {
            Ok(Some(observed)) => observed,
            Ok(None) => continue,
            Err(_) => {
                unrecovered.insert(run.id.clone());
                continue;
            }
        };
        if observed["state"] == "interrupted" {
            set_observed(&mut run, "interrupted");
            let now = chrono::Utc::now().timestamp();
            run.stop(
                Phase::Interrupted,
                now,
                format!(
                    "The host restarted while implementation run {} was active, so it was interrupted; resume to continue from this stage (a started coding run is not started again).",
                    active.id
                ),
            );
            state.db.save(&run)?;
        }
    }
    Ok(unrecovered)
}

/// Record the observed state of the run's active implementation run.
fn set_observed(run: &mut Run, observed: &str) {
    let stage = run.stopped_in.unwrap_or(run.state);
    let target = match stage {
        Phase::Planning => run.planning.as_mut(),
        Phase::Coding => run
            .children
            .iter_mut()
            .find(|c| c.outcome.is_none())
            .and_then(|c| c.coding.as_mut()),
        _ => None,
    };
    if let Some(target) = target {
        target.state = Some(observed.to_owned());
    }
}

/// Take a slot for a run in a stage that needs one, when one is free.
async fn admit(host: &Arc<Host>, run: &mut Run) -> Result<bool> {
    let state = &host.state.workflows;
    let _admission = state.admission.lock().await;
    let concurrency = host.state.settings.map()?.workflow_settings.concurrency as usize;
    let slotted: Vec<Phase> = Phase::ALL.into_iter().filter(|s| s.takes_slot()).collect();
    let taken = state
        .db
        .list(&slotted, None, -1)?
        .iter()
        .filter(|r| r.admitted && r.id != run.id)
        .count();
    if taken >= concurrency {
        return Ok(false);
    }
    run.admitted = true;
    state.db.save(run)?;
    Ok(true)
}

/// Advance one run as far as it goes now.
async fn advance(host: &Arc<Host>, azure: &Azure, id: &str, now: i64, poll: i64) -> Result<()> {
    let state = &host.state.workflows;
    let lock = state.lock(id);
    for _ in 0..STEPS {
        let guard = lock.lock().await;
        let Some(mut run) = state.db.get(id)? else {
            return Ok(());
        };
        if !run.state.driven() {
            return Ok(());
        }
        if run.retry_at.is_some_and(|at| now < at) {
            return Ok(());
        }
        if run.state.takes_slot() && !run.admitted && !admit(host, &mut run).await? {
            return Ok(());
        }
        let _advancing = Advancing::new(&state.advancing);
        if run.state == Phase::Classifying {
            // The director's session can take a minute or two: the run's lock is not held
            // meanwhile, so a cancel is not kept waiting; its verdict applies only to a run
            // still classifying.
            drop(guard);
            if !classify(host, azure, run, now).await? {
                return Ok(());
            }
            continue;
        }
        // The acting blocks due at this stage run before the stage's own step; the run's lock
        // is not held while they act.
        if acting::has_due(&run) {
            drop(guard);
            match acting::execute(host, id, now).await? {
                acting::Progress::Waiting => return Ok(()),
                acting::Progress::Idle | acting::Progress::Changed => continue,
            }
        }
        match step(host, azure, &mut run, now, poll).await {
            Ok(true) => {
                run.updated_at = now;
                state.db.save(&run)?;
            }
            Ok(false) => return Ok(()),
            Err(error) => {
                let redactor =
                    security::Redactor::new(&[], Vec::new()).expect("built-in redaction patterns");
                run.stop(Phase::Failed, now, redactor.redact(&error));
                state.db.save(&run)?;
                // The blocks of the `failed` stage.
                drop(guard);
                acting::execute(host, id, now).await?;
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Note why a run waits (a machine offline, a read that failed); a change only when it is new.
fn wait(run: &mut Run, why: String) -> Result<bool, String> {
    if run.error.as_deref() == Some(why.as_str()) {
        return Ok(false);
    }
    run.error = Some(why);
    Ok(true)
}

/// A launch refusal the run waits through (the machine is offline or busy with these
/// directories) rather than one that fails it.
fn transient(error: &str) -> bool {
    error.contains("offline")
        || error.contains("Another run is using these directories")
        || error.contains("could not be reached")
        || error.contains("did not answer")
}

/// Ask the director and apply its verdict, when the run is still classifying; returns whether
/// the run changed.
async fn classify(host: &Arc<Host>, azure: &Azure, run: Run, now: i64) -> Result<bool> {
    let state = &host.state.workflows;
    let map = host.state.settings.map()?;
    let verdict: std::result::Result<Value, String> = match &map.workflow_settings.director {
        None => Err("No workflow director is configured; assign a workflow to this run.".into()),
        Some(director) => {
            match azure
                .item(
                    &host.state,
                    run.item.source.as_deref(),
                    Some(&run.item.project),
                    run.item.id,
                )
                .await
            {
                Ok(detail) => {
                    Box::pin(super::director::classify_item(
                        host,
                        director,
                        &detail,
                        run.auto_start,
                    ))
                    .await
                }
                Err(error) => Err(error),
            }
        }
    };
    let lock = state.lock(&run.id);
    let _guard = lock.lock().await;
    let Some(mut run) = state.db.get(&run.id)? else {
        return Ok(false);
    };
    if run.state != Phase::Classifying {
        return Ok(false);
    }
    let verdict = match verdict {
        // The director's machine is offline or the item could not be read: try again later.
        Err(error) if error.starts_with("[not_ready]") || error.starts_with("[network]") => {
            let note = error.split_once("] ").map_or(error.as_str(), |(_, t)| t);
            run.error = Some(note.to_owned());
            // Asked again later, one minute doubling up to half an hour, without a slot
            // meanwhile, so an unreachable director never blocks other runs.
            run.retries = run.retries.saturating_add(1);
            run.retry_at = Some(now + (60i64 << run.retries.min(5).saturating_sub(1)).min(1800));
            run.admitted = false;
            run.updated_at = now;
            state.db.save(&run)?;
            return Ok(false);
        }
        Err(error) => {
            let note = error.split_once("] ").map_or(error.as_str(), |(_, t)| t);
            run.classification = Some(json!({"workflow":null,"outcome":"none","reason":note}));
            run.advance(Phase::Unassigned, now, "no workflow");
            run.attention = Some(note.to_owned());
            state.db.save(&run)?;
            return Ok(true);
        }
        Ok(verdict) => verdict,
    };
    run.classification = Some(json!({
        "workflow": verdict["workflow"],
        "outcome": verdict["outcome"],
        "reason": verdict["reason"],
        "director": verdict["director"],
        "session": verdict["run"],
    }));
    // Checked against the workflows as they are now: one removed or disabled during the
    // session is not assigned.
    let map = host.state.settings.map()?;
    let chosen = verdict["workflow"].as_str().and_then(|id| {
        let workflow = map.workflows.iter().find(|w| w.id == id && w.enabled)?;
        // A run the watcher started chooses only among the workflows that start by themselves.
        (same_project(workflow, &run.item) && (!run.auto_start || workflow.auto_start))
            .then_some(workflow)
    });
    match chosen {
        Some(workflow) => {
            run.error = None;
            assign(
                &mut run,
                workflow,
                &map.workflow_blocks,
                now,
                &format!("chosen: {}", workflow.id),
            )
        }
        None => {
            let reason = match verdict["workflow"].as_str() {
                Some(id) => {
                    format!("workflow {id} is no longer an enabled workflow of this project")
                }
                None => verdict["reason"].as_str().unwrap_or("none").to_owned(),
            };
            run.error = None;
            run.advance(Phase::Unassigned, now, "no workflow");
            run.attention = Some(format!(
                "The workflow director chose no workflow ({reason}); assign one to this run."
            ));
        }
    }
    state.db.save(&run)?;
    Ok(true)
}

/// Whether a workflow covers the item's organization and project.
pub(in crate::app) fn same_project(workflow: &crate::workflows::Workflow, item: &Item) -> bool {
    crate::workflows::organization_name(&workflow.organization)
        == crate::workflows::organization_name(&item.organization)
        && workflow.project.eq_ignore_ascii_case(&item.project)
}

/// One step of a run in a driven stage other than classifying; returns whether it changed.
async fn step(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    now: i64,
    poll: i64,
) -> std::result::Result<bool, String> {
    let workflow = run
        .definition
        .clone()
        .ok_or("The run has no workflow definition.")?;
    match run.state {
        Phase::Planning => planning(host, azure, run, &workflow, now).await,
        Phase::PlanReview => {
            let plan = run
                .planning
                .as_ref()
                .map(|p| p.id.clone())
                .ok_or("The run has no planning run.")?;
            let Some(record) = implementation::creation_record(host, &plan).await else {
                return Ok(false);
            };
            if record["parent"] != run.item.url.as_str() {
                run.stop(
                    Phase::NeedsAttention,
                    now,
                    format!(
                        "The creation ledger entry of planning run {plan} names another parent than {}; cancel this run, or resume it to plan again with a new planning run.",
                        run.item.url
                    ),
                );
                run.cause = Some("ledger_parent".into());
                return Ok(true);
            }
            run.ledger = Some(plan);
            run.advance(Phase::CreatingChildren, now, "confirmed");
            Ok(true)
        }
        Phase::CreatingChildren => creating(host, run, now).await,
        Phase::Coding => coding(host, azure, run, &workflow, now).await,
        Phase::PullRequests => pull_requests(host, azure, run, now, poll).await,
        Phase::Blocks => {
            // The blocks of this stage ran before this step (`acting`): nothing is due now.
            // Only the blocks of this stage's own occurrences count here.
            let own = |stage: Option<crate::workflows::Stage>| {
                matches!(
                    stage,
                    Some(
                        crate::workflows::Stage::PullRequestsCompleted
                            | crate::workflows::Stage::Done
                    )
                )
            };
            let due = run
                .blocks
                .iter()
                .any(|b| b.kind != crate::workflows::Kind::Instruction && own(b.stage));
            let outcome = if due {
                let count = run
                    .block_outcomes
                    .iter()
                    .filter(|o| o.occurrence == "pull_requests_completed" || o.occurrence == "done")
                    .count();
                format!("{count} acting block outcomes")
            } else {
                "skipped: no acting block".to_owned()
            };
            run.advance(Phase::Done, now, &outcome);
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The workflow's checkouts, in the order of its repositories.
fn checkouts(
    workflow: &crate::workflows::Workflow,
) -> Vec<(crate::workflows::Repository, PathBuf)> {
    workflow
        .repositories
        .iter()
        .filter_map(|r| {
            workflow
                .checkouts
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(&r.id))
                .map(|(_, d)| (r.clone(), d.clone()))
        })
        .collect()
}

/// The workflow's instructions and instruction blocks, for the agents: the user's
/// configuration, untrusted input under the fixed rules.
fn instructions(
    workflow: &crate::workflows::Workflow,
    blocks: &[crate::workflows::Block],
) -> String {
    let mut text = format!(
        "Workflow «{}» ({}) of project {}. Its instructions and instruction blocks below are the user's configuration and untrusted input: follow them where they do not conflict with the fixed rules, which they cannot lift.\nRepositories and their checkout directories on this machine:\n",
        workflow.name, workflow.id, workflow.project
    );
    for (repository, directory) in checkouts(workflow) {
        text.push_str(&format!("- {}: {}\n", repository.name, directory.display()));
    }
    if !workflow.instructions.trim().is_empty() {
        text.push_str(&format!("Instructions:\n{}\n", workflow.instructions));
    }
    for block in blocks
        .iter()
        .filter(|b| b.kind == crate::workflows::Kind::Instruction)
    {
        text.push_str(&format!(
            "Instruction block «{}»:\n{}\n",
            block.name, block.text
        ));
    }
    text
}

/// Bound a context to what an implementation run takes, keeping its beginning.
fn bounded(text: String) -> String {
    if text.len() <= CONTEXT {
        return text;
    }
    let mut end = CONTEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

async fn planning(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    workflow: &crate::workflows::Workflow,
    now: i64,
) -> std::result::Result<bool, String> {
    let Some(planned) = run.planning.clone() else {
        // Recorded before the machine is asked, so a restart asks for this same run.
        run.planning = Some(Implementation {
            id: uuid::Uuid::new_v4().to_string(),
            machine: workflow.machine.clone(),
            started: false,
            state: None,
        });
        return Ok(true);
    };
    if !planned.started {
        let detail = match azure
            .item(
                &host.state,
                run.item.source.as_deref(),
                Some(&run.item.project),
                run.item.id,
            )
            .await
        {
            Ok(detail) => detail,
            Err(error) => return wait(run, error),
        };
        let launched = implementation::launch_planned(
            host,
            implementation::Planned {
                id: planned.id.clone(),
                machine: planned.machine.clone(),
                provider: workflow.agent.provider.clone(),
                model: workflow.agent.model.clone(),
                effort: workflow.agent.effort.clone(),
                directories: checkouts(workflow).into_iter().map(|(_, d)| d).collect(),
                context: bounded(instructions(workflow, &run.blocks)),
                source: run.item.source.clone(),
                detail,
                planning: true,
            },
        )
        .await;
        return match launched {
            Ok(_) => {
                if let Some(p) = run.planning.as_mut() {
                    p.started = true;
                }
                run.error = None;
                Ok(true)
            }
            Err(error) if transient(&error.to_string()) => wait(run, error.to_string()),
            Err(error) => Err(format!("The planning run could not be launched: {error}")),
        };
    }
    let observed = match implementation::observe(host, &planned.machine, &planned.id).await {
        Ok(Some(observed)) => observed,
        Ok(None) => {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "Planning run {} is not on its machine; resume to plan again.",
                    planned.id
                ),
            );
            return Ok(true);
        }
        Err(error) => return wait(run, error.to_string()),
    };
    let observed_state = observed["state"].as_str().unwrap_or_default().to_owned();
    let changed = planned.state.as_deref() != Some(observed_state.as_str()) || run.error.is_some();
    run.error = None;
    set_observed(run, &observed_state);
    match observed_state.as_str() {
        "planning" => Ok(changed),
        "plan_ready" => {
            let children = observed["plan"]["children"].as_array().map_or(0, Vec::len);
            run.advance(
                Phase::PlanReview,
                now,
                &format!("plan_ready: {children} children proposed"),
            );
            run.attention = Some(format!(
                "Review the plan of planning run {} and confirm the children to create (implementation_create).",
                planned.id
            ));
            Ok(true)
        }
        "interrupted" => {
            run.stop(
                Phase::Interrupted,
                now,
                format!(
                    "Planning run {} was interrupted; resume to plan again.",
                    planned.id
                ),
            );
            Ok(true)
        }
        other => {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "Planning run {} ended {other}; read its record (implementation_runs), then resume to plan again.",
                    planned.id
                ),
            );
            Ok(true)
        }
    }
}

/// The children the ledger created, in order.
fn created_children(record: &Value) -> Vec<Child> {
    record["created"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let id = c["id"].as_u64().filter(|id| *id > 0)?;
            let url = c["url"].as_str().and_then(crate::tools::todo::https_link)?;
            Some(Child {
                id,
                title: c["title"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(500)
                    .collect(),
                url,
                kind: c["kind"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(120)
                    .collect(),
                ..Default::default()
            })
        })
        .collect()
}

async fn creating(host: &Arc<Host>, run: &mut Run, now: i64) -> std::result::Result<bool, String> {
    let ledger = run
        .ledger
        .clone()
        .ok_or("The run has no creation ledger entry.")?;
    let record = implementation::creation_record(host, &ledger)
        .await
        .ok_or("The creation ledger entry is missing.")?;
    let state = record["state"].as_str().unwrap_or_default().to_owned();
    match state.as_str() {
        "sending" => Ok(false),
        "created" => {
            run.children = created_children(&record);
            let count = run.children.len();
            run.advance(Phase::Coding, now, &format!("created: {count} children"));
            Ok(true)
        }
        _ if run.creation_accepted => {
            run.children = created_children(&record);
            let count = run.children.len();
            run.advance(
                Phase::Coding,
                now,
                &format!("{state}, accepted: {count} children created"),
            );
            Ok(true)
        }
        _ => {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "The creation of the children ended {state}; review what was created (implementation_creation {ledger}), then resume to code the children that were created."
                ),
            );
            Ok(true)
        }
    }
}

/// The plain text of the item's description.
fn description(detail: &Detail) -> String {
    detail
        .fields
        .iter()
        .find(|f| f.reference == "System.Description")
        .and_then(|f| f.value.as_str())
        .map(crate::adapters::teams::plain_text)
        .unwrap_or_default()
}

/// The workflow repository a child names (the one named first in its title and description,
/// as a whole word), or the workflow's first repository.
pub(in crate::app) fn named_repository(
    workflow: &crate::workflows::Workflow,
    title: &str,
    description: &str,
) -> Option<(crate::workflows::Repository, PathBuf)> {
    let text = format!("{title}\n{description}").to_lowercase();
    // A name followed by a full stop is still a mention.
    let word = |c: char| c.is_alphanumeric() || "_-".contains(c);
    let position = |name: &str| {
        let name = name.to_lowercase();
        text.match_indices(&name)
            .find(|(at, _)| {
                let before = text[..*at].chars().next_back();
                let after = text[at + name.len()..].chars().next();
                !before.is_some_and(word) && !after.is_some_and(word)
            })
            .map(|(at, _)| at)
    };
    let all = checkouts(workflow);
    all.iter()
        .filter_map(|(r, d)| position(&r.name).map(|at| (at, r, d)))
        .min_by_key(|(at, _, _)| *at)
        .map(|(_, r, d)| (r.clone(), d.clone()))
        .or_else(|| all.into_iter().next())
}

/// The branch a coding session reported: the last line of its report that starts with
/// `Branch:`, as a plain branch name.
pub(in crate::app) fn reported_branch(output: &str) -> Option<String> {
    output.lines().rev().find_map(|line| {
        // A list marker or emphasis may open the line.
        let line = line.trim_start_matches(|c: char| c.is_whitespace() || "-*>`".contains(c));
        let rest = line
            .get(..BRANCH.len())
            .filter(|p| p.eq_ignore_ascii_case(BRANCH))
            .map(|_| &line[BRANCH.len()..])?;
        let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || "*`\"'".contains(c));
        let token = rest.split_whitespace().next()?;
        // Only a remark may follow the name («(pushed)», «— with tests»), never more words.
        let after = rest[token.len()..].trim_start();
        if !(after.is_empty() || after.starts_with(['(', '-', '\u{2013}', '\u{2014}'])) {
            return None;
        }
        // Sentence punctuation and quoting after the name; a trailing hyphen is the name's.
        let branch = token.trim_end_matches(|c: char| ".,;:)`\"'*".contains(c));
        let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        crate::ado::workflow::valid_branch(branch).then(|| branch.to_owned())
    })
}

async fn coding(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    workflow: &crate::workflows::Workflow,
    now: i64,
) -> std::result::Result<bool, String> {
    let Some(index) = run.children.iter().position(|c| c.outcome.is_none()) else {
        let done = run.children.iter().filter(|c| !c.accepted()).count();
        run.advance(
            Phase::PullRequests,
            now,
            &format!("coded: {done} of {}", run.children.len()),
        );
        run.waiting_since = Some(now);
        run.checked_at = None;
        return Ok(true);
    };
    let parent = (!run.task()).then(|| run.item.url.clone());
    let child = run.children[index].clone();
    let Some(coding) = child.coding.clone() else {
        // Recorded before the machine is asked: never a second run for this child.
        run.children[index].coding = Some(Implementation {
            id: uuid::Uuid::new_v4().to_string(),
            machine: workflow.machine.clone(),
            started: false,
            state: None,
        });
        return Ok(true);
    };
    if !coding.started {
        let detail = match azure
            .item(
                &host.state,
                run.item.source.as_deref(),
                Some(&run.item.project),
                child.id,
            )
            .await
        {
            Ok(detail) => detail,
            Err(error) => return wait(run, error),
        };
        let (repository, directory) =
            named_repository(workflow, &detail.item.title, &description(&detail))
                .ok_or("The workflow has no checkout.")?;
        let mut context = format!(
            "You are implementing {} {} «{}» ({}){}, in the checkout of repository {} at {}. Open its pull request as the instructions ask. When you finish, end your report with one line `{BRANCH} NAME` naming the branch you pushed (without refs/heads/).\n",
            child.kind,
            child.id,
            child.title,
            child.url,
            parent
                .as_ref()
                .map(|p| format!(", a child of {p}"))
                .unwrap_or_default(),
            repository.name,
            directory.display()
        );
        context.push_str(&instructions(workflow, &run.blocks));
        let launched = implementation::launch_planned(
            host,
            implementation::Planned {
                id: coding.id.clone(),
                machine: coding.machine.clone(),
                provider: workflow.agent.provider.clone(),
                model: workflow.agent.model.clone(),
                effort: workflow.agent.effort.clone(),
                directories: vec![directory.clone()],
                context: bounded(context),
                source: run.item.source.clone(),
                detail,
                planning: false,
            },
        )
        .await;
        return match launched {
            Ok(_) => {
                let child = &mut run.children[index];
                if let Some(c) = child.coding.as_mut() {
                    c.started = true;
                }
                child.repository = Some(repository);
                child.directory = Some(directory);
                child.started_at = Some(now);
                run.error = None;
                Ok(true)
            }
            Err(error) if transient(&error.to_string()) => wait(run, error.to_string()),
            Err(error) => Err(format!(
                "The coding run of {} could not be launched: {error}",
                child.url
            )),
        };
    }
    let observed = match implementation::observe(host, &coding.machine, &coding.id).await {
        Ok(Some(observed)) => observed,
        Ok(None) => {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "Coding run {} of {} is not on its machine.",
                    coding.id, child.url
                ),
            );
            return Ok(true);
        }
        Err(error) => return wait(run, error.to_string()),
    };
    let observed_state = observed["state"].as_str().unwrap_or_default().to_owned();
    let changed = coding.state.as_deref() != Some(observed_state.as_str()) || run.error.is_some();
    run.error = None;
    set_observed(run, &observed_state);
    match observed_state.as_str() {
        "running" => Ok(changed),
        "done" => {
            let branch = reported_branch(observed["output"].as_str().unwrap_or_default());
            {
                let child = &mut run.children[index];
                child.branch = branch;
                child.outcome = Some("done".into());
                child.finished_at = Some(now);
            }
            // The pull request the session opened, read once now; the next stage reads again.
            let _ = detect(host, azure, run, index).await;
            Ok(true)
        }
        other => {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "Coding run {} of {} ended {other}; read its record (implementation_runs), finish or fix the child, then resume to continue with the next child (this run is not started again).",
                    coding.id, child.url
                ),
            );
            Ok(true)
        }
    }
}

/// Read the pull requests of a child's reported branch and record them, keeping what the user
/// accepted; returns whether they changed.
async fn detect(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    index: usize,
) -> std::result::Result<bool, String> {
    let child = &run.children[index];
    let (Some(repository), Some(branch)) = (child.repository.clone(), child.branch.clone()) else {
        return Ok(false);
    };
    let mut found = azure
        .pull_requests(&host.state, run, &repository, &branch)
        .await?;
    let child = &mut run.children[index];
    for pull in &mut found {
        pull.accepted = child
            .pull_requests
            .iter()
            .any(|p| p.id == pull.id && p.accepted);
    }
    if child.pull_requests == found {
        return Ok(false);
    }
    child.pull_requests = found;
    Ok(true)
}

async fn pull_requests(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    now: i64,
    poll: i64,
) -> std::result::Result<bool, String> {
    let mut changed = false;
    if let Some(child) = run
        .children
        .iter()
        .find(|c| c.outcome.as_deref() == Some("done") && c.branch.is_none())
    {
        run.stop(
            Phase::NeedsAttention,
            now,
            format!(
                "The coding session of {} reported no branch, so its pull request cannot be found; open it by hand if it is missing, then resume.",
                child.url
            ),
        );
        return Ok(true);
    }
    if run.checked_at.is_none_or(|at| now - at >= poll) {
        for index in 0..run.children.len() {
            if run.children[index].outcome.as_deref() != Some("done") {
                continue;
            }
            if let Err(error) = detect(host, azure, run, index).await {
                // A failed read waits for the poll interval too.
                run.checked_at = Some(now);
                run.error = Some(error);
                return Ok(true);
            }
        }
        // Recorded on every read, so the next read waits for the poll interval.
        run.checked_at = Some(now);
        run.error = None;
        changed = true;
    }
    let coded: Vec<&Child> = run
        .children
        .iter()
        .filter(|c| c.outcome.as_deref() == Some("done"))
        .collect();
    if coded.is_empty() && !run.undelivered_accepted {
        let reason = format!(
            "No child was delivered ({} children, none coded to done), so there is no pull request to wait for; cancel the run, or resume it to finish it as done without delivering anything.",
            run.children.len()
        );
        run.stop(Phase::NeedsAttention, now, reason);
        run.cause = Some("nothing_delivered".into());
        return Ok(true);
    }
    if let Some(pull) = coded
        .iter()
        .flat_map(|c| c.pull_requests.iter())
        .find(|p| p.status == "abandoned" && !p.accepted)
    {
        let reason = format!(
            "Pull request {} was abandoned; resume to continue without it.",
            pull.url
        );
        run.stop(Phase::NeedsAttention, now, reason);
        return Ok(true);
    }
    // Every coded child has its pull requests, each completed (or abandoned and accepted).
    let settled = coded.iter().all(|c| {
        !c.pull_requests.is_empty()
            && c.pull_requests
                .iter()
                .all(|p| p.status == "completed" || p.accepted)
    });
    if settled {
        let count = coded.iter().map(|c| c.pull_requests.len()).sum::<usize>();
        let outcome = if coded.is_empty() {
            "accepted: nothing delivered".to_owned()
        } else {
            format!("completed: {count} pull requests")
        };
        run.advance(Phase::Blocks, now, &outcome);
        return Ok(true);
    }
    let days = host
        .state
        .settings
        .map()
        .map_err(fail)?
        .workflow_settings
        .pull_request_wait_days;
    let since = run.waiting_since.unwrap_or(now);
    if now - since >= i64::from(days) * 86_400 {
        let missing: Vec<&str> = coded
            .iter()
            .filter(|c| c.pull_requests.is_empty())
            .map(|c| c.url.as_str())
            .collect();
        let open: Vec<&str> = coded
            .iter()
            .flat_map(|c| c.pull_requests.iter())
            .filter(|p| p.status != "completed" && !p.accepted)
            .map(|p| p.url.as_str())
            .collect();
        run.stop(
            Phase::NeedsAttention,
            now,
            format!(
                "The pull requests were not completed within {days} days (open: {}; children without one: {}); resume to wait again.",
                if open.is_empty() { "none".into() } else { open.join(", ") },
                if missing.is_empty() { "none".into() } else { missing.join(", ") }
            ),
        );
        return Ok(true);
    }
    Ok(changed)
}

/// Where a stopped run continues: the stage it stopped in, with what the person resumed past
/// accepted. A plan is never submitted again and a coding run recorded as started is never
/// started again; a planning run that did not end ready is replaced by a new one (planning is
/// read-only).
pub(in crate::app) async fn resume(host: &Arc<Host>, run: &mut Run, now: i64) -> Result<Value> {
    let mut stage = run
        .stopped_in
        .context("This run did not stop in a stage it can resume.")?;
    let mut accepted = Vec::new();
    match (stage, run.cause.as_deref()) {
        // The ledger entry names another parent: plan again, with a new planning run.
        (_, Some("ledger_parent")) => {
            if let Some(planned) = run.planning.take() {
                accepted.push(json!({"planning_run": planned.id, "replaced": true}));
            }
            run.ledger = None;
            stage = Phase::Planning;
        }
        // Nothing was delivered: the person accepts the run as done without pull requests.
        (_, Some("nothing_delivered")) => {
            run.undelivered_accepted = true;
            accepted.push(json!({"nothing_delivered": true}));
        }
        (Phase::Planning, _) => {
            if let Some(planned) = run.planning.clone()
                && planned.started
            {
                // A planning run the machine no longer has, or one that did not end ready, is
                // replaced (planning is read-only); one the machine cannot be asked about now
                // is looked at again by the stage.
                let replace =
                    match implementation::observe(host, &planned.machine, &planned.id).await {
                        Ok(Some(observed)) => {
                            !matches!(observed["state"].as_str(), Some("planning" | "plan_ready"))
                        }
                        Ok(None) => true,
                        Err(_) => !matches!(
                            planned.state.as_deref(),
                            None | Some("planning" | "plan_ready")
                        ),
                    };
                if replace {
                    accepted.push(json!({"planning_run": planned.id, "replaced": true}));
                    run.planning = None;
                }
            }
        }
        (Phase::CreatingChildren, _) => {
            run.creation_accepted = true;
            accepted.push(json!({"creation": run.ledger}));
        }
        (Phase::Coding, _) => {
            if let Some(index) = run.children.iter().position(|c| c.outcome.is_none())
                && let Some(coding) = run.children[index].coding.clone()
                && coding.started
            {
                // A coding run is never started again: one that ended (or that its machine no
                // longer has) is accepted as it ended, and the next child is coded.
                let (ended, output) =
                    match implementation::observe(host, &coding.machine, &coding.id).await {
                        Ok(Some(observed)) => (
                            observed["state"].as_str().map(str::to_owned),
                            observed["output"].as_str().map(str::to_owned),
                        ),
                        Ok(None) => (Some("missing".to_owned()), None),
                        Err(_) => (coding.state.clone(), None),
                    };
                if ended.as_deref() != Some("running") {
                    let child = &mut run.children[index];
                    child.outcome = Some("accepted".into());
                    child.finished_at = Some(now);
                    child.branch = output.as_deref().and_then(reported_branch);
                    if let (Some(c), Some(ended)) = (child.coding.as_mut(), ended.clone()) {
                        c.state = Some(ended);
                    }
                    accepted
                        .push(json!({"child": child.url, "coding_run": coding.id, "ended": ended}));
                }
            }
        }
        (Phase::PullRequests, _) => {
            for child in &mut run.children {
                if child.outcome.as_deref() == Some("done") && child.branch.is_none() {
                    child.outcome = Some("accepted".into());
                    accepted.push(json!({"child": child.url, "without": "pull_request"}));
                }
                for pull in &mut child.pull_requests {
                    if pull.status == "abandoned" && !pull.accepted {
                        pull.accepted = true;
                        accepted.push(json!({"pull_request": pull.url}));
                    }
                }
            }
            run.waiting_since = Some(now);
            run.checked_at = None;
        }
        _ => {}
    }
    run.advance(stage, now, "resumed");
    run.error = None;
    Ok(json!(accepted))
}

/// The tracked task of the control loop: one tick every 30 seconds.
pub(in crate::app) async fn schedule(host: Arc<Host>) {
    loop {
        let now = chrono::Utc::now().timestamp();
        if tick(&host, &Azure::Live, now, PULL_REQUEST_POLL_SECONDS)
            .await
            .is_err()
        {
            tracing::warn!(event = "workflow_engine_not_ready");
        }
        tokio::time::sleep(std::time::Duration::from_secs(TICK_SECONDS)).await;
    }
}
