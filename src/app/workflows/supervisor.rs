//! The workflow supervisor: a model-backed agent on the coordinator that looks after the
//! account's workflow runs the way a person following them would. Code reads everything (the
//! run, its implementation records and causes, its sessions' bounded summaries, the work item
//! with its comments and history, the pull requests and branches, the machines); the model
//! only judges what code gave it, in the director's tool-less, read-only, closed-JSON session
//! on the supervisor's machine (`implementation::director::ask` with `DirectTask::Supervise`).
//!
//! A look happens on an event the engine's runs show (`events`: a run stopped, waited too
//! long, lost its machine, or disagrees with its sessions) or on request (`workflow_supervise`),
//! once per run per event, at least `min_interval_minutes` apart for one run, one at a time on
//! the account and at most `looks_per_day` automatic ones in 24 hours. Events are kept in
//! `workflow_looks` in `workflows.db`, so a key is looked at once even across restarts; a look
//! that fails is never retried, and a look's result is never an event.
//!
//! What the model returns never reaches a record as it was returned: every text is cleaned and
//! cut like a progress summary, claims keep only references code can parse, and actions outside
//! the closed list or about another run are dropped. Code checks each claim of a final report
//! against what it read (`checks`). By itself the supervisor stores the look on the run and may
//! move that run ahead in the line for a slot; every other action is a proposal the person
//! confirms through the operation that already does it (`workflow_supervision_act`). Nothing
//! here writes to Azure DevOps, merges or changes code.
use super::engine::Azure;
use super::record::{self, Pause, Run, State as Phase};
use super::runs::{found, require_engine};
use super::*;
use crate::llm::implementation::{Cause, DirectTask, Failure, SUPERVISOR_ACTIONS};
use crate::workflows::director::{Director, Supervisor};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

pub(in crate::app) const METHODS: [&str; 5] = [
    "workflow_supervise",
    "workflow_supervision",
    "workflow_supervision_act",
    "workflow_supervision_dismiss",
    "workflow_run_requeue",
];

/// A request for a look of a run waits this long after its previous look started.
pub(in crate::app) const REQUEST_SECONDS: i64 = 300;
/// At most this many looks are kept on a run; the oldest is dropped.
pub(in crate::app) const MAX_LOOKS: usize = 20;
/// How long the reads before a look's session may take, all of them side by side.
const READ_SECONDS: u64 = 60;
/// The most characters of a look's input; past it code cuts in a fixed order.
pub(in crate::app) const MAX_INPUT: usize = 60_000;
const DAY: i64 = 86_400;
/// The most runs listed when the events are derived (the most recent).
const RUNS_LISTED: i64 = 200;
/// What one look reads at most.
const MAX_IMPLEMENTATIONS: usize = 10;
const MAX_COMMENTS: usize = 30;
const COMMENT_CHARS: usize = 1500;
const MAX_REVISIONS: usize = 50;
const REVISION_CHARS: usize = 300;
const DESCRIPTION_CHARS: usize = 4000;
const REPORT_CHARS: usize = 6000;
/// The answer's bounds.
const MAX_CLAIMS: usize = 20;
const MAX_UNVERIFIED: usize = 10;
const MAX_DRAFTS: usize = 3;
const MAX_ACTIONS: usize = 5;
const PART_CHARS: usize = 2000;
const LINE_CHARS: usize = 300;
const TITLE_CHARS: usize = 200;

/// What the supervisor said about a run.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Assessment {
    pub done: String,
    pub left: String,
    pub recommendation: String,
}

/// One claim of a final report, as code checked it against what it read.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Check {
    /// The claim, cleaned and cut.
    pub claim: String,
    /// `pull_request`, `branch`, `item_state` or `other`.
    pub kind: String,
    /// What code parsed from it: a pull request number, a branch name or a state; empty when
    /// nothing could be parsed.
    #[serde(default, rename = "ref", skip_serializing_if = "String::is_empty")]
    pub reference: String,
    /// `verified` or `not_verified`.
    pub result: String,
    /// Code's sentence about what it read.
    pub fact: String,
    /// `report` when code found the claim in a final report by itself, `supervisor` when the
    /// supervisor listed it.
    pub from: String,
}

/// A work item the supervisor drafted, for the person to create with Create request.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Draft {
    /// `requirement` or `bug`.
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub description: String,
    /// The workflow repository it is for, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<crate::workflows::Repository>,
    pub reason: String,
}

/// One proposed action and what became of it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Action {
    /// Its place in the look (`a1`…).
    pub id: String,
    /// One of `SUPERVISOR_ACTIONS`.
    pub action: String,
    /// `stop` or `finish`, for `pause_project`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sessions: String,
    /// The index of its drafted work item, for `create_work_item`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item: Option<usize>,
    pub reason: String,
    /// `proposed`; `done` for what the supervisor does by itself (`move_ahead`); `attempting`
    /// while the person's confirmed action runs, then `executed`, `refused` or `uncertain`;
    /// `opened` for a work item handed to Create request; `expired` when the run moved on
    /// since the look; `dismissed`.
    pub state: String,
    /// Who acted: `supervisor` (by itself) or `person`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    /// Why it was refused or is uncertain (a fixed text of this package).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// One look of the supervisor at a run, as stored on it (`Run::supervision`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Look {
    /// Its session's record in Agent history.
    pub id: String,
    /// The event that started it: `run_stopped`, `run_waiting`, `machine_offline`,
    /// `session_disagrees`, or `requested`.
    pub event: String,
    /// The event keys it covered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    /// Events of this run skipped (over the daily cap) since its previous look.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missed: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requested: bool,
    pub created_at: i64,
    pub supervised_by: Director,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// `done` or `failed`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    /// What code could not read before the session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<String>,
    /// What code cut to keep the input within its bound.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncated: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<Assessment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
    /// The drafted handover note for a restart.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handover: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work_items: Vec<Draft>,
    /// The drafted progress summary, with the links code builds.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// Actions and drafts code dropped (another run, outside the list, a repository that is
    /// not the workflow's).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u32,
    /// The run as it was when looked at: an action whose run moved on is expired.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub marker: String,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn time(at: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(at, 0).map(|t| t.to_rfc3339())
}
fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
/// The last `max` characters of a text (a report ends with what it concluded).
fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(max)).collect()
}

/// Where a run is, as an action confirmed later must still find it.
pub(in crate::app) fn marker(run: &Run, pauses: &[Pause]) -> String {
    format!(
        "{}:{}:{}:{}",
        run.state.word(),
        run.timeline.len(),
        run.timeline.last().map_or(0, |s| s.started_at),
        record::paused(pauses, &run.item.project)
    )
}

/// One recorded event of the supervisor, or a request for a look.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct Row {
    pub key: String,
    pub run: String,
    pub event: String,
    /// `pending`, `running`, `done`, `failed` or `skipped`.
    pub state: String,
    pub requested: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// The look that covered it, or why it was skipped.
    pub payload: Value,
}

