//! Workflow runs: the durable record of one execution of a workflow for one work item, kept in
//! `implementation/workflows.db` next to `runs.db`, on the coordinator only.
//!
//! The table `workflow_runs` holds each run under a stable UUID, indexed by its item's verified
//! link, its workflow, its state and when it changed; the payload is the whole record (`Run`):
//! the workflow's definition and blocks as they were when the run got its workflow, the item's
//! identity and link, the stage timeline, the implementation runs it asked for (the planning run
//! and one coding run per child), the creation ledger entry, the children with their branches
//! and pull requests, the block outcomes, the summaries and the attention or error text. A
//! partial unique index keeps one active run (any state but `done` and `cancelled`) per item.
use super::*;
use crate::ado::workflow::PullRequest;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(in crate::app) enum State {
    /// The workflow director is choosing the workflow.
    Classifying,
    /// No workflow yet: the director chose none, or there is no director; the user assigns one.
    Unassigned,
    /// The planning run proposes the children.
    Planning,
    /// The plan waits for the user's review and confirmation (`implementation_create`).
    PlanReview,
    /// The confirmed children are being created (the creation ledger).
    CreatingChildren,
    /// One coding run per child, one after another.
    Coding,
    /// Waiting for every pull request to be completed.
    PullRequests,
    /// The stage blocks.
    Blocks,
    Done,
    /// The engine could not continue (a refused launch, an unreadable item).
    Failed,
    Cancelled,
    /// The host restarted while an implementation run of this run was active.
    Interrupted,
    /// Stopped on something the user must look at (`attention` says what).
    NeedsAttention,
}
impl State {
    pub(in crate::app) const ALL: [State; 13] = [
        State::Classifying,
        State::Unassigned,
        State::Planning,
        State::PlanReview,
        State::CreatingChildren,
        State::Coding,
        State::PullRequests,
        State::Blocks,
        State::Done,
        State::Failed,
        State::Cancelled,
        State::Interrupted,
        State::NeedsAttention,
    ];
    pub(in crate::app) fn word(self) -> &'static str {
        match self {
            State::Classifying => "classifying",
            State::Unassigned => "unassigned",
            State::Planning => "planning",
            State::PlanReview => "plan_review",
            State::CreatingChildren => "creating_children",
            State::Coding => "coding",
            State::PullRequests => "pull_requests",
            State::Blocks => "blocks",
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
            State::Interrupted => "interrupted",
            State::NeedsAttention => "needs_attention",
        }
    }
    pub(in crate::app) fn parse(word: &str) -> Option<State> {
        State::ALL.into_iter().find(|s| s.word() == word)
    }
    /// Done or cancelled: nothing more happens to the run.
    pub(in crate::app) fn finished(self) -> bool {
        matches!(self, State::Done | State::Cancelled)
    }
    /// Stopped until the user resumes it.
    pub(in crate::app) fn stopped(self) -> bool {
        matches!(
            self,
            State::Failed | State::Interrupted | State::NeedsAttention
        )
    }
    /// Waiting on the user: Home lists these as attention items.
    pub(in crate::app) fn waits_on_user(self) -> bool {
        matches!(
            self,
            State::PlanReview
                | State::Unassigned
                | State::NeedsAttention
                | State::Interrupted
                | State::Failed
        )
    }
    /// The stages the engine advances by itself.
    pub(in crate::app) fn driven(self) -> bool {
        matches!(
            self,
            State::Classifying
                | State::Planning
                | State::PlanReview
                | State::CreatingChildren
                | State::Coding
                | State::PullRequests
                | State::Blocks
        )
    }
    /// The stages that take one of the `concurrency` slots: those where a coding or planning
    /// agent works or the run is about to act. Waiting for the user's review or for pull
    /// requests takes none, and neither does classifying (a short read-only director session,
    /// bounded on its own by the engine).
    pub(in crate::app) fn takes_slot(self) -> bool {
        matches!(
            self,
            State::Planning | State::CreatingChildren | State::Coding | State::Blocks
        )
    }
}

