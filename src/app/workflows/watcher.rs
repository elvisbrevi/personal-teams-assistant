//! The ready-state watcher: a tracked task of the coordinator's control loop, like the run
//! engine and the daily scheduler. Every `watch_interval_minutes` it lists every requirement-,
//! bug- and task-category item assigned to the user in the ready states of each configured
//! project, whatever its type (a plain Task as much as a development bug), with two To-Do reads
//! `list_azure_hus` per state (the default list, which holds the HUs, the bugs and the task
//! types listed with them, and the `task` kind, which holds every other task type), with the
//! same assignment rules, the read credential and the process cache, and starts a workflow run
//! for each item it has not seen in that state yet, through the engine's own start path
//! (`runs::start_confirmed`) with the project's director choosing among its enabled workflows
//! that start by themselves (`auto_start`), even when there is only one, so an item that
//! workflow does not fit waits unassigned. The director is the project's
//! own, else the machine and agent its workflows share, else the account's
//! (`workflows::director::resolve`). Only when no director can classify (none resolves, or its
//! machine cannot run the director) does the one workflow that starts by itself take the item
//! directly; with several and no director the project waits for one and nothing starts. It
//! reads and starts runs, nothing else: the plan-review gate still stands
//! between a run and any child, and it never changes a source's audiences.
//!
//! The seen ledger (`workflow_seen` in `workflows.db`) makes an item in a given ready state
//! start at most one run. Each row holds the ID of the run it asked for, written **before** the
//! run is requested (the engine creates the run under that ID), so the row and the run agree: a
//! row whose run exists is seen; a row without its run (an abort or a restart between the two,
//! or a start that could not read Azure) is not seen once one interval has passed, and is asked
//! for again under the same run ID, which the engine's one-active-run-per-item index also
//! guards. An item that is no longer in a ready state is forgotten, so one that comes back is
//! seen again; nothing is forgotten from a read that was partial or failed.
//!
//! Bounds: at most [`MAX_ITEMS`] items per project per poll over both reads (the To-Do list's
//! own limit, and each read keeps its time budget), at most [`MAX_STARTS`] new runs per poll
//! (the rest are seen at the next one), and the poll reports which projects it covered fully
//! and how many items of each category it saw (`seen_by_category`). It runs only on the
//! account owner's coordinator (never on a computer signed in to one, nor for a shared portal
//! account), needs no running Teams service, and does nothing while no ready state is
//! configured.
use super::engine::{self, Azure};
use super::*;
use crate::workflows::director::{ReadyStates, Settings};
use rusqlite::{Connection, OptionalExtension, params};

/// Items read per project in one poll, over all its ready states (the To-Do list's maximum).
pub(in crate::app) const MAX_ITEMS: usize = 100;
/// New runs one poll starts; items beyond it are not recorded and are taken by the next poll,
/// so a first poll over a long backlog never opens dozens of director sessions at once.
pub(in crate::app) const MAX_STARTS: usize = 25;
/// How often the task looks at whether a poll is due.
const TICK_SECONDS: u64 = 30;
/// What a project with several workflows that start by themselves and no director reports.
const WAITING: &str = "Waiting for a director: several workflows of this project start by themselves, they differ in machine or agent, and neither the project nor the account has a workflow director to choose among them.";

/// The watcher's state on the profile: its seen ledger and its last poll.
pub(in crate::app) struct Watcher {
    ledger: std::sync::Mutex<Connection>,
    last: std::sync::Mutex<Option<Value>>,
    /// One poll at a time (the task's and a test's).
    polling: Mutex<()>,
}

/// What the ledger holds for an item in a state: the run it asked for and when it did.
struct Entry {
    run: String,
    marked_at: i64,
}