/// The supervisor's events and looks in `workflows.db` (its own connection beside the runs').
pub(in crate::app) struct Looks(std::sync::Mutex<Connection>);
impl Looks {
    pub(in crate::app) fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        security::protect_file(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_looks (
                key TEXT PRIMARY KEY, run TEXT NOT NULL, event TEXT NOT NULL,
                state TEXT NOT NULL, requested INTEGER NOT NULL,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, payload TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS workflow_looks_run ON workflow_looks(run, updated_at);
            CREATE INDEX IF NOT EXISTS workflow_looks_state ON workflow_looks(state, created_at);",
        )?;
        Ok(Self(std::sync::Mutex::new(db)))
    }
    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
        Ok(Row {
            key: r.get(0)?,
            run: r.get(1)?,
            event: r.get(2)?,
            state: r.get(3)?,
            requested: r.get::<_, i64>(4)? != 0,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
            payload: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
        })
    }
    const COLUMNS: &'static str =
        "key, run, event, state, requested, created_at, updated_at, payload";
    /// Record an event once: a key already recorded is never recorded again.
    pub(in crate::app) fn insert(&self, row: &Row) -> Result<bool> {
        let db = self.0.lock().unwrap();
        Ok(db.execute(
            "INSERT OR IGNORE INTO workflow_looks (key, run, event, state, requested, created_at, updated_at, payload) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                row.key,
                row.run,
                row.event,
                row.state,
                i64::from(row.requested),
                row.created_at,
                row.updated_at,
                row.payload.to_string()
            ],
        )? == 1)
    }
    pub(in crate::app) fn set(
        &self,
        key: &str,
        state: &str,
        payload: &Value,
        at: i64,
    ) -> Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE workflow_looks SET state=?2, payload=?3, updated_at=?4 WHERE key=?1",
            params![key, state, payload.to_string(), at],
        )?;
        Ok(())
    }
    /// The rows in `state`, oldest first.
    pub(in crate::app) fn in_state(&self, state: &str) -> Result<Vec<Row>> {
        let db = self.0.lock().unwrap();
        let mut query = db.prepare(&format!(
            "SELECT {} FROM workflow_looks WHERE state=?1 ORDER BY created_at, rowid",
            Self::COLUMNS
        ))?;
        let rows = query.query_map([state], Self::row)?;
        rows.map(|r| Ok(r?)).collect()
    }
    /// The rows of one run, newest first.
    pub(in crate::app) fn of_run(&self, run: &str) -> Result<Vec<Row>> {
        let db = self.0.lock().unwrap();
        let mut query = db.prepare(&format!(
            "SELECT {} FROM workflow_looks WHERE run=?1 ORDER BY updated_at DESC, rowid DESC",
            Self::COLUMNS
        ))?;
        let rows = query.query_map([run], Self::row)?;
        rows.map(|r| Ok(r?)).collect()
    }
    /// How many automatic looks started since `since` (Unix seconds).
    pub(in crate::app) fn automatic_since(&self, since: i64) -> Result<usize> {
        let db = self.0.lock().unwrap();
        // A look covers several keys of one run; it counts once, by the look's ID.
        let count: i64 = db.query_row(
            "SELECT COUNT(DISTINCT json_extract(payload, '$.look')) FROM workflow_looks WHERE requested=0 AND state IN ('running','done','failed') AND json_extract(payload, '$.started_at') >= ?1",
            [since],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or_default())
    }
    /// When the last look of a run started.
    pub(in crate::app) fn last_started(&self, run: &str) -> Result<Option<i64>> {
        let db = self.0.lock().unwrap();
        Ok(db
            .query_row(
                "SELECT MAX(json_extract(payload, '$.started_at')) FROM workflow_looks WHERE run=?1 AND state IN ('running','done','failed')",
                [run],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }
}

/// The supervisor's state on the profile: its looks, whether one runs now, and the coding
/// runs seen without a live session at the last tick.
pub(in crate::app) struct Supervision {
    pub(in crate::app) looks: Looks,
    running: AtomicBool,
    suspects: std::sync::Mutex<HashSet<String>>,
    /// The actions being carried out in this process (`run/look/action`).
    acting: std::sync::Mutex<HashSet<String>>,
    /// The actions left attempting by a host that stopped were swept once.
    swept: AtomicBool,
}
impl Supervision {
    /// Open the looks; one left running by a host that stopped failed with it and is never
    /// repeated.
    pub(in crate::app) fn open(path: &Path) -> Result<Self> {
        let looks = Looks::open(path)?;
        let now = now();
        for row in looks.in_state("running")? {
            let mut payload = row.payload.clone();
            payload["failure"] = json!(Failure::of(Cause::HostRestarted));
            looks.set(&row.key, "failed", &payload, now)?;
        }
        Ok(Self {
            looks,
            running: AtomicBool::new(false),
            suspects: std::sync::Mutex::new(HashSet::new()),
            acting: std::sync::Mutex::new(HashSet::new()),
            swept: AtomicBool::new(false),
        })
    }
    /// Whether a look's session runs now (the updater waits).
    pub(in crate::app) fn looking(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}
/// A look in flight; the flag drops with it.
struct Running(Arc<Host>);
impl Drop for Running {
    fn drop(&mut self) {
        self.0
            .state
            .workflows
            .supervision
            .running
            .store(false, Ordering::SeqCst);
    }
}

/// An event a run shows now, with the key that makes it occur once.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct Event {
    pub key: String,
    pub run: String,
    pub event: &'static str,
}

/// What the tick observed beside the runs: the machines' presence (online, last seen), the
/// sessions' life of the implementation runs it asked about (alive, handed to the user) and the
/// coding runs seen running without a live session at the previous tick.
#[derive(Default)]
pub(in crate::app) struct Observed {
    pub machines: HashMap<String, (bool, i64)>,
    pub life: HashMap<String, (bool, bool)>,
    pub suspects: HashSet<String>,
}

/// The implementation runs whose session's life the events need: every implementation run of
/// a run that ended or stopped in the last day, and the active coding run of a run coding.
pub(in crate::app) fn watched(runs: &[Run], now: i64) -> Vec<String> {
    let mut ids = Vec::new();
    for run in runs {
        let closed =
            run.state.finished() || matches!(run.state, Phase::Failed | Phase::Interrupted);
        if closed && now - run.updated_at <= DAY {
            ids.extend(run.implementations().map(|i| i.id.clone()));
        } else if run.state == Phase::Coding
            && let Some(active) = run.active_implementation()
        {
            ids.push(active.id.clone());
        }
    }
    ids
}

/// The events the runs show now (a pure function of the runs and what the tick observed), and
/// the coding runs to watch again at the next tick. Runs of a paused project show none: the
/// person is handling them.
pub(in crate::app) fn events(
    runs: &[Run],
    observed: &Observed,
    settings: &Supervisor,
    pauses: &[Pause],
    now: i64,
) -> (Vec<Event>, HashSet<String>) {
    let mut found = Vec::new();
    let mut suspects = HashSet::new();
    let mut add = |run: &Run, event: &'static str, detail: String| {
        found.push(Event {
            key: format!("{}:{event}:{detail}", run.id),
            run: run.id.clone(),
            event,
        });
    };
    for run in runs {
        if record::paused(pauses, &run.item.project) {
            continue;
        }
        let stage_start = run.timeline.last().map_or(run.created_at, |s| s.started_at);
        // A run that stopped in the last day, unless the person stopped it with a pause: turning
        // the supervisor on does not look at every old stop.
        if run.state.stopped()
            && run.cause.as_deref() != Some("paused")
            && now - run.updated_at <= DAY
        {
            add(
                run,
                "run_stopped",
                format!("{}:{stage_start}", run.timeline.len()),
            );
        }
        // A run that waits for a slot, or for a launch its machine has not accepted.
        let launch_waits = run.state.takes_slot()
            && run.admitted
            && run.active_implementation().is_some_and(|i| !i.started);
        if (run.waits_for_slot() || launch_waits)
            && now - stage_start >= i64::from(settings.wait_hours) * 3600
        {
            add(run, "run_waiting", stage_start.to_string());
        }
        // A machine the run needs that has been unseen too long.
        if run.state.driven() {
            let machine = match run.active_implementation() {
                Some(active) if active.started => Some(active.machine.clone()),
                _ if run.waits_for_slot() || launch_waits => {
                    run.definition.as_ref().map(|d| d.machine.clone())
                }
                _ => None,
            };
            if let Some(machine) = machine
                && let Some((false, seen)) = observed.machines.get(&machine).copied()
                && now - seen >= i64::from(settings.offline_minutes) * 60
            {
                add(run, "machine_offline", format!("{machine}:{seen}"));
            }
        }
        // A session alive on a run that ended, not handed to the user.
        let closed =
            run.state.finished() || matches!(run.state, Phase::Failed | Phase::Interrupted);
        if closed && now - run.updated_at <= DAY {
            for implementation in run.implementations() {
                if observed.life.get(&implementation.id) == Some(&(true, false)) {
                    add(
                        run,
                        "session_disagrees",
                        format!("{}:alive", implementation.id),
                    );
                }
            }
        }
        // A coding run read `running` with no live session, on two ticks in a row.
        if run.state == Phase::Coding
            && let Some(active) = run.active_implementation()
            && active.started
            && active.state.as_deref() == Some("running")
            && observed
                .life
                .get(&active.id)
                .is_some_and(|(alive, _)| !alive)
        {
            if observed.suspects.contains(&active.id) {
                add(
                    run,
                    "session_disagrees",
                    format!("{}:no_session", active.id),
                );
            } else {
                suspects.insert(active.id.clone());
            }
        }
    }
    (found, suspects)
}

/// One look of the supervisor every tick, beside the engine: with the supervisor off it reads
/// nothing and records nothing.
pub(in crate::app) async fn tick(host: &Arc<Host>, azure: &Azure, now: i64) -> Result<()> {
    let state = &host.state;
    if !engine::may_run(state) {
        return Ok(());
    }
    // Once after a start, even with the supervisor off: an action a stopped host left
    // attempting becomes uncertain.
    if !state
        .workflows
        .supervision
        .swept
        .swap(true, Ordering::SeqCst)
    {
        sweep(host, now)?;
    }
    let settings = state.settings.map()?.workflow_settings.supervisor;
    let Some(agent) = settings.active() else {
        return Ok(());
    };
    let supervision = &state.workflows.supervision;
    let mut runs = state.workflows.db.list(&[], None, RUNS_LISTED)?;
    runs.reverse();
    let pauses = state.workflows.db.pauses()?;
    let mut observed = Observed {
        suspects: supervision.suspects.lock().unwrap().clone(),
        ..Default::default()
    };
    for run in &runs {
        for machine in run
            .implementations()
            .map(|i| i.machine.clone())
            .chain(run.definition.as_ref().map(|d| d.machine.clone()))
        {
            if observed.machines.contains_key(&machine) {
                continue;
            }
            if let Ok(Some(facts)) = implementation::machine_facts(host, &machine).await {
                observed
                    .machines
                    .insert(machine, (facts.own || facts.online, facts.seen));
            }
        }
    }
    for id in watched(&runs, now) {
        if let Some(life) = implementation::session_life(host, &id).await {
            observed.life.insert(
                id,
                (
                    life["session_alive"] == true,
                    life["handed_to_user"] == true,
                ),
            );
        }
    }
    let (found, suspects) = events(&runs, &observed, &settings, &pauses, now);
    *supervision.suspects.lock().unwrap() = suspects;
    for event in found {
        supervision.looks.insert(&Row {
            key: event.key,
            run: event.run,
            event: event.event.into(),
            state: "pending".into(),
            requested: false,
            created_at: now,
            updated_at: now,
            payload: json!({}),
        })?;
    }
    start_next(host, azure, &agent, &settings, now).await
}