/// The work item a run is for, as it was read when the run started.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Item {
    pub provider: String,
    pub id: u64,
    pub project: String,
    /// The organization's URL (`https://dev.azure.com/NAME`).
    pub organization: String,
    /// The verified link.
    pub url: String,
    pub title: String,
    pub kind: String,
    /// `requirement`, `bug` or `task`.
    pub category: String,
    pub state: String,
    /// The Azure source it was read from, when several are enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// One stage of the timeline.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Stage {
    pub stage: State,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// An implementation run the workflow run asked for, by the ID it chose and recorded first.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Implementation {
    pub id: String,
    pub machine: String,
    /// The machine accepted the launch; from then on the run is never started again.
    #[serde(default)]
    pub started: bool,
    /// Its state as the engine last read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// A child the run codes (for a task-category item, the item itself).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Child {
    pub id: u64,
    pub title: String,
    /// Its verified link.
    pub url: String,
    #[serde(default)]
    pub kind: String,
    /// The workflow repository it is coded in, and its checkout on the machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<crate::workflows::Repository>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    /// Its coding run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding: Option<Implementation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    /// The branch its session reported (`Branch: NAME` at the end of its report).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_requests: Vec<PullRequest>,
    /// `done`, or `accepted` when the user resumed the run past a coding run that did not end
    /// done (its record says how it ended) or past a missing pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}
impl Child {
    pub(in crate::app) fn accepted(&self) -> bool {
        self.outcome.as_deref() == Some("accepted")
    }
}

/// What an acting block did at one occurrence of its stage, or why it did not run. The engine
/// records the attempt (`attempting`) before it acts and the result after: an attempt found
/// without a result after a restart is `uncertain` and is never made again.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct BlockOutcome {
    pub block: String,
    pub kind: crate::workflows::Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<crate::workflows::Stage>,
    /// A notification: `sent`, `refused` or `uncertain`. A launcher: `succeeded`, `failed`,
    /// `timed_out`, `not_started` or `uncertain`. `attempting` until the result is recorded.
    pub outcome: String,
    /// When the result was recorded (the attempt's time while `attempting`).
    pub at: i64,
    /// Which occurrence of its stage this was (`done`, `child_done:ID`, ...): a block runs at
    /// most once for each.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub occurrence: String,
    /// Why a notification was refused or a launcher did not start (never a launcher's output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// A launcher's exit code, when the program ran to its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// A launcher's duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// A notification's Teams message ID, once it was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// One workflow run.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Run {
    pub id: String,
    /// The workflow's ID; none while classifying or unassigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// The workflow's definition when the run got it: later edits do not change this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<crate::workflows::Workflow>,
    /// Its blocks, in order, as they were then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<crate::workflows::Block>,
    pub item: Item,
    pub state: State,
    /// The stage it stopped in, while failed, interrupted or needing attention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_in: Option<State>,
    #[serde(default)]
    pub timeline: Vec<Stage>,
    /// What the director concluded, when it was asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning: Option<Implementation>,
    /// The creation ledger entry (the planning run's ID, as `implementation_create` keys it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Child>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_outcomes: Vec<BlockOutcome>,
    /// Progress summaries the director wrote (`summary::Summary`: `id`, `text`, `created_at`,
    /// `provider`, `model`, `session_id`), oldest first, at most `summary::MAX_SUMMARIES`.
    /// A notification block's `{summary}` is rendered from the latest entry (the last).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summaries: Vec<Value>,
    /// One record per send of a summary to a chat (`summary::Send`), oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary_sends: Vec<Value>,
    /// What the user must look at, while the run waits on them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    /// The last thing that went wrong (kept after a resume).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The person resumed past an uncertain or incomplete creation: the children created are
    /// the ones coded.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub creation_accepted: bool,
    /// The person resumed a run that delivered no child: it ends done without pull requests.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub undelivered_accepted: bool,
    /// Why it stopped, when a resume treats that cause on its own (`nothing_delivered`,
    /// `ledger_parent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// While classifying: the director could not be reached; it is asked again from then on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<i64>,
    /// How many times in a row the director could not be reached (the wait doubles).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retries: u32,
    /// The watcher started it: the director chooses among the enabled workflows that start by
    /// themselves (`auto_start`) only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_start: bool,
    /// It holds one of the `concurrency` slots.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub admitted: bool,
    /// When the engine began to act on this run's acting blocks (0.12.8 or later). A run with
    /// none was recorded by an older version: the occurrences of its stages that had already
    /// happened are marked `skipped` when the engine first sees it, and never acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acting_since: Option<i64>,
    /// When the wait for its pull requests began (or began again on a resume).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_since: Option<i64>,
    /// When its pull requests were last read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
}

