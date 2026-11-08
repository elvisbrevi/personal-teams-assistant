//! The ready-state watcher: a tracked task of the coordinator's control loop, like the run
//! engine and the daily scheduler. Every `watch_interval_minutes` it lists the HUs, bugs and
//! listed tasks assigned to the user in the ready states of each configured project (the To-Do
//! read `list_azure_hus`, with the same assignment rules, the read credential and the process
//! cache), and starts a workflow run for each item it has not seen in that state yet, through
//! the engine's own start path (`runs::start_confirmed`) with the director choosing among the
//! enabled workflows of the project that start by themselves (`auto_start`). It reads and
//! starts runs, nothing else: the plan-review gate still stands between a run and any child,
//! and it never changes a source's audiences.
//!
//! The seen ledger (`workflow_seen` in `workflows.db`) makes an item in a given ready state
//! start at most one run: an item is recorded before its run is asked for, and forgotten again
//! only when the start could not reach Azure (the next poll tries it again). An item that is
//! no longer in a ready state is forgotten, so one that comes back is seen again; nothing is
//! forgotten from a read that was partial or failed.
//!
//! Bounds: at most [`MAX_ITEMS`] items per project per poll (the To-Do list's own limit), at
//! most [`MAX_STARTS`] new runs per poll (the rest are seen at the next one), and the poll
//! reports which projects it covered fully. It runs only on the account owner's coordinator
//! (never on a computer signed in to one, nor for a shared portal account), needs no running
//! Teams service, and does nothing while no ready state is configured.
use super::engine::{self, Azure};
use super::*;
use crate::workflows::director::{ReadyStates, Settings};
use rusqlite::{Connection, params};

/// Items read per project in one poll, over all its ready states (the To-Do list's maximum).
pub(in crate::app) const MAX_ITEMS: usize = 100;
/// New runs one poll starts; items beyond it are not recorded and are taken by the next poll,
/// so a first poll over a long backlog never opens dozens of director sessions at once.
pub(in crate::app) const MAX_STARTS: usize = 25;
/// How often the task looks at whether a poll is due.
const TICK_SECONDS: u64 = 30;

/// The watcher's state on the profile: its seen ledger and its last poll.
pub(in crate::app) struct Watcher {
    ledger: std::sync::Mutex<Connection>,
    last: std::sync::Mutex<Option<Value>>,
    /// One poll at a time (the task's and a test's).
    polling: Mutex<()>,
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
                first_seen INTEGER NOT NULL, PRIMARY KEY (item_url, state));
            CREATE INDEX IF NOT EXISTS workflow_seen_project ON workflow_seen(organization, project, state);",
        )?;
        Ok(Self {
            ledger: std::sync::Mutex::new(db),
            last: std::sync::Mutex::new(None),
            polling: Mutex::new(()),
        })
    }
    /// Record the item in the state; true when it was not recorded yet.
    fn mark(&self, item: &crate::tools::todo::Item, organization: &str, now: i64) -> Result<bool> {
        let db = self.ledger.lock().unwrap();
        let inserted = db.execute(
            "INSERT OR IGNORE INTO workflow_seen (item_url,item_id,organization,project,state,changed_at,first_seen) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                item.url,
                item.id,
                organization,
                item.project.to_lowercase(),
                item.state,
                item.changed_at,
                now
            ],
        )?;
        Ok(inserted == 1)
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
    #[cfg(test)]
    pub(in crate::app) fn seen(&self) -> usize {
        self.ledger
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM workflow_seen", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0) as usize
    }
    /// The ledger rows of an item: `(state, changed_at)`.
    #[cfg(test)]
    pub(in crate::app) fn rows(&self, url: &str) -> Vec<(String, Option<String>)> {
        let db = self.ledger.lock().unwrap();
        let mut query = db
            .prepare("SELECT state, changed_at FROM workflow_seen WHERE item_url=?1 ORDER BY state")
            .unwrap();
        query
            .query_map([url], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
    fn record(&self, poll: Value) {
        *self.last.lock().unwrap() = Some(poll);
    }
    fn last(&self) -> Option<Value> {
        self.last.lock().unwrap().clone()
    }
}

/// The watcher as `workflow_settings` shows it: whether it watches on this computer, the
/// interval, and the last poll since the host started.
pub(in crate::app) fn view(state: &DesktopState) -> Value {
    let settings = state
        .settings
        .map()
        .map(|m| m.workflow_settings)
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
    } else {
        ("watching", None)
    };
    json!({
        "state": word,
        "reason": reason,
        "interval_minutes": settings.watch_interval_minutes,
        "last_poll": last,
        "next_poll_at": last
            .as_ref()
            .and_then(|l| l["at"].as_i64())
            .filter(|_| word == "watching")
            .map(|at| at + i64::from(settings.watch_interval_minutes) * 60),
    })
}