/// Start the next look when none runs: a requested one first, then the oldest event whose run
/// was not looked at within the interval, within the daily cap. Boxed: the look it spawns can
/// reach the operations that call it again, so its future needs a nameable type.
fn start_next<'a>(
    host: &'a Arc<Host>,
    azure: &'a Azure,
    agent: &'a Director,
    settings: &'a Supervisor,
    now: i64,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(start_next_now(host, azure, agent, settings, now))
}
async fn start_next_now(
    host: &Arc<Host>,
    azure: &Azure,
    agent: &Director,
    settings: &Supervisor,
    now: i64,
) -> Result<()> {
    let supervision = &host.state.workflows.supervision;
    let looks = &supervision.looks;
    let mut pending = looks.in_state("pending")?;
    // A pending event older than a day is stale.
    for row in pending.iter().filter(|r| now - r.created_at > DAY) {
        looks.set(&row.key, "skipped", &json!({"reason":"stale"}), now)?;
    }
    pending.retain(|r| now - r.created_at <= DAY);
    pending.sort_by_key(|r| (!r.requested, r.created_at));
    let interval = i64::from(settings.min_interval_minutes) * 60;
    let mut chosen = None;
    for row in &pending {
        if !row.requested {
            if looks
                .last_started(&row.run)?
                .is_some_and(|at| now - at < interval)
            {
                continue;
            }
            if looks.automatic_since(now - DAY)? >= settings.looks_per_day as usize {
                looks.set(&row.key, "skipped", &json!({"reason":"daily_limit"}), now)?;
                continue;
            }
        }
        chosen = Some(row.clone());
        break;
    }
    let Some(first) = chosen else {
        return Ok(());
    };
    if supervision.running.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let running = Running(host.clone());
    // Every pending event of that run joins this look.
    let keys: Vec<Row> = pending
        .iter()
        // A requested look covers the requests only, so it never spends the automatic budget;
        // an automatic one covers the run's pending events.
        .filter(|r| r.run == first.run && r.requested == first.requested)
        .cloned()
        .collect();
    let look = uuid::Uuid::new_v4().to_string();
    for row in &keys {
        looks.set(
            &row.key,
            "running",
            &json!({"look": look, "started_at": now}),
            now,
        )?;
    }
    let (host, azure, agent, settings) =
        (host.clone(), azure.clone(), agent.clone(), settings.clone());
    tokio::spawn(async move {
        let _running = running;
        let outcome = Box::pin(look_at(&host, &azure, &agent, &settings, &first, &keys)).await;
        let (state_word, extra) = match &outcome {
            Ok(stored) => (stored.outcome.clone(), json!({"id": stored.id})),
            Err(_) => ("failed".to_owned(), json!({})),
        };
        let at = chrono::Utc::now().timestamp();
        for row in &keys {
            let mut payload = json!({"look": look, "started_at": now});
            if let Some(id) = extra.get("id") {
                payload["session"] = id.clone();
            }
            let _ = host.state.workflows.supervision.looks.set(
                &row.key,
                if state_word == "done" {
                    "done"
                } else {
                    "failed"
                },
                &payload,
                at,
            );
        }
    });
    Ok(())
}

/// The supervisor's agent, when it is on.
fn active(host: &Arc<Host>) -> Result<(Director, Supervisor), String> {
    let settings = host
        .state
        .settings
        .map()
        .map_err(fail)?
        .workflow_settings
        .supervisor;
    let agent = settings.active().ok_or_else(|| {
        "[not_ready] The workflow supervisor is off: turn it on and choose its machine, CLI, model and effort in Settings › Workflows (workflow_settings_update supervisor).".to_owned()
    })?;
    Ok((agent, settings))
}

/// What one look read, as the input to the session and the facts code checks against.
#[derive(Default)]
pub(in crate::app) struct Read {
    pub item: Option<crate::tools::todo::Detail>,
    pub children: Vec<(u64, Option<String>)>,
    pub implementations: Vec<Value>,
    pub reports: Vec<(String, String)>,
    pub sessions: Vec<Value>,
    /// The pull requests read, each with the branch it was read for.
    pub pulls: Vec<(String, crate::ado::workflow::PullRequest)>,
    pub branches: Vec<(crate::workflows::Repository, String, Option<bool>)>,
    pub machines: Vec<Value>,
    pub unread: Vec<String>,
    /// The reads did not answer within their bound.
    pub timed_out: bool,
}

/// Read everything a look needs, side by side, within `READ_SECONDS`; what fails or does not
/// answer in time is in `unread`.
async fn gather(host: &Arc<Host>, azure: &Azure, run: &Run) -> Read {
    let state = &host.state;
    let mut read = Read::default();
    let item = async {
        azure
            .item(
                state,
                run.item.source.as_deref(),
                Some(&run.item.project),
                run.item.id,
            )
            .await
    };
    let children = async {
        let mut states = Vec::new();
        for child in run
            .children
            .iter()
            .filter(|c| c.id != run.item.id)
            .take(MAX_IMPLEMENTATIONS)
        {
            let detail = azure
                .item(
                    state,
                    run.item.source.as_deref(),
                    Some(&run.item.project),
                    child.id,
                )
                .await;
            states.push((child.id, detail.ok().map(|d| d.item.state)));
        }
        states
    };
    let implementations = async {
        let mut records = Vec::new();
        for implementation in run.implementations().take(MAX_IMPLEMENTATIONS) {
            let observed =
                implementation::observe(host, &implementation.machine, &implementation.id).await;
            let life = implementation::session_life(host, &implementation.id).await;
            records.push((implementation.clone(), observed.ok().flatten(), life));
        }
        records
    };
    let pulls = async {
        let mut found = Vec::new();
        let mut branches = Vec::new();
        let mut failed = false;
        for child in &run.children {
            for (repository, branch) in child.delivered() {
                match azure.branch_pulls(state, run, &repository, &branch).await {
                    Ok(pulls) => found.extend(pulls.into_iter().map(|p| (branch.clone(), p))),
                    Err(_) => failed = true,
                }
                let exists = azure
                    .branch_exists(state, run, &repository.id, &branch)
                    .await
                    .ok();
                branches.push((repository, branch, exists));
            }
        }
        (found, branches, failed)
    };
    let sessions = azure.run_sessions(host, run.implementations().map(|i| i.id.clone()).collect());
    let all = async { tokio::join!(item, children, implementations, pulls, sessions) };
    let Ok((item, children, implementations, pulls, sessions)) =
        tokio::time::timeout(std::time::Duration::from_secs(READ_SECONDS), all).await
    else {
        // Nothing answered in time: every check that needs a read says it could not be read.
        read.unread.extend(
            [
                "the work item",
                "some child work items",
                "the implementation runs",
                "some pull requests",
                "some branches",
                "the session summaries",
            ]
            .map(String::from),
        );
        read.timed_out = true;
        return read;
    };
    match item {
        Ok(detail) => read.item = Some(detail),
        Err(_) => read.unread.push("the work item".into()),
    }
    if children.iter().any(|(_, s)| s.is_none()) {
        read.unread.push("some child work items".into());
    }
    read.children = children;
    for (implementation, observed, life) in implementations {
        let Some(observed) = observed else {
            read.unread
                .push(format!("implementation run {}", implementation.id));
            continue;
        };
        let failure = crate::llm::implementation::Failure::of_record(&observed);
        read.implementations.push(json!({
            "id": implementation.id,
            "machine": implementation.machine,
            "state": observed["state"],
            "planning": observed["planning"],
            "provider": observed["provider"], "model": observed["model"], "effort": observed["effort"],
            "started_at": observed["started_at"], "finished_at": observed["finished_at"],
            "failure": failure.map(|f| json!({"cause": f.cause, "text": f.sentence()})),
            "session_alive": life.as_ref().map(|l| l["session_alive"].clone()),
            "handed_to_user": life.as_ref().map(|l| l["handed_to_user"].clone()),
        }));
        if observed["planning"] != true
            && let Some(output) = observed["output"].as_str().filter(|o| !o.trim().is_empty())
        {
            read.reports
                .push((implementation.id.clone(), output.to_owned()));
        }
    }
    let (found, branches, failed) = pulls;
    if failed {
        read.unread.push("some pull requests".into());
    }
    if branches.iter().any(|(_, _, e)| e.is_none()) {
        read.unread.push("some branches".into());
    }
    read.pulls = found;
    read.branches = branches;
    match sessions {
        Ok((summaries, missing)) => {
            read.sessions = summaries;
            if missing {
                read.unread.push("some session summaries".into());
            }
        }
        Err(_) => read.unread.push("the session summaries".into()),
    }
    let mut seen = HashSet::new();
    for machine in run
        .definition
        .iter()
        .map(|d| d.machine.clone())
        .chain(run.implementations().map(|i| i.machine.clone()))
    {
        if !seen.insert(machine.clone()) {
            continue;
        }
        if let Ok(Some(facts)) = implementation::machine_facts(host, &machine).await {
            read.machines.push(json!({
                "id": machine,
                "this_coordinator": facts.own,
                "online": facts.own || facts.online,
                "last_seen": time(facts.seen),
            }));
        }
    }
    read
}