impl Run {
    pub(in crate::app) fn new(item: Item, now: i64) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            workflow: None,
            definition: None,
            blocks: Vec::new(),
            item,
            state: State::Classifying,
            stopped_in: None,
            timeline: vec![Stage {
                stage: State::Classifying,
                started_at: now,
                finished_at: None,
                outcome: None,
            }],
            classification: None,
            planning: None,
            ledger: None,
            children: Vec::new(),
            block_outcomes: Vec::new(),
            summaries: Vec::new(),
            summary_sends: Vec::new(),
            attention: None,
            error: None,
            creation_accepted: false,
            undelivered_accepted: false,
            cause: None,
            retry_at: None,
            retries: 0,
            auto_start: false,
            admitted: false,
            acting_since: Some(now),
            waiting_since: None,
            checked_at: None,
            created_at: now,
            updated_at: now,
            finished_at: None,
        }
    }
    /// A task-category item has no children: it is coded itself.
    pub(in crate::app) fn task(&self) -> bool {
        self.item.category == "task"
    }
    /// Close the current stage with `outcome`.
    fn close(&mut self, now: i64, outcome: &str) {
        if let Some(stage) = self.timeline.last_mut()
            && stage.finished_at.is_none()
        {
            stage.finished_at = Some(now);
            stage.outcome = Some(outcome.to_owned());
        }
    }
    /// Move to the next stage (or to `done`), closing the current one with `outcome`.
    pub(in crate::app) fn advance(&mut self, next: State, now: i64, outcome: &str) {
        self.close(now, outcome);
        // A slot is kept only from one stage that takes it to the next; any other run waits in
        // the line for one, in its place by creation.
        if !(self.state.takes_slot() && next.takes_slot()) {
            self.admitted = false;
        }
        self.state = next;
        self.stopped_in = None;
        self.attention = None;
        self.cause = None;
        self.retry_at = None;
        self.retries = 0;
        if next.driven() || next == State::Unassigned {
            self.timeline.push(Stage {
                stage: next,
                started_at: now,
                finished_at: None,
                outcome: None,
            });
        }
        if next == State::Done {
            self.timeline.push(Stage {
                stage: State::Done,
                started_at: now,
                finished_at: Some(now),
                outcome: Some("done".into()),
            });
            self.finished_at = Some(now);
        }
        self.updated_at = now;
    }
    /// Stop in `state` (failed, interrupted or needing attention) with what the user must read.
    pub(in crate::app) fn stop(&mut self, state: State, now: i64, reason: String) {
        if self.state.driven() {
            self.stopped_in = Some(self.state);
        }
        self.close(now, state.word());
        self.state = state;
        self.attention = Some(reason.clone());
        self.error = Some(reason);
        self.cause = None;
        self.admitted = false;
        self.updated_at = now;
    }
    /// Stop for good: cancelled.
    pub(in crate::app) fn cancel(&mut self, now: i64) {
        self.close(now, "cancelled");
        self.state = State::Cancelled;
        self.attention = None;
        self.admitted = false;
        self.finished_at = Some(now);
        self.updated_at = now;
    }
    /// The implementation run active now, if any: the planning run while planning, or the
    /// coding run of the child being coded.
    pub(in crate::app) fn active_implementation(&self) -> Option<&Implementation> {
        match self.stopped_in.unwrap_or(self.state) {
            State::Planning => self.planning.as_ref(),
            State::Coding => self
                .children
                .iter()
                .find(|c| c.outcome.is_none())
                .and_then(|c| c.coding.as_ref()),
            _ => None,
        }
    }
    /// It waits in the line for a slot: in a stage that takes one, without one.
    pub(in crate::app) fn waits_for_slot(&self) -> bool {
        self.state.takes_slot() && !self.admitted
    }
    /// The run as the operations show it: the whole record, with the stage it is in.
    pub(in crate::app) fn view(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        value["stage"] = json!(self.stopped_in.unwrap_or(self.state).word());
        value["waits_on_user"] = json!(self.state.waits_on_user());
        // What its coding runs may do beyond the fixed rules, as the definition it got holds it.
        value["allow"] = json!(self.definition.as_ref().map(|d| &d.allow));
        value
    }
    /// The run in a list: its identity, item, state, stage and what it waits for.
    pub(in crate::app) fn summary(&self) -> Value {
        json!({
            "id": self.id,
            "workflow": self.workflow,
            "item": {"id": self.item.id, "title": self.item.title, "url": self.item.url,
                "kind": self.item.kind, "category": self.item.category, "project": self.item.project},
            "state": self.state,
            "stage": self.stopped_in.unwrap_or(self.state).word(),
            "attention": self.attention,
            "waits_on_user": self.state.waits_on_user(),
            "children": self.children.len(),
            "pull_requests": self.children.iter().flat_map(|c| c.pull_requests.iter())
                .map(|p| json!({"url": p.url, "status": p.status})).collect::<Vec<_>>(),
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "finished_at": self.finished_at,
        })
    }
}