impl Watcher {
    pub(in crate::app) fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        security::protect_file(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS workflow_seen (
                item_url TEXT NOT NULL, item_id INTEGER NOT NULL, organization TEXT NOT NULL,
                project TEXT NOT NULL, state TEXT NOT NULL, changed_at TEXT,
                run_id TEXT NOT NULL, marked_at INTEGER NOT NULL, first_seen INTEGER NOT NULL,
                PRIMARY KEY (item_url, state));
            CREATE INDEX IF NOT EXISTS workflow_seen_project ON workflow_seen(organization, project, state);",
        )?;
        Ok(Self {
            ledger: std::sync::Mutex::new(db),
            last: std::sync::Mutex::new(None),
            polling: Mutex::new(()),
        })
    }
    fn entry(&self, url: &str, state: &str) -> Result<Option<Entry>> {
        Ok(self
            .ledger
            .lock()
            .unwrap()
            .query_row(
                "SELECT run_id, marked_at FROM workflow_seen WHERE item_url=?1 AND state=?2",
                params![url, state],
                |r| {
                    Ok(Entry {
                        run: r.get(0)?,
                        marked_at: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }
    /// Record the item in the state with the run about to be requested for it.
    fn mark(
        &self,
        item: &crate::tools::todo::Item,
        organization: &str,
        run: &str,
        now: i64,
    ) -> Result<()> {
        self.ledger.lock().unwrap().execute(
            "INSERT OR IGNORE INTO workflow_seen (item_url,item_id,organization,project,state,changed_at,run_id,marked_at,first_seen) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
            params![
                item.url,
                item.id,
                organization,
                item.project.to_lowercase(),
                item.state,
                item.changed_at,
                run,
                now
            ],
        )?;
        Ok(())
    }
    /// The run is being asked for again: its mark is as new as this attempt.
    fn retry(&self, url: &str, state: &str, now: i64) -> Result<()> {
        self.ledger.lock().unwrap().execute(
            "UPDATE workflow_seen SET marked_at=?3 WHERE item_url=?1 AND state=?2",
            params![url, state, now],
        )?;
        Ok(())
    }
    /// The item's run is another one (a run already active for it).
    fn settle(&self, url: &str, state: &str, run: &str) -> Result<()> {
        self.ledger.lock().unwrap().execute(
            "UPDATE workflow_seen SET run_id=?3 WHERE item_url=?1 AND state=?2",
            params![url, state, run],
        )?;
        Ok(())
    }
    fn forget(&self, url: &str, state: &str) -> Result<()> {
        self.ledger.lock().unwrap().execute(
            "DELETE FROM workflow_seen WHERE item_url=?1 AND state=?2",
            params![url, state],
        )?;
        Ok(())
    }
    /// Forget the items of a project's state that a complete read no longer found in it.
    fn prune(
        &self,
        organization: &str,
        project: &str,
        state: &str,
        present: &[String],
    ) -> Result<usize> {
        let db = self.ledger.lock().unwrap();
        let keep = serde_json::to_string(present)?;
        Ok(db.execute(
            "DELETE FROM workflow_seen WHERE organization=?1 AND project=?2 AND state=?3 AND item_url NOT IN (SELECT value FROM json_each(?4))",
            params![organization, project.to_lowercase(), state, keep],
        )?)
    }
    /// How many items the ledger holds.
    #[cfg(all(test, unix))]
    pub(in crate::app) fn seen(&self) -> usize {
        self.ledger
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM workflow_seen", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0) as usize
    }
    /// The ledger rows of an item: `(state, changed_at, run_id)`.
    #[cfg(all(test, unix))]
    pub(in crate::app) fn rows(&self, url: &str) -> Vec<(String, Option<String>, String)> {
        let db = self.ledger.lock().unwrap();
        let mut query = db
            .prepare("SELECT state, changed_at, run_id FROM workflow_seen WHERE item_url=?1 ORDER BY state")
            .unwrap();
        query
            .query_map([url], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
    /// Make a row look as old as an abort left it (a test of the stale mark).
    #[cfg(all(test, unix))]
    pub(in crate::app) fn age(&self, url: &str, seconds: i64) {
        self.ledger
            .lock()
            .unwrap()
            .execute(
                "UPDATE workflow_seen SET marked_at = marked_at - ?2 WHERE item_url=?1",
                params![url, seconds],
            )
            .unwrap();
    }
    fn record(&self, poll: Value) {
        *self.last.lock().unwrap() = Some(poll);
    }
    fn last(&self) -> Option<Value> {
        self.last.lock().unwrap().clone()
    }
}

/// The enabled workflows of a project that start by themselves (IDs, in the map's order).
fn auto_workflows(
    workflows: &[crate::workflows::Workflow],
    organization: &str,
    project: &str,
) -> Vec<String> {
    crate::workflows::director::candidates(workflows, organization, project)
        .into_iter()
        .filter(|w| w.auto_start)
        .map(|w| w.id.clone())
        .collect()
}

/// The projects with ready states whose several workflows that start by themselves have no
/// director to choose among them: nothing starts for them.
fn waiting_for_director(
    settings: &Settings,
    workflows: &[crate::workflows::Workflow],
) -> Vec<String> {
    settings
        .ready_states
        .iter()
        .filter(|r| !r.states.is_empty())
        .filter(|r| {
            crate::workflows::organization_name(&r.organization).is_some_and(|o| {
                auto_workflows(workflows, &o, &r.project).len() > 1
                    && crate::workflows::director::resolve(settings, workflows, &o, &r.project)
                        .is_none()
            })
        })
        .map(|r| r.project.clone())
        .collect()
}

/// The watcher as `workflow_settings` shows it: whether it watches on this computer, the
/// interval, and the last poll since the host started.
pub(in crate::app) fn view(state: &DesktopState) -> Value {
    let map = state.settings.map().ok();
    let settings = map
        .as_ref()
        .map(|m| m.workflow_settings.clone())
        .unwrap_or_default();
    let waiting = map
        .as_ref()
        .map(|m| waiting_for_director(&settings, &m.workflows))
        .unwrap_or_default();
    let last = state.workflows.watcher.last();
    let (word, reason) = if !engine::may_run(state) {
        (
            "unavailable",
            Some(
                "Only the account owner's coordinator watches for ready items; a computer signed in to it and a shared portal account never do.",
            ),
        )
    } else if !settings.ready_states.iter().any(|r| !r.states.is_empty()) {
        (
            "idle",
            Some("No ready state is configured; choose one for a project in Settings › Workflows."),
        )
    } else if !waiting.is_empty() {
        ("waiting_for_director", Some(WAITING))
    } else {
        ("watching", None)
    };
    json!({
        "state": word,
        "reason": reason,
        "waiting_projects": waiting,
        "interval_minutes": settings.watch_interval_minutes,
        "last_poll": last,
        "next_poll_at": last
            .as_ref()
            .and_then(|l| l["at"].as_i64())
            .filter(|_| word == "watching" || word == "waiting_for_director")
            .map(|at| at + i64::from(settings.watch_interval_minutes) * 60),
    })
}

impl Azure {
    /// The items assigned to the user in one state of a project, at most `limit`: the To-Do
    /// read of that Azure source, with the read credential and the process cache. Without a
    /// kind it lists the HUs, the bugs and the task types listed with them; the `task` kind
    /// lists every other task-category type (a plain Task).
    async fn ready_items(
        &self,
        state: &DesktopState,
        source: Option<&str>,
        project: &str,
        ready: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<crate::tools::todo::List, String> {
        match self {
            Self::Live => {
                let mut body =
                    json!({"source":source,"project":project,"state":ready,"limit":limit});
                if let Some(kind) = kind {
                    body["kind"] = json!(kind);
                }
                let value = tools::todo::read(state, "list_azure_hus", &body).await?;
                serde_json::from_value(value).map_err(fail)
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => catalog.ready_items(source, project, ready, kind, limit),
        }
    }
}

/// The process categories the watcher takes, as each item's `category` names them.
const CATEGORIES: [&str; 3] = ["requirement", "bug", "task"];
/// The two To-Do reads that together list every type of those categories: the default list
/// (HUs, bugs and the task types listed with them) and the `task` kind (every other one).
const READS: [Option<&str>; 2] = [None, Some("task")];

/// How many items of each category a poll saw.
fn by_category(counts: &[usize; 3]) -> Value {
    CATEGORIES
        .iter()
        .zip(counts)
        .map(|(category, n)| ((*category).to_owned(), json!(n)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// What one project's poll found.
#[derive(Default)]
struct Coverage {
    seen: usize,
    /// `seen` by category, in the order of [`CATEGORIES`].
    categories: [usize; 3],
    started: usize,
    already: usize,
    deferred: usize,
    complete: bool,
    errors: Vec<String>,
}

/// One poll: every configured project's ready states, a run started for each new item.
pub(in crate::app) async fn poll(host: &Arc<Host>, azure: &Azure, now: i64) -> Value {
    let state = &host.state;
    let watcher = &state.workflows.watcher;
    let _polling = watcher.polling.lock().await;
    let Ok(map) = state.settings.map() else {
        let unread = json!({"at": now, "started": 0, "partial": true, "projects": []});
        watcher.record(unread.clone());
        return unread;
    };
    let settings = map.workflow_settings.clone();
    let mut projects = Vec::new();
    let mut budget = MAX_STARTS;
    for ready in settings
        .ready_states
        .iter()
        .filter(|r| !r.states.is_empty())
    {
        projects.push(project(host, azure, &map, ready, &mut budget, now).await);
    }
    let partial = projects.iter().any(|p| p["complete"] != true);
    let mut categories = [0; 3];
    for (i, category) in CATEGORIES.iter().enumerate() {
        categories[i] = projects
            .iter()
            .map(|p| p["seen_by_category"][category].as_u64().unwrap_or(0) as usize)
            .sum();
    }
    let poll = json!({
        "at": now,
        "started": projects.iter().map(|p| p["started"].as_u64().unwrap_or(0)).sum::<u64>(),
        "seen_by_category": by_category(&categories),
        "partial": partial,
        "projects": projects,
    });
    watcher.record(poll.clone());
    poll
}

async fn project(
    host: &Arc<Host>,
    azure: &Azure,
    map: &KnowledgeMap,
    ready: &ReadyStates,
    budget: &mut usize,
    now: i64,
) -> Value {
    let state = &host.state;
    let watcher = &state.workflows.watcher;
    let interval = i64::from(map.workflow_settings.watch_interval_minutes) * 60;
    let named = |extra: Value| {
        let mut base = json!({"organization": ready.organization, "project": ready.project, "states": ready.states});
        for (key, value) in extra.as_object().cloned().unwrap_or_default() {
            base[key] = value;
        }
        base
    };
    let Some(organization) = crate::workflows::organization_name(&ready.organization) else {
        return named(
            json!({"complete": false, "skipped": "The organization of the ready states is not an Azure DevOps address."}),
        );
    };
    // Only the workflows that start by themselves are asked about; with none, nothing is read.
    let candidates = auto_workflows(&map.workflows, &organization, &ready.project);
    // The director classifies whenever one can: it may find that even the only workflow does
    // not fit the item.
    let director = crate::workflows::director::resolve(
        &map.workflow_settings,
        &map.workflows,
        &organization,
        &ready.project,
    );
    let usable = match &director {
        Some(resolved) => implementation::machine_facts(host, &resolved.director.machine)
            .await
            .ok()
            .flatten()
            .is_some_and(|m| m.own || m.direct),
        None => false,
    };
    let direct = match candidates.as_slice() {
        [] => {
            return named(
                json!({"complete": true, "skipped": "No enabled workflow of this project starts by itself (auto_start)."}),
            );
        }
        _ if director.is_some() && (usable || candidates.len() > 1) => None,
        // No director can classify: the one workflow that starts by itself takes the item.
        [only] => Some(only.clone()),
        _ => {
            return named(
                json!({"complete": true, "skipped": WAITING, "waiting_for_director": true}),
            );
        }
    };
    // The enabled source that holds the organization, when several are enabled.
    let source = match tools::todo::source_for(state, &ready.organization) {
        Ok(source) => source,
        Err(error) => {
            let note = error.split_once("] ").map_or(error.as_str(), |(_, t)| t);
            return named(json!({"complete": false, "errors": [note]}));
        }
    };
    let mut found = Coverage {
        complete: true,
        ..Default::default()
    };
    let mut items_left = MAX_ITEMS;
    for ready_state in &ready.states {
        if items_left == 0 {
            // The project's bound is spent: the states left are read at the next poll.
            found.complete = false;
            break;
        }
        // Both reads of the state, within the project's bound; items of this organization only
        // (a project name can exist in several).
        let mut items: Vec<crate::tools::todo::Item> = Vec::new();
        let mut sure = true;
        for kind in READS {
            let limit = items_left.saturating_sub(items.len());
            if limit == 0 {
                // The bound is spent: the rest of the state is read at the next poll.
                sure = false;
                break;
            }
            let list = match azure
                .ready_items(
                    state,
                    source.as_deref(),
                    &ready.project,
                    ready_state,
                    kind,
                    limit,
                )
                .await
            {
                Ok(list) => list,
                Err(error) => {
                    // Reported and kept as it is: nothing of this state is forgotten, what was
                    // read is still taken, and the other states are still read.
                    let note = error.split_once("] ").map_or(error.as_str(), |(_, t)| t);
                    found.errors.push(format!("{ready_state}: {note}"));
                    sure = false;
                    break;
                }
            };
            if list.partial || list.has_more {
                sure = false;
                found.errors.extend(list.warnings.iter().cloned());
            }
            for item in list.items {
                if crate::workflows::director::item_organization(&item.url).as_deref()
                    == Some(organization.as_str())
                    && item.state == *ready_state
                    && CATEGORIES.contains(&item.category.as_str())
                    && !items.iter().any(|i| i.url == item.url)
                {
                    items.push(item);
                }
            }
        }
        // Newest first over both reads, as one list orders them.
        items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
                .then_with(|| a.url.cmp(&b.url))
        });
        items_left = items_left.saturating_sub(items.len());
        found.seen += items.len();
        for item in &items {
            if let Some(i) = CATEGORIES.iter().position(|c| *c == item.category) {
                found.categories[i] += 1;
            }
        }
        if !sure {
            found.complete = false;
        }
        // Every listed item is present, whether or not this poll may start it.
        let present: Vec<String> = items.iter().map(|i| i.url.clone()).collect();
        for item in &items {
            let run = match watcher.entry(&item.url, ready_state) {
                // Its run exists: seen. A mark without its run is waited for one interval (it
                // may be the start in flight), then asked for again under the same run ID.
                Ok(Some(entry)) => {
                    let exists = state.workflows.db.get(&entry.run).ok().flatten().is_some();
                    if exists || now - entry.marked_at < interval {
                        found.already += 1;
                        continue;
                    }
                    if *budget == 0 {
                        found.deferred += 1;
                        found.complete = false;
                        continue;
                    }
                    if watcher.retry(&item.url, ready_state, now).is_err() {
                        found.complete = false;
                        continue;
                    }
                    entry.run
                }
                Ok(None) => {
                    if *budget == 0 {
                        // Not recorded: the next poll sees it as new.
                        found.deferred += 1;
                        found.complete = false;
                        continue;
                    }
                    let run = uuid::Uuid::new_v4().to_string();
                    if watcher.mark(item, &organization, &run, now).is_err() {
                        found
                            .errors
                            .push(format!("{}: the seen ledger could not be written", item.id));
                        found.complete = false;
                        continue;
                    }
                    run
                }
                Err(_) => {
                    found
                        .errors
                        .push(format!("{}: the seen ledger could not be read", item.id));
                    found.complete = false;
                    continue;
                }
            };
            match super::runs::start_confirmed(
                host,
                azure,
                (item.id, Some(item.project.clone()), source.clone()),
                direct.clone(),
                true,
                Some(run),
            )
            .await
            {
                Ok(_) => {
                    *budget -= 1;
                    found.started += 1;
                }
                // Azure could not be reached or the engine is not ready: not seen yet.
                Err(error)
                    if error.starts_with("[network]") || error.starts_with("[not_ready]") =>
                {
                    let _ = watcher.forget(&item.url, ready_state);
                    let note = error.split_once("] ").map_or(error.as_str(), |(_, t)| t);
                    found.errors.push(format!("{}: {note}", item.id));
                    found.complete = false;
                }
                // The item already has an active run (started by hand, say): that is its run.
                // Any other refusal leaves the mark, asked again after one interval.
                Err(_) => {
                    if let Ok(Some(active)) = state.workflows.db.active_for(&item.url) {
                        let _ = watcher.settle(&item.url, ready_state, &active.id);
                    }
                }
            }
        }
        if sure {
            let _ = watcher.prune(&organization, &ready.project, ready_state, &present);
        }
    }
    named(json!({
        "complete": found.complete,
        "seen": found.seen,
        "seen_by_category": by_category(&found.categories),
        "started": found.started,
        "already_seen": found.already,
        "deferred": found.deferred,
        "errors": found.errors.iter().take(10).collect::<Vec<_>>(),
    }))
}

/// Poll when one is due: the coordinator, with a ready state configured, `interval` minutes
/// after the last poll.
pub(in crate::app) async fn tick(host: &Arc<Host>, azure: &Azure, now: i64) {
    let state = &host.state;
    if !engine::may_run(state) {
        return;
    }
    let Ok(map) = state.settings.map() else {
        return;
    };
    let settings = map.workflow_settings;
    if !settings.ready_states.iter().any(|r| !r.states.is_empty()) {
        return;
    }
    let due = state
        .workflows
        .watcher
        .last()
        .and_then(|l| l["at"].as_i64())
        .is_none_or(|at| now - at >= i64::from(settings.watch_interval_minutes) * 60);
    if due {
        poll(host, azure, now).await;
    }
}

pub(in crate::app) async fn schedule(host: Arc<Host>) {
    loop {
        tick(&host, &Azure::Live, chrono::Utc::now().timestamp()).await;
        tokio::time::sleep(std::time::Duration::from_secs(TICK_SECONDS)).await;
    }
}

#[cfg(all(test, unix))]
mod tests;