fn field_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other["displayName"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| other.to_string()),
    }
}

/// The input of a look: what code read under `facts`, every third-party text under
/// `untrusted`, bounded to `MAX_INPUT` characters by cutting in a fixed order (the oldest
/// comments, then the oldest revisions, then the session summaries, then the older reports).
pub(in crate::app) fn input(
    run: &Run,
    read: &Read,
    looks: &[Look],
    event: &str,
    pauses: &[Pause],
    language: crate::config::Language,
    now: i64,
) -> (String, Vec<String>) {
    let detail = read.item.as_ref();
    let description = detail
        .and_then(|d| {
            d.fields
                .iter()
                .find(|f| {
                    matches!(
                        f.reference.as_str(),
                        "System.Description" | "Microsoft.VSTS.TCM.ReproSteps"
                    )
                })
                .map(|f| crate::adapters::teams::plain_text(&field_text(&f.value)))
        })
        .map(|d| cut(&d, DESCRIPTION_CHARS))
        .unwrap_or_default();
    let mut comments: Vec<Value> = detail
        .map(|d| {
            let start = d.comments.len().saturating_sub(MAX_COMMENTS);
            d.comments[start..]
                .iter()
                .map(|c| json!({"author": cut(&c.author, 100), "at": c.created_at, "text": cut(&c.text, COMMENT_CHARS)}))
                .collect()
        })
        .unwrap_or_default();
    let mut revisions: Vec<Value> = detail
        .map(|d| {
            let start = d.history.len().saturating_sub(MAX_REVISIONS);
            d.history[start..]
                .iter()
                .map(|c| json!({
                    "author": cut(&c.author, 100), "at": c.at,
                    "fields": c.fields.iter().take(5).map(|f| json!({
                        "field": cut(&f.name, 100), "value": cut(&field_text(&f.value), REVISION_CHARS),
                    })).collect::<Vec<_>>(),
                }))
                .collect()
        })
        .unwrap_or_default();
    let mut sessions = read.sessions.clone();
    let mut reports: Vec<Value> = read
        .reports
        .iter()
        .map(|(id, text)| json!({"implementation_run": id, "text": tail(text, REPORT_CHARS)}))
        .collect();
    let children: Vec<Value> = run
        .children
        .iter()
        .take(50)
        .map(|c| json!({
            "id": c.id, "type": c.kind, "outcome": c.outcome,
            "repository": c.repository.as_ref().map(|r| r.name.clone()),
            "branches": c.delivered().into_iter().map(|(r, b)| json!({"repository": r.name, "branch": b})).collect::<Vec<_>>(),
            "coding_run": c.coding.as_ref().map(|i| json!({"id": i.id, "state": i.state, "started": i.started})),
        }))
        .collect();
    let facts = json!({
        "item": detail.map(|d| json!({"id": d.item.id, "type": d.item.kind, "state": d.item.state, "project": d.item.project})),
        "children": read.children.iter().map(|(id, state)| json!({"id": id, "state": state})).collect::<Vec<_>>(),
        "implementation_runs": read.implementations,
        "pull_requests": read.pulls.iter().take(50).map(|(branch, p)| json!({
            "id": p.id, "repository": p.repository, "branch": branch, "status": p.status,
            "created_at": p.created_at, "closed_at": p.closed_at,
        })).collect::<Vec<_>>(),
        "branches": read.branches.iter().map(|(r, b, e)| json!({"repository": r.name, "branch": b, "exists": e})).collect::<Vec<_>>(),
        "machines": read.machines,
        "paused": record::paused(pauses, &run.item.project),
    });
    let workflow = run.definition.as_ref().map(|w| {
        json!({
            "id": w.id, "name": w.name,
            "repositories": w.repositories.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
        })
    });
    let earlier: Vec<Value> = looks
        .iter()
        .rev()
        .take(2)
        .map(|l| json!({
            "created_at": time(l.created_at), "event": l.event, "outcome": l.outcome,
            "assessment": l.assessment,
            "actions": l.actions.iter().map(|a| json!({"action": a.action, "state": a.state})).collect::<Vec<_>>(),
        }))
        .collect();
    let mut truncated = Vec::new();
    loop {
        let value = json!({
            "language": language,
            "generated_at": time(now),
            "event": event,
            "allowed_actions": SUPERVISOR_ACTIONS,
            "run": {
                "id": run.id, "workflow": workflow, "state": run.state.word(),
                "stage": run.stopped_in.unwrap_or(run.state).word(),
                "created_at": time(run.created_at), "finished_at": run.finished_at.and_then(time),
                "stages": run.timeline.iter().take(100).map(|s| json!({
                    "stage": s.stage.word(), "started_at": time(s.started_at),
                    "finished_at": s.finished_at.and_then(time), "outcome": s.outcome,
                })).collect::<Vec<_>>(),
                "children": children,
                "waiting_for_slot": run.waits_for_slot(),
                "continues": run.continues, "restarted_as": run.restarted_as,
                "attention": run.attention.as_ref().map(|a| cut(a, 1000)),
                "error": run.error.as_ref().map(|e| cut(e, 1000)),
            },
            "facts": facts,
            "untrusted": {
                "item": {
                    "title": cut(&run.item.title, 500),
                    "description": description,
                    "comments": comments,
                    "revisions": revisions,
                },
                "children": run.children.iter().take(50).map(|c| json!({"id": c.id, "title": cut(&c.title, TITLE_CHARS)})).collect::<Vec<_>>(),
                "reports": reports,
                "sessions": sessions,
                "handover": run.handover,
                "earlier_looks": earlier,
            },
            "unread": read.unread,
            "truncated": truncated,
        });
        let text = value.to_string();
        if text.chars().count() <= MAX_INPUT {
            return (text, truncated);
        }
        let note = |truncated: &mut Vec<String>, what: &str| {
            if !truncated.iter().any(|t| t == what) {
                truncated.push(what.to_owned());
            }
        };
        if !comments.is_empty() {
            comments.remove(0);
            note(&mut truncated, "the oldest comments");
        } else if !revisions.is_empty() {
            revisions.remove(0);
            note(&mut truncated, "the oldest revisions");
        } else if !sessions.is_empty() {
            sessions.remove(0);
            note(&mut truncated, "session summaries");
        } else if reports.len() > 1 {
            reports.remove(0);
            note(&mut truncated, "older reports");
        } else if let Some(last) = reports.pop() {
            let text = last["text"].as_str().unwrap_or_default();
            if text.chars().count() > 500 {
                reports.push(json!({"implementation_run": last["implementation_run"], "text": tail(text, text.chars().count() / 2)}));
            }
            note(&mut truncated, "the latest report");
        } else {
            // The rest is bounded by code already; cut the run's own lists as a last resort.
            return (cut(&text, MAX_INPUT), truncated);
        }
    }
}

/// The supervisor's answer, exactly the closed schema (seven properties, nothing else).
fn shaped(answer: Option<&Value>) -> Option<&serde_json::Map<String, Value>> {
    let object = answer?.as_object()?;
    let keys = [
        "assessment",
        "claims",
        "unverified",
        "handover",
        "work_items",
        "summary",
        "actions",
    ];
    (object.len() == keys.len() && keys.iter().all(|k| object.contains_key(*k))).then_some(object)
}

/// A text the model wrote, cleaned (no URL, link, HTML or mention marker of its own) and cut.
fn clean(text: &Value, max: usize) -> String {
    super::summary::clean(text.as_str().unwrap_or_default(), max)
}
fn one_line(text: &Value, max: usize) -> String {
    crate::workflows::director::one_line(&clean(text, max * 2), max)
}