/// The line for the `concurrency` slots, from `runs` oldest first (`Store::list` reversed):
/// every run waiting for a slot, in creation order, and in its place a classifying run that is
/// not waiting out a retry, which will need one once classified. A run that gave a slot back
/// keeps its place, so a run created later never passes it.
pub(in crate::app) fn line(runs: &[Run], now: i64) -> Vec<&Run> {
    runs.iter()
        .filter(|r| {
            r.waits_for_slot()
                || (r.state == State::Classifying && r.retry_at.is_none_or(|at| at <= now))
        })
        .collect()
}

/// Whether run `id` takes a slot now: fewer runs are ahead of it in the line than slots are
/// free.
pub(in crate::app) fn may_take_slot(runs: &[Run], id: &str, concurrency: usize, now: i64) -> bool {
    let taken = runs
        .iter()
        .filter(|r| r.admitted && r.state.takes_slot() && r.id != id)
        .count();
    let free = concurrency.saturating_sub(taken);
    line(runs, now)
        .iter()
        .position(|r| r.id == id)
        .is_some_and(|ahead| ahead < free)
}

/// The place of each run waiting for a slot (1 is next), by run ID.
pub(in crate::app) fn positions(runs: &[Run], now: i64) -> HashMap<String, usize> {
    line(runs, now)
        .into_iter()
        .enumerate()
        .filter(|(_, r)| r.waits_for_slot())
        .map(|(at, r)| (r.id.clone(), at + 1))
        .collect()
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

pub(in crate::app) struct Store(std::sync::Mutex<Connection>);

impl Store {
    pub(in crate::app) fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        security::protect_file(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS workflow_runs (
                id TEXT PRIMARY KEY, workflow_id TEXT NOT NULL, item_provider TEXT NOT NULL,
                item_id INTEGER NOT NULL, item_url TEXT NOT NULL, state TEXT NOT NULL,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, payload TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS workflow_runs_item ON workflow_runs(item_url, created_at);
            CREATE INDEX IF NOT EXISTS workflow_runs_workflow ON workflow_runs(workflow_id, created_at);
            CREATE INDEX IF NOT EXISTS workflow_runs_state ON workflow_runs(state, updated_at);
            CREATE UNIQUE INDEX IF NOT EXISTS workflow_runs_active_item ON workflow_runs(item_url)
                WHERE state NOT IN ('done','cancelled');",
        )?;
        Ok(Self(std::sync::Mutex::new(db)))
    }
    /// Record a new run; refused while the item has an active run.
    pub(in crate::app) fn insert(&self, run: &Run) -> Result<bool> {
        let db = self.0.lock().unwrap();
        let inserted = db.execute(
            "INSERT OR IGNORE INTO workflow_runs (id,workflow_id,item_provider,item_id,item_url,state,created_at,updated_at,payload) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                run.id,
                run.workflow.clone().unwrap_or_default(),
                run.item.provider,
                run.item.id,
                run.item.url,
                run.state.word(),
                run.created_at,
                run.updated_at,
                serde_json::to_string(run)?
            ],
        )?;
        Ok(inserted == 1)
    }
    pub(in crate::app) fn save(&self, run: &Run) -> Result<()> {
        let db = self.0.lock().unwrap();
        let changed = db.execute(
            "UPDATE workflow_runs SET workflow_id=?2,state=?3,updated_at=?4,payload=?5 WHERE id=?1",
            params![
                run.id,
                run.workflow.clone().unwrap_or_default(),
                run.state.word(),
                run.updated_at,
                serde_json::to_string(run)?
            ],
        )?;
        ensure!(changed == 1, "Workflow run {} is not recorded.", run.id);
        Ok(())
    }
    /// Lose a run, as a failing disk would: a test of what is reported when saving fails.
    #[cfg(all(test, unix))]
    pub(in crate::app) fn forget(&self, id: &str) {
        let db = self.0.lock().unwrap();
        db.execute("DELETE FROM workflow_runs WHERE id=?1", [id])
            .unwrap();
    }
    pub(in crate::app) fn get(&self, id: &str) -> Result<Option<Run>> {
        let db = self.0.lock().unwrap();
        let payload: Option<String> = db
            .query_row("SELECT payload FROM workflow_runs WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        payload
            .map(|p| serde_json::from_str(&p).map_err(Into::into))
            .transpose()
    }
    /// The active run of an item (any state but done and cancelled).
    pub(in crate::app) fn active_for(&self, url: &str) -> Result<Option<Run>> {
        let db = self.0.lock().unwrap();
        let payload: Option<String> = db
            .query_row(
                "SELECT payload FROM workflow_runs WHERE item_url=?1 AND state NOT IN ('done','cancelled')",
                [url],
                |r| r.get(0),
            )
            .optional()?;
        payload
            .map(|p| serde_json::from_str(&p).map_err(Into::into))
            .transpose()
    }
    /// Runs, the most recently created first: in one of `states` (all when empty), of one
    /// workflow, at most `limit` (-1 for all).
    pub(in crate::app) fn list(
        &self,
        states: &[State],
        workflow: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Run>> {
        let db = self.0.lock().unwrap();
        let words: Vec<&str> = states.iter().map(|s| s.word()).collect();
        let filter = serde_json::to_string(&words)?;
        let mut query = db.prepare(
            "SELECT payload FROM workflow_runs WHERE (?1 = '[]' OR state IN (SELECT value FROM json_each(?1))) AND (?2 IS NULL OR workflow_id=?2) ORDER BY created_at DESC, rowid DESC LIMIT ?3",
        )?;
        let rows = query.query_map(params![filter, workflow, limit], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
}