impl Azure {
    /// The HUs, bugs and listed tasks assigned to the user in one state of a project, at most
    /// `limit`: the To-Do read, with the read credential and the process cache.
    async fn ready_items(
        &self,
        state: &DesktopState,
        project: &str,
        ready: &str,
        limit: usize,
    ) -> Result<crate::tools::todo::List, String> {
        match self {
            Self::Live => {
                let body = json!({"project":project,"state":ready,"limit":limit});
                let value = tools::todo::read(state, "list_azure_hus", &body).await?;
                serde_json::from_value(value).map_err(fail)
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => catalog.ready_items(project, ready, limit),
        }
    }
}

/// What one project's poll found.
#[derive(Default)]
struct Coverage {
    seen: usize,
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
    let settings: Settings = state
        .settings
        .map()
        .map(|m| m.workflow_settings)
        .unwrap_or_default();
    let mut projects = Vec::new();
    let mut budget = MAX_STARTS;
    for ready in settings
        .ready_states
        .iter()
        .filter(|r| !r.states.is_empty())
    {
        projects.push(project(host, azure, ready, &mut budget, now).await);
    }
    let partial = projects.iter().any(|p| p["complete"] != true);
    let poll = json!({
        "at": now,
        "started": projects.iter().map(|p| p["started"].as_u64().unwrap_or(0)).sum::<u64>(),
        "partial": partial,
        "projects": projects,
    });
    watcher.record(poll.clone());
    poll
}

async fn project(
    host: &Arc<Host>,
    azure: &Azure,
    ready: &ReadyStates,
    budget: &mut usize,
    now: i64,
) -> Value {
    let state = &host.state;
    let watcher = &state.workflows.watcher;
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
    let covered = state.settings.map().is_ok_and(|map| {
        crate::workflows::director::candidates(&map.workflows, &organization, &ready.project)
            .iter()
            .any(|w| w.auto_start)
    });
    if !covered {
        return named(
            json!({"complete": true, "skipped": "No enabled workflow of this project starts by itself (auto_start)."}),
        );
    }
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
        let list = match azure
            .ready_items(state, &ready.project, ready_state, items_left)
            .await
        {
            Ok(list) => list,
            Err(error) => {
                let note = error.split_once("] ").map_or(error.as_str(), |(_, t)| t);
                found.errors.push(format!("{ready_state}: {note}"));
                found.complete = false;
                break;
            }
        };
        // Items of this organization only (a project name can exist in several).
        let items: Vec<_> = list
            .items
            .iter()
            .filter(|i| {
                crate::workflows::director::item_organization(&i.url).as_deref()
                    == Some(organization.as_str())
                    && i.state == *ready_state
                    && ["requirement", "bug", "task"].contains(&i.category.as_str())
            })
            .collect();
        items_left = items_left.saturating_sub(items.len());
        found.seen += items.len();
        let sure = !list.partial && !list.has_more;
        if !sure {
            found.complete = false;
            found.errors.extend(list.warnings.iter().cloned());
        }
        let mut present = Vec::new();
        for item in items {
            if *budget == 0 {
                // Not recorded: the next poll sees it as new.
                found.deferred += 1;
                found.complete = false;
                continue;
            }
            present.push(item.url.clone());
            match watcher.mark(item, &organization, now) {
                Ok(true) => {}
                Ok(false) => {
                    found.already += 1;
                    continue;
                }
                Err(_) => {
                    found
                        .errors
                        .push(format!("{}: the seen ledger could not be written", item.id));
                    found.complete = false;
                    continue;
                }
            }
            match super::runs::start_confirmed(
                host,
                azure,
                (item.id, Some(item.project.clone()), None),
                None,
                true,
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
                // The item already has an active run, or is not one a workflow runs: seen.
                Err(_) => {}
            }
        }
        if sure {
            let _ = watcher.prune(&organization, &ready.project, ready_state, &present);
        }
    }
    named(json!({
        "complete": found.complete,
        "seen": found.seen,
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