/// A pull request number as a claim refers to it (`!7`, `#7`, `PR 7`, `7`).
fn pull_number(reference: &str) -> Option<u64> {
    let digits: String = reference
        .trim()
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// Whether a claim's words say a pull request was merged or completed.
fn claims_merge(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "merged",
        "completed",
        "fusionad",
        "completad",
        "mergead",
        "integrad",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

/// Check one claim against what code read.
fn check(kind: &str, reference: &str, claim: String, from: &str, run: &Run, read: &Read) -> Check {
    let result = |verified: bool, fact: String| {
        (
            if verified { "verified" } else { "not_verified" }.to_owned(),
            fact,
        )
    };
    let (outcome, reference) = match kind {
        "pull_request" => match pull_number(reference) {
            Some(number) => {
                let fact = match read.pulls.iter().map(|(_, p)| p).find(|p| p.id == number) {
                    Some(pull) if claims_merge(&claim) => result(
                        pull.status == "completed",
                        format!("Pull request !{number} is {}.", pull.status),
                    ),
                    Some(pull) => result(
                        true,
                        format!("Pull request !{number} exists; it is {}.", pull.status),
                    ),
                    None if read.unread.iter().any(|u| u.contains("pull requests")) => {
                        result(false, format!("Pull request !{number} could not be read."))
                    }
                    None => result(
                        false,
                        format!(
                            "Pull request !{number} is not a pull request of the branches this run reported."
                        ),
                    ),
                };
                (fact, number.to_string())
            }
            None => (
                result(false, "Nothing code read checks this.".into()),
                String::new(),
            ),
        },
        "branch" => {
            let name = reference.trim().trim_start_matches("refs/heads/");
            if crate::ado::workflow::valid_branch(name) {
                let fact = match read.branches.iter().find(|(_, b, _)| b == name) {
                    Some((repository, _, Some(true))) => result(
                        true,
                        format!("Branch {name} exists in {}.", repository.name),
                    ),
                    Some((repository, _, Some(false))) => {
                        // A merged branch may be deleted: a completed pull request read for that
                        // very branch and repository counts, never another branch's.
                        let merged = read.pulls.iter().any(|(branch, p)| {
                            branch == name
                                && p.status == "completed"
                                && p.repository.eq_ignore_ascii_case(&repository.id)
                        }) && run
                            .children
                            .iter()
                            .any(|c| c.delivered().iter().any(|(_, b)| b == name));
                        result(
                            merged,
                            if merged {
                                format!(
                                    "Branch {name} no longer exists in {}; a completed pull request came from it.",
                                    repository.name
                                )
                            } else {
                                format!("Branch {name} does not exist in {}.", repository.name)
                            },
                        )
                    }
                    Some((_, _, None)) => {
                        result(false, format!("Branch {name} could not be read."))
                    }
                    None if read.unread.iter().any(|u| u.contains("branches")) => {
                        result(false, format!("Branch {name} could not be read."))
                    }
                    None => result(
                        false,
                        format!("Branch {name} is not a branch this run reported."),
                    ),
                };
                (fact, name.to_owned())
            } else {
                (
                    result(false, "Nothing code read checks this.".into()),
                    String::new(),
                )
            }
        }
        "item_state" => {
            let wanted = reference.trim();
            let states: Vec<(u64, String)> = read
                .item
                .iter()
                .map(|d| (d.item.id, d.item.state.clone()))
                .chain(
                    read.children
                        .iter()
                        .filter_map(|(id, s)| s.clone().map(|s| (*id, s))),
                )
                .collect();
            if wanted.is_empty() || wanted.chars().count() > 100 {
                (
                    result(false, "Nothing code read checks this.".into()),
                    String::new(),
                )
            } else if let Some((id, state)) =
                states.iter().find(|(_, s)| s.eq_ignore_ascii_case(wanted))
            {
                (
                    result(true, format!("Work item #{id} is in state {state}.")),
                    state.clone(),
                )
            } else if let Some((id, state)) = states.first() {
                (
                    result(false, format!("Work item #{id} is in state {state}.")),
                    // What the model wrote, cleaned like any of its text.
                    crate::workflows::director::one_line(&super::summary::clean(wanted, 100), 100),
                )
            } else {
                (
                    result(false, "The work item's state could not be read.".into()),
                    String::new(),
                )
            }
        }
        _ => (
            result(false, "Nothing code read checks this.".into()),
            String::new(),
        ),
    };
    // A claim reaches the record cleaned, whether the model or a report wrote it.
    let claim = crate::workflows::director::one_line(
        &super::summary::clean(&claim, LINE_CHARS * 2),
        LINE_CHARS,
    );
    Check {
        claim,
        kind: if matches!(kind, "pull_request" | "branch" | "item_state") && !reference.is_empty() {
            kind.to_owned()
        } else {
            "other".into()
        },
        reference,
        result: outcome.0,
        fact: outcome.1,
        from: from.into(),
    }
}

/// The claims code finds in each final report by itself: a pull request number in a sentence
/// with a merge word, and every branch the run recorded from its branch lines.
fn report_claims(run: &Run, read: &Read) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    let number =
        regex::Regex::new(r"(?i)(?:!|\bpr\s*[#!]?|\bpull request\s*[#!]?)\s*(\d{1,9})\b").ok();
    for (_, report) in &read.reports {
        for sentence in report.split(['.', '\n', ';']) {
            if !claims_merge(sentence) {
                continue;
            }
            if let Some(re) = &number {
                for capture in re.captures_iter(sentence) {
                    found.push((
                        "pull_request".to_owned(),
                        capture[1].to_owned(),
                        cut(sentence.trim(), LINE_CHARS),
                    ));
                }
            }
        }
    }
    for child in &run.children {
        for (repository, branch) in child.delivered() {
            found.push((
                "branch".to_owned(),
                branch.clone(),
                format!("Branch {}: {branch}", repository.name),
            ));
        }
    }
    found
}

/// What code keeps of the answer: everything cleaned and cut, checks made by code, the drafts
/// and actions it accepts.
pub(in crate::app) fn judge(
    answer: &serde_json::Map<String, Value>,
    run: &Run,
    read: &Read,
    language: crate::config::Language,
) -> Look {
    let mut look = Look::default();
    let assessment = &answer["assessment"];
    look.assessment = Some(Assessment {
        done: clean(&assessment["done"], PART_CHARS),
        left: clean(&assessment["left"], PART_CHARS),
        recommendation: clean(&assessment["recommendation"], PART_CHARS),
    });
    let mut checks: Vec<Check> = Vec::new();
    for claim in answer["claims"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_CLAIMS)
    {
        let kind = claim["kind"].as_str().unwrap_or("other");
        let reference = claim["ref"].as_str().unwrap_or_default();
        checks.push(check(
            kind,
            reference,
            one_line(&claim["text"], LINE_CHARS),
            "supervisor",
            run,
            read,
        ));
    }
    for (kind, reference, claim) in report_claims(run, read) {
        let covered = checks.iter().any(|c| {
            c.kind == kind
                && (c.reference == reference
                    || pull_number(&c.reference) == pull_number(&reference)
                        && kind == "pull_request")
        });
        if !covered {
            checks.push(check(
                &kind,
                &reference,
                crate::workflows::director::one_line(&claim, LINE_CHARS),
                "report",
                run,
                read,
            ));
        }
    }
    look.checks = checks;
    look.unverified = answer["unverified"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_UNVERIFIED)
        .map(|u| one_line(u, LINE_CHARS))
        .filter(|u| !u.is_empty())
        .collect();
    look.handover = clean(&answer["handover"], record::MAX_HANDOVER);
    // Work item drafts: only for the workflow's repositories (or none named).
    let repositories = run
        .definition
        .as_ref()
        .map(|w| w.repositories.clone())
        .unwrap_or_default();
    let mut indexes: HashMap<usize, usize> = HashMap::new();
    for (index, draft) in answer["work_items"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if look.work_items.len() >= MAX_DRAFTS {
            look.dropped += 1;
            continue;
        }
        let kind = draft["type"].as_str().unwrap_or_default();
        let named = draft["repository"].as_str().unwrap_or_default().trim();
        let repository = repositories
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(named))
            .cloned();
        let title = one_line(&draft["title"], TITLE_CHARS);
        if !matches!(kind, "requirement" | "bug")
            || title.is_empty()
            || (!named.is_empty() && repository.is_none())
        {
            look.dropped += 1;
            continue;
        }
        indexes.insert(index, look.work_items.len());
        look.work_items.push(Draft {
            kind: kind.into(),
            title,
            description: clean(&draft["description"], PART_CHARS),
            repository,
            reason: one_line(&draft["reason"], LINE_CHARS),
        });
    }
    let summary = answer["summary"].as_str().unwrap_or_default();
    if !super::summary::clean(summary, super::summary::MAX_CHARS).is_empty() {
        look.summary = super::summary::compose(summary, run, language);
    }
    for proposed in answer["actions"].as_array().into_iter().flatten() {
        let action = proposed["action"].as_str().unwrap_or_default();
        let about = proposed["run"].as_str().unwrap_or_default().trim();
        let sessions = proposed["sessions"].as_str().unwrap_or_default();
        let work_item = proposed["work_item"]
            .as_i64()
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| indexes.get(&i).copied());
        let usable = SUPERVISOR_ACTIONS.contains(&action)
            && (about.is_empty() || about == run.id)
            && look.actions.len() < MAX_ACTIONS
            && !look
                .actions
                .iter()
                // Each action once; a work item once per draft.
                .any(|a| {
                    a.action == action && (action != "create_work_item" || a.work_item == work_item)
                })
            && match action {
                "pause_project" => matches!(sessions, "stop" | "finish"),
                "create_work_item" => work_item.is_some(),
                "post_summary" => !look.summary.is_empty(),
                "restart" => !look.handover.is_empty() && !run.state.finished(),
                "move_ahead" => waits_in_line(run),
                "resume" => run.state.stopped(),
                "cancel" => !run.state.finished(),
                _ => false,
            };
        if !usable {
            look.dropped += 1;
            continue;
        }
        look.actions.push(Action {
            id: format!("a{}", look.actions.len() + 1),
            action: action.into(),
            sessions: if action == "pause_project" {
                sessions.into()
            } else {
                String::new()
            },
            work_item: if action == "create_work_item" {
                work_item
            } else {
                None
            },
            reason: one_line(&proposed["reason"], LINE_CHARS),
            state: "proposed".into(),
            ..Default::default()
        });
    }
    look
}

/// Whether a run waits in the line for a slot (or will, once classified).
fn waits_in_line(run: &Run) -> bool {
    run.waits_for_slot() || run.state == Phase::Classifying
}

/// One look at a run: read, ask, judge, store. Returns the stored look.
async fn look_at(
    host: &Arc<Host>,
    azure: &Azure,
    agent: &Director,
    settings: &Supervisor,
    first: &Row,
    rows: &[Row],
) -> Result<Look> {
    let state = &host.state;
    let run = found(state, &first.run).map_err(anyhow::Error::msg)?;
    let pauses = state.workflows.db.pauses()?;
    let map = state.settings.map()?;
    let language = state.settings.config()?.llm.language;
    let now = now();
    let read = gather(host, azure, &run).await;
    let event = if first.requested {
        "requested"
    } else {
        first.event.as_str()
    };
    let (input, truncated) = input(&run, &read, &run.supervision, event, &pauses, language, now);
    let instructions =
        crate::agent_prompts::resolve(&map.agent_prompts, crate::agent_prompts::SUPERVISE);
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
    let missed: Vec<String> = state
        .workflows
        .supervision
        .looks
        .of_run(&run.id)?
        .into_iter()
        .filter(|r| {
            r.state == "skipped"
                && r.payload["reason"] == "daily_limit"
                && r.payload["seen"].is_null()
        })
        .map(|r| r.event)
        .collect();
    let mut look = Look {
        event: event.into(),
        keys: rows.iter().map(|r| r.key.clone()).collect(),
        missed,
        requested: first.requested,
        created_at: now,
        supervised_by: agent.clone(),
        outcome: "failed".into(),
        unread: read.unread.clone(),
        truncated,
        marker: marker(&run, &pauses),
        ..Default::default()
    };
    let answer = Box::pin(implementation::director::ask(
        host,
        implementation::director::Ask {
            machine: &agent.machine,
            task: DirectTask::Supervise,
            director: agent,
            instructions: &instructions,
            input,
            item: &item,
            seconds: Some(u64::from(settings.session_seconds)),
        },
    ))
    .await;
    match answer {
        Ok(answer) => {
            look.id = answer.run.clone();
            look.session_id = answer.session_id.clone();
            let _ = implementation::director::annotate(
                host,
                &answer.run,
                "workflow_run",
                json!(run.id),
            );
            match shaped(answer.direction.answer.as_ref()) {
                Some(object) => {
                    let judged = judge(object, &run, &read, language);
                    look = Look {
                        assessment: judged.assessment,
                        checks: judged.checks,
                        unverified: judged.unverified,
                        handover: judged.handover,
                        work_items: judged.work_items,
                        summary: judged.summary,
                        actions: judged.actions,
                        dropped: judged.dropped,
                        outcome: "done".into(),
                        ..look
                    };
                }
                None => {
                    let failure = answer
                        .direction
                        .failure
                        .unwrap_or(Failure::of(Cause::InvalidAnswer));
                    look.failure = Some(failure);
                    let _ = implementation::director::annotate(
                        host,
                        &answer.run,
                        "state",
                        json!("failed"),
                    );
                    let _ = implementation::director::annotate(
                        host,
                        &answer.run,
                        "output",
                        json!("The workflow supervisor's answer is not the JSON it was asked for."),
                    );
                    let _ = implementation::director::annotate(
                        host,
                        &answer.run,
                        "failure",
                        json!(failure),
                    );
                }
            }
        }
        Err(error) => {
            look.failure = Some(crate::llm::implementation::of_error(&error, Cause::Unknown));
            // The session's record is the `request_runs` entry `ask` wrote; its ID is not
            // returned on an error, so the look keeps a fresh one.
            look.id = uuid::Uuid::new_v4().to_string();
        }
    }
    store(host, &run.id, look).await
}

/// Store a look on its run, under the run's lock, and do what the supervisor does by itself:
/// move the run ahead in the line when it proposed so and the run still waits there.
async fn store(host: &Arc<Host>, id: &str, mut look: Look) -> Result<Look> {
    let state = &host.state;
    let lock = state.workflows.lock(id);
    let _guard = lock.lock().await;
    let mut run = found(state, id).map_err(anyhow::Error::msg)?;
    let now = now();
    for action in look.actions.iter_mut().filter(|a| a.action == "move_ahead") {
        if waits_in_line(&run) {
            run.ahead_since.get_or_insert(now);
            action.state = "done".into();
            action.by = Some("supervisor".into());
            action.at = Some(now);
        } else {
            action.state = "expired".into();
        }
    }
    run.supervision.push(look.clone());
    while run.supervision.len() > MAX_LOOKS {
        run.supervision.remove(0);
    }
    state.workflows.db.save(&run)?;
    // The skipped events it was told about are not told again.
    for row in state.workflows.supervision.looks.of_run(id)? {
        if row.state == "skipped"
            && row.payload["reason"] == "daily_limit"
            && row.payload["seen"].is_null()
        {
            let mut payload = row.payload.clone();
            payload["seen"] = json!(look.id);
            state
                .workflows
                .supervision
                .looks
                .set(&row.key, "skipped", &payload, row.updated_at)?;
        }
    }
    Ok(look)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuperviseArgs {
    run: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupervisionArgs {
    run: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActArgs {
    run: String,
    look: String,
    action: String,
    /// The person's edited handover, for `restart`.
    #[serde(default)]
    note: Option<String>,
    /// The chat to post the drafted summary to, for `post_summary`.
    #[serde(default)]
    chat: Option<String>,
    #[serde(default)]
    mentions: Vec<String>,
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DismissArgs {
    run: String,
    look: String,
    action: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequeueArgs {
    run: String,
    ahead: bool,
}

pub(super) async fn operation(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
    azure: &Azure,
) -> Result<Value, String> {
    match method {
        "workflow_supervise" => supervise(host, parse(args)?, azure).await,
        "workflow_supervision" => supervision(host, parse(args)?),
        "workflow_supervision_act" => act(host, parse(args)?, azure).await,
        "workflow_supervision_dismiss" => dismiss(host, parse(args)?).await,
        "workflow_run_requeue" => requeue(host, parse(args)?).await,
        _ => Err(invalid("Unknown workflow operation.")),
    }
}

/// The looks queued or running for a run.
pub(in crate::app) fn queued(state: &DesktopState, run: &str) -> Vec<Value> {
    state
        .workflows
        .supervision
        .looks
        .of_run(run)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| matches!(r.state.as_str(), "pending" | "running"))
        .map(|r| json!({"key": r.key, "event": r.event, "state": if r.state == "pending" {"queued"} else {"running"}, "requested": r.requested, "created_at": r.created_at}))
        .collect()
}

/// `workflow_supervise {run}`: ask for a look at one run now.
async fn supervise(host: &Arc<Host>, args: SuperviseArgs, azure: &Azure) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let (agent, settings) = active(host)?;
    let run = found(state, &args.run)?;
    if !queued(state, &run.id).is_empty() {
        return Err(conflict(format!(
            "A look at workflow run {} is already queued or running.",
            run.id
        )));
    }
    let now = now();
    if let Some(at) = state
        .workflows
        .supervision
        .looks
        .last_started(&run.id)
        .map_err(fail)?
        .filter(|at| now - at < REQUEST_SECONDS)
    {
        return Err(conflict(format!(
            "The supervisor looked at workflow run {} {} seconds ago; ask again in {} seconds.",
            run.id,
            now - at,
            REQUEST_SECONDS - (now - at)
        )));
    }
    let key = format!("{}:requested:{}", run.id, uuid::Uuid::new_v4());
    state
        .workflows
        .supervision
        .looks
        .insert(&Row {
            key: key.clone(),
            run: run.id.clone(),
            event: "requested".into(),
            state: "pending".into(),
            requested: true,
            created_at: now,
            updated_at: now,
            payload: json!({}),
        })
        .map_err(fail)?;
    start_next(host, azure, &agent, &settings, now)
        .await
        .map_err(fail)?;
    Ok(
        json!({"run": run.id, "look": key, "state": if state.workflows.supervision.looking() { "running" } else { "queued" }}),
    )
}

/// `workflow_supervision {run}`: the run's looks, newest last, and any queued or running.
fn supervision(host: &Arc<Host>, args: SupervisionArgs) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let run = found(state, &args.run)?;
    Ok(json!({"run": run.id, "looks": run.supervision, "queued": queued(state, &run.id)}))
}

/// What an action is about, as its confirmation names it.
pub(in crate::app) struct About<'a> {
    pub run: &'a str,
    pub item: u64,
    pub project: &'a str,
    pub stage: &'a str,
}
impl<'a> About<'a> {
    fn of(run: &'a Run) -> Self {
        Self {
            run: &run.id,
            item: run.item.id,
            project: &run.item.project,
            stage: run.stopped_in.unwrap_or(run.state).word(),
        }
    }
    /// From a run as `workflow_run_status` shows it.
    pub(in crate::app) fn shown(run: &'a Value) -> Self {
        Self {
            run: run["id"].as_str().unwrap_or("this"),
            item: run["item"]["id"].as_u64().unwrap_or_default(),
            project: run["item"]["project"].as_str().unwrap_or("its project"),
            stage: run["stage"].as_str().unwrap_or("its stage"),
        }
    }
}

/// The note a restart uses, as the person gave it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) enum NoteChoice<'a> {
    /// No note given: the supervisor's draft, which the coding runs read as the supervisor's.
    Draft,
    /// The person's own note (an edited draft).
    Edited(&'a str),
    /// The person emptied the note: the run restarts without one.
    Empty,
}
impl<'a> NoteChoice<'a> {
    /// From the `note` argument: absent is the draft, blank is none, the draft unchanged is the
    /// draft, anything else is the person's.
    pub(in crate::app) fn of(note: Option<&'a str>, draft: &str) -> Self {
        match note.map(str::trim) {
            None => Self::Draft,
            Some("") => Self::Empty,
            Some(text) if text == draft.trim() => Self::Draft,
            Some(text) => Self::Edited(text),
        }
    }
}

/// What a person confirms for an action, with its draft: the action, the run and the exact
/// note or text it uses, and for a post its chat and mentions.
pub(in crate::app) fn describe_action(
    about: &About,
    look: &Look,
    action: &Action,
    note: NoteChoice,
    chat: Option<&str>,
    mentions: &[String],
) -> String {
    let run = about.run;
    match action.action.as_str() {
        "restart" => {
            let start = format!(
                "Restart workflow run {run}: its active implementation run is cancelled, every live session it started is stopped, and a new run of the same workflow starts for work item {} from its first stage",
                about.item
            );
            match note {
                NoteChoice::Draft => format!(
                    "{start}, with the supervisor's drafted handover note ({} characters), which its coding runs read as the supervisor's untrusted text. The note:\n\n{}",
                    look.handover.chars().count(),
                    look.handover
                ),
                NoteChoice::Edited(text) => format!(
                    "{start}, with your handover note ({} characters). The note:\n\n{text}",
                    text.chars().count()
                ),
                NoteChoice::Empty => format!("{start}, without a handover note."),
            }
        }
        "resume" => format!(
            "Resume workflow run {run} from {}, accepting what stopped it.",
            about.stage
        ),
        "cancel" => format!(
            "Cancel workflow run {run}: its active implementation run is cancelled and every live session it started is stopped; the file changes stay."
        ),
        "pause_project" => format!(
            "Pause every workflow run of project {} and {} their sessions.",
            about.project,
            if action.sessions == "stop" {
                "stop"
            } else {
                "let finish"
            }
        ),
        "post_summary" => format!(
            "Post the supervisor's summary of workflow run {run} to the Teams chat {}, as you, mentioning {}: one message, never retried. The text posted is:\n\n{}",
            chat.unwrap_or("you choose"),
            if mentions.is_empty() {
                "nobody".to_owned()
            } else {
                mentions.join(", ")
            },
            look.summary
        ),
        "create_work_item" => {
            "Open Create request with the drafted work item; it is written only when you confirm there."
                .into()
        }
        _ => String::new(),
    }
}

/// The confirmation of `workflow_supervision_act` from a run as `workflow_run_status` shows
/// it (MCP's question), or none when its look or action is not there.
pub(in crate::app) fn describe_act(status: &Value, args: &Value) -> Option<String> {
    let run = &status["run"];
    let look: Look = run["supervision"]
        .as_array()?
        .iter()
        .find(|l| l["id"] == args["look"])
        .and_then(|l| serde_json::from_value(l.clone()).ok())?;
    let wanted = args["action"].as_str().unwrap_or_default();
    let action = look
        .actions
        .iter()
        .find(|a| a.id == wanted || a.action == wanted)?
        .clone();
    let mentions: Vec<String> = args["mentions"]
        .as_array()
        .map(|m| {
            m.iter()
                .filter_map(|n| n.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(describe_action(
        &About::shown(run),
        &look,
        &action,
        NoteChoice::of(args["note"].as_str(), &look.handover),
        args["chat"].as_str(),
        &mentions,
    ))
}

/// Fixed texts of what became of an action; an operation's own error goes to the caller only.
const NOT_ACTED: &str = "The operation refused it before acting; it is still proposed.";
const REFUSED: &str =
    "The operation refused it after the run had changed; read the run before acting again.";
const UNCERTAIN: &str = "Its result is unknown: check Teams (or the run) before doing it again.";
const CUT: &str =
    "The host stopped while it was carried out; check what it did before doing it again.";

/// Move one action of a look from one of `from` to `to`, under the run's lock: the compare and
/// the write are one step, so two confirmations at once never both carry it out. Refused, with
/// the state it is in, when it is not in `from`.
async fn transition(
    host: &Arc<Host>,
    id: &str,
    look: &str,
    action: &str,
    from: &[&str],
    to: &str,
    outcome: Option<&str>,
) -> Result<(), String> {
    let state = &host.state;
    let lock = state.workflows.lock(id);
    let _guard = lock.lock().await;
    let mut run = found(state, id)?;
    let entry = run
        .supervision
        .iter_mut()
        .find(|l| l.id == look)
        .and_then(|l| l.actions.iter_mut().find(|a| a.id == action))
        .ok_or_else(|| invalid(format!("Look {look} proposed no action {action}.")))?;
    if !from.contains(&entry.state.as_str()) {
        return Err(conflict(format!(
            "Action {action} of look {look} is already {}.",
            entry.state
        )));
    }
    entry.state = to.into();
    entry.by = (to != "proposed").then(|| "person".into());
    entry.at = Some(now());
    entry.outcome = outcome.map(str::to_owned);
    state.workflows.db.save(&run).map_err(fail)
}

/// An action carried out now in this process: the sweep after a restart leaves it alone.
struct Acting(Arc<Host>, String);
impl Drop for Acting {
    fn drop(&mut self) {
        self.0
            .state
            .workflows
            .supervision
            .acting
            .lock()
            .unwrap()
            .remove(&self.1);
    }
}

/// After a start, an action left `attempting` by a host that stopped is `uncertain`: it may
/// have happened, and it is never carried out again.
pub(in crate::app) fn sweep(host: &Arc<Host>, now: i64) -> Result<()> {
    let state = &host.state;
    let acting = state.workflows.supervision.acting.lock().unwrap().clone();
    for listed in state.workflows.db.list(&[], None, -1)? {
        let stale = |r: &Run| {
            r.supervision.iter().any(|l| {
                l.actions.iter().any(|a| {
                    a.state == "attempting"
                        && !acting.contains(&format!("{}/{}/{}", r.id, l.id, a.id))
                })
            })
        };
        if !stale(&listed) {
            continue;
        }
        let lock = state.workflows.lock(&listed.id);
        let Ok(_guard) = lock.try_lock() else {
            continue;
        };
        let Some(mut run) = state.workflows.db.get(&listed.id)? else {
            continue;
        };
        let id = run.id.clone();
        for look in &mut run.supervision {
            for action in &mut look.actions {
                if action.state == "attempting"
                    && !acting.contains(&format!("{id}/{}/{}", look.id, action.id))
                {
                    action.state = "uncertain".into();
                    action.at = Some(now);
                    action.outcome = Some(CUT.into());
                }
            }
        }
        state.workflows.db.save(&run)?;
    }
    Ok(())
}

/// The sends recorded for a summary.
fn sends_of(host: &Arc<Host>, id: &str, summary: &str) -> Vec<Value> {
    found(&host.state, id)
        .map(|r| {
            r.summary_sends
                .into_iter()
                .filter(|s| s["summary"] == summary)
                .collect()
        })
        .unwrap_or_default()
}

/// `workflow_supervision_act`: run one proposed action through the operation that does it, as
/// the person confirmed it. Nothing is written before the confirmation; the action moves from
/// `proposed` to `attempting` in one step under the run's lock, so it runs once.
async fn act(host: &Arc<Host>, args: ActArgs, azure: &Azure) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let (run, look, action) = {
        let run = found(state, &args.run)?;
        let look = run
            .supervision
            .iter()
            .find(|l| l.id == args.look)
            .cloned()
            .ok_or_else(|| {
                invalid(format!(
                    "Workflow run {} has no look {}.",
                    run.id, args.look
                ))
            })?;
        let action = look
            .actions
            .iter()
            .find(|a| a.id == args.action || a.action == args.action)
            .cloned()
            .ok_or_else(|| {
                invalid(format!(
                    "Look {} proposed no action {}.",
                    look.id, args.action
                ))
            })?;
        (run, look, action)
    };
    if action.state != "proposed" {
        return Err(conflict(format!(
            "Action {} of look {} is already {}.",
            action.id, look.id, action.state
        )));
    }
    // Everything that can be refused before acting is refused here, and leaves it proposed.
    if args.note.is_some() && action.action != "restart" {
        return Err(invalid("Only a restart takes a note."));
    }
    if let Some(note) = &args.note {
        super::runs::handover_note(Some(note))?;
    }
    let note = NoteChoice::of(args.note.as_deref(), &look.handover);
    if action.action != "post_summary" && (args.chat.is_some() || !args.mentions.is_empty()) {
        return Err(invalid(
            "Only posting the summary takes a chat and mentions.",
        ));
    }
    if action.action == "post_summary" && args.chat.is_none() {
        return Err(invalid(
            "Choose the chat to post the summary to (teams_chats).",
        ));
    }
    if !args.confirmed {
        return Err(invalid(format!(
            "Confirm with the user, then act with confirmed: true. {}",
            describe_action(
                &About::of(&run),
                &look,
                &action,
                note,
                args.chat.as_deref(),
                &args.mentions
            )
        )));
    }
    let pauses = state.workflows.db.pauses().map_err(fail)?;
    if marker(&run, &pauses) != look.marker {
        let _ = transition(
            host,
            &run.id,
            &look.id,
            &action.id,
            &["proposed"],
            "expired",
            None,
        )
        .await;
        return Err(conflict(format!(
            "Workflow run {} moved on since look {}: its proposal expired. Ask for a new look (workflow_supervise).",
            run.id, look.id
        )));
    }
    if action.action == "create_work_item" {
        transition(
            host,
            &run.id,
            &look.id,
            &action.id,
            &["proposed"],
            "opened",
            None,
        )
        .await?;
        let draft = action
            .work_item
            .and_then(|i| look.work_items.get(i))
            .cloned()
            .unwrap_or_default();
        return Ok(json!({
            "run": run.id, "look": look.id, "action": action.id, "state": "opened",
            "create_request": {
                "provider": "azure", "organization": run.item.organization, "project": run.item.project,
                "type": draft.kind, "title": draft.title, "description": draft.description,
                "repository": draft.repository.map(|r| r.name),
            },
        }));
    }
    // Recorded before it acts, in one step with the check: an interruption never reads as an
    // action that did not happen, and a second confirmation finds it attempting.
    transition(
        host,
        &run.id,
        &look.id,
        &action.id,
        &["proposed"],
        "attempting",
        None,
    )
    .await?;
    let key = format!("{}/{}/{}", run.id, look.id, action.id);
    state
        .workflows
        .supervision
        .acting
        .lock()
        .unwrap()
        .insert(key.clone());
    let _acting = Acting(host.clone(), key);
    let summary = format!("supervisor-{}", look.id);
    let sent_before = sends_of(host, &run.id, &summary).len();
    let result = match action.action.as_str() {
        "restart" => match note {
            NoteChoice::Draft => {
                super::runs::restart_drafted(host, &run.id, &look.handover, azure).await
            }
            NoteChoice::Edited(text) => {
                super::runs::operation(
                    host,
                    "workflow_run_restart",
                    &json!({"run": run.id, "note": text, "confirmed": true}),
                    azure,
                )
                .await
            }
            NoteChoice::Empty => {
                super::runs::operation(
                    host,
                    "workflow_run_restart",
                    &json!({"run": run.id, "confirmed": true}),
                    azure,
                )
                .await
            }
        },
        "resume" => {
            super::runs::operation(
                host,
                "workflow_run_resume",
                &json!({"run": run.id, "confirmed": true}),
                azure,
            )
            .await
        }
        "cancel" => {
            super::runs::operation(
                host,
                "workflow_run_cancel",
                &json!({"run": run.id, "confirmed": true}),
                azure,
            )
            .await
        }
        "pause_project" => super::runs::operation(
            host,
            "workflow_runs_pause",
            &json!({"project": run.item.project, "sessions": action.sessions, "confirmed": true}),
            azure,
        )
        .await,
        "post_summary" => {
            post_summary(
                host,
                &run.id,
                &look,
                args.chat.as_deref().unwrap_or_default(),
                &args.mentions,
            )
            .await
        }
        _ => Err(invalid("This action is not one a person confirms.")),
    };
    let attempting = ["attempting"];
    match result {
        Ok(reply) => {
            let uncertain =
                reply["outcome"] == "uncertain" || reply["send"]["outcome"] == "uncertain";
            let word = if uncertain { "uncertain" } else { "executed" };
            transition(
                host,
                &run.id,
                &look.id,
                &action.id,
                &attempting,
                word,
                uncertain.then_some(UNCERTAIN),
            )
            .await?;
            Ok(
                json!({"run": run.id, "look": look.id, "action": action.id, "state": word, "result": reply}),
            )
        }
        Err(error) => {
            // What happened decides, never the error's words: a post that left a send record
            // was refused by Teams or is uncertain; any other action whose run is as it was
            // refused before acting and is proposed again.
            let (word, outcome) = if action.action == "post_summary" {
                match sends_of(host, &run.id, &summary).get(sent_before) {
                    Some(send) if send["outcome"] == "refused" => ("refused", REFUSED),
                    Some(_) => ("uncertain", UNCERTAIN),
                    None => ("proposed", NOT_ACTED),
                }
            } else {
                let now_marker = found(state, &run.id)
                    .ok()
                    .map(|r| marker(&r, &state.workflows.db.pauses().unwrap_or_default()));
                if now_marker.as_deref() == Some(look.marker.as_str()) {
                    ("proposed", NOT_ACTED)
                } else {
                    ("refused", REFUSED)
                }
            };
            transition(
                host,
                &run.id,
                &look.id,
                &action.id,
                &attempting,
                word,
                Some(outcome),
            )
            .await?;
            Err(error)
        }
    }
}

/// Store the look's drafted summary on the run, marked as the supervisor's (a notification
/// block's `{summary}` never reads it), and post it with `workflow_summary_send`.
async fn post_summary(
    host: &Arc<Host>,
    id: &str,
    look: &Look,
    chat: &str,
    mentions: &[String],
) -> Result<Value, String> {
    let state = &host.state;
    let summary = format!("supervisor-{}", look.id);
    {
        let lock = state.workflows.lock(id);
        let _guard = lock.lock().await;
        let mut run = found(state, id)?;
        if !run.summaries.iter().any(|s| s["id"] == summary.as_str()) {
            run.summaries.push(json!({
                "id": summary, "text": look.summary, "created_at": now(),
                "provider": look.supervised_by.provider, "model": look.supervised_by.model,
                "session_id": look.session_id, "by": "supervisor",
            }));
            while run.summaries.len() > super::summary::MAX_SUMMARIES {
                run.summaries.remove(0);
            }
            state.workflows.db.save(&run).map_err(fail)?;
        }
    }
    super::summary::operation(
        host,
        "workflow_summary_send",
        &json!({"run": id, "summary": summary, "chat": chat, "mentions": mentions, "confirmed": true}),
    )
    .await
}

/// `workflow_supervision_dismiss`: set a proposed action aside.
async fn dismiss(host: &Arc<Host>, args: DismissArgs) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let run = found(state, &args.run)?;
    let action = run
        .supervision
        .iter()
        .find(|l| l.id == args.look)
        .and_then(|l| {
            l.actions
                .iter()
                .find(|a| a.id == args.action || a.action == args.action)
        })
        .cloned()
        .ok_or_else(|| {
            invalid(format!(
                "Look {} proposed no action {}.",
                args.look, args.action
            ))
        })?;
    transition(
        host,
        &run.id,
        &args.look,
        &action.id,
        &["proposed"],
        "dismissed",
        None,
    )
    .await?;
    Ok(json!({"run": run.id, "look": args.look, "action": action.id, "state": "dismissed"}))
}

/// `workflow_run_requeue {run, ahead}`: move a run that waits in the line ahead, or put it
/// back in its place by creation.
async fn requeue(host: &Arc<Host>, args: RequeueArgs) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let lock = state.workflows.lock(&args.run);
    let _guard = lock.lock().await;
    let mut run = found(state, &args.run)?;
    if !waits_in_line(&run) {
        return Err(conflict(format!(
            "Workflow run {} does not wait for a slot ({}).",
            run.id,
            run.state.word()
        )));
    }
    if args.ahead {
        run.ahead_since.get_or_insert(now());
    } else {
        run.ahead_since = None;
    }
    state.workflows.db.save(&run).map_err(fail)?;
    Ok(json!({"run": run.id, "ahead": run.ahead_since.is_some(), "ahead_since": run.ahead_since}))
}

#[cfg(test)]
mod tests;
