//! The implementation record: the finished agent runs on work items and the finished request
//! runs (a work item drafted and created from a request). A run is one document identified by
//! its launch `id`, a request run by its `id`; its payload is its revision. Runs still planning
//! or running, requests still generating or sending, the change feed and session index the
//! coordinator keeps for enrolled machines, a run's remote-session access and the machine files
//! (`registry.json`, `connection.json`, `installation.json`, `requests/`) are runtime state and
//! stay in the local `Database`. A request that was sending when it stopped is `uncertain` and
//! never sent again.

use crate::app::implementation::{Database, Run};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};

/// States of an agent run that has not finished.
pub(crate) const RUNS_IN_FLIGHT: [&str; 2] = ["running", "planning"];
/// States of a request run that has not finished.
pub(crate) const REQUESTS_IN_FLIGHT: [&str; 2] = ["generating", "sending"];

pub(crate) trait ImplementationStore: Send + Sync {
    /// Finish a run that is planning or running with its final state, output and plan; a run
    /// already finished is left as it was.
    fn finish_run(&self, id: &str, state: &str, output: &str, plan: Option<Value>) -> Result<()>;
    /// Finished runs, the most recently requested first (at most 100); `url` keeps a work
    /// item's, `id` one run.
    fn runs(&self, url: Option<&str>, id: Option<&str>) -> Result<Vec<Run>>;
    /// Finish a request run: `change` receives its record and sets its final state (and its
    /// result); a state still in flight is refused and nothing changes.
    fn finish_request(
        &self,
        id: &str,
        change: &mut dyn FnMut(&mut Value) -> Result<()>,
    ) -> Result<()>;
    /// Finished request runs, the most recently requested first (at most 100); `id` keeps one.
    fn requests(&self, id: Option<&str>) -> Result<Vec<Value>>;
}

/// The finished rows of the installation's `implementation/runs.db`, next to the in-flight ones.
pub(crate) struct LocalImplementation<'a> {
    db: &'a Database,
}

impl<'a> LocalImplementation<'a> {
    pub(crate) fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl ImplementationStore for LocalImplementation<'_> {
    fn finish_run(&self, id: &str, state: &str, output: &str, plan: Option<Value>) -> Result<()> {
        let mut run = self.db.get(id)?.context("Execution record unavailable.")?;
        if !RUNS_IN_FLIGHT.contains(&run.state.as_str()) {
            return Ok(());
        }
        run.state = state.into();
        run.output = output.into();
        run.plan = plan;
        let finished = chrono::Utc::now().to_rfc3339();
        let mut db = self.db.connection();
        let tx = db.transaction()?;
        if tx.execute("UPDATE agent_runs SET state=?2,finished_at=?3,payload=?4 WHERE id=?1 AND state IN ('running','planning')",params![id,state,finished,serde_json::to_string(&run)?])?>0 { tx.execute("INSERT INTO agent_run_events(run_id) VALUES (?1)",[id])?; }
        tx.commit()?;
        Ok(())
    }

    fn runs(&self, url: Option<&str>, id: Option<&str>) -> Result<Vec<Run>> {
        self.db
            .runs(url, id, "state NOT IN ('running','planning')", 100)
    }

    fn finish_request(
        &self,
        id: &str,
        change: &mut dyn FnMut(&mut Value) -> Result<()>,
    ) -> Result<()> {
        let mut db = self.db.connection();
        let tx = db.transaction()?;
        let (payload, state): (String, String) = tx.query_row(
            "SELECT payload,state FROM request_runs WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut value: Value = serde_json::from_str(&payload)?;
        value["state"] = json!(state);
        change(&mut value)?;
        ensure!(
            value["state"]
                .as_str()
                .is_some_and(|s| !REQUESTS_IN_FLIGHT.contains(&s)),
            "A request run finishes with a final state."
        );
        tx.execute(
            "UPDATE request_runs SET state=?2,payload=?3 WHERE id=?1",
            params![id, value["state"].as_str(), value.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn requests(&self, id: Option<&str>) -> Result<Vec<Value>> {
        self.db
            .request_rows(id, "state NOT IN ('generating','sending')")
    }
}

/// What every `ImplementationStore` must do, whatever keeps the records. `store` starts empty
/// and is changed by the suite; `start` begins a run (`planning`, a request `generating`) the
/// way the machine does, outside the store.
#[cfg(test)]
pub(crate) mod contract {
    use super::*;

    pub(crate) enum Start<'a> {
        Run(&'a Run),
        Request(&'a Value),
    }

    pub(crate) fn run(store: &dyn ImplementationStore, start: &dyn Fn(Start)) {
        finished_runs_join_the_record_once(store, start);
        finished_requests_join_the_record(store, start);
    }

    pub(crate) fn sample(id: &str, item: u64, created_at: &str) -> Run {
        serde_json::from_value(json!({"launch":{"id":id,"request":{"machine":"22222222-2222-4222-8222-222222222222","item":{"provider":"azure","id":item,"project":"Payments","source":"azure","task":true},"context":"Synthetic task","directories":["/work"],"provider":"codex","model":"test-model","effort":"high"},"detail":{"item":{"id":item,"title":"Synthetic task","url":format!("https://dev.azure.com/contoso/Payments/_workitems/edit/{item}"),"project":"Payments","state":"Active","kind":"Task","created_at":null,"dates":[]},"fields":[],"comments":[],"relations":[],"history":[],"partial":false,"warnings":[]},"language":"en","planning":false},"state":"running","created_at":created_at,"finished_at":null,"output":"","plan":null})).unwrap()
    }

    fn ids(runs: Vec<Run>) -> Vec<String> {
        runs.into_iter().map(|r| r.launch.id).collect()
    }

    fn finished_runs_join_the_record_once(store: &dyn ImplementationStore, start: &dyn Fn(Start)) {
        let older = sample(
            "11111111-1111-4111-8111-111111111111",
            7,
            "2026-10-24T12:00:00Z",
        );
        let newer = sample(
            "33333333-3333-4333-8333-333333333333",
            8,
            "2026-10-25T12:00:00Z",
        );
        start(Start::Run(&older));
        start(Start::Run(&newer));
        assert!(
            store.runs(None, None).unwrap().is_empty(),
            "a running run is not part of the record"
        );
        let plan = json!({"children":[{"title":"Write the export"}]});
        store
            .finish_run(
                &older.launch.id,
                "plan_ready",
                "Planned.",
                Some(plan.clone()),
            )
            .unwrap();
        store
            .finish_run(&newer.launch.id, "done", "Implemented and checked.", None)
            .unwrap();
        assert_eq!(
            ids(store.runs(None, None).unwrap()),
            [newer.launch.id.clone(), older.launch.id.clone()]
        );
        let planned = store
            .runs(None, Some(&older.launch.id))
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(planned.state, "plan_ready");
        assert_eq!(planned.output, "Planned.");
        assert_eq!(planned.plan, Some(plan));
        assert!(planned.finished_at.is_some());
        // Finishing again changes nothing.
        store
            .finish_run(&older.launch.id, "cancelled", "", None)
            .unwrap();
        let kept = store
            .runs(None, Some(&older.launch.id))
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(kept.state, "plan_ready");
        assert_eq!(
            ids(store
                .runs(Some(&newer.launch.detail.item.url), None)
                .unwrap()),
            std::slice::from_ref(&newer.launch.id)
        );
        assert!(store.finish_run("unknown", "done", "", None).is_err());
    }

    fn finished_requests_join_the_record(store: &dyn ImplementationStore, start: &dyn Fn(Start)) {
        let id = "44444444-4444-4444-8444-444444444444";
        let generating = json!({"id":id,"state":"generating","created_at":"2026-10-25T12:00:00Z","session_id":null,"submission":{"context":"Synthetic request"}});
        start(Start::Request(&generating));
        assert!(store.requests(None).unwrap().is_empty());
        assert!(
            store
                .finish_request(id, &mut |r| {
                    r["state"] = json!("sending");
                    Ok(())
                })
                .is_err(),
            "sending is not a final state"
        );
        assert!(store.requests(Some(id)).unwrap().is_empty());
        store
            .finish_request(id, &mut |r| {
                r["state"] = json!("New");
                r["item"] = json!({"id": 9, "url": "https://dev.azure.com/contoso/Payments/_workitems/edit/9"});
                r["finished_at"] = json!("2026-10-25T12:05:00Z");
                Ok(())
            })
            .unwrap();
        let record = store.requests(Some(id)).unwrap().pop().unwrap();
        assert_eq!(record["state"], "New");
        assert_eq!(record["item"]["id"], 9);
        assert_eq!(record["submission"]["context"], "Synthetic request");
        assert_eq!(store.requests(None).unwrap().len(), 1);
        assert!(
            store
                .finish_request("unknown", &mut |r| {
                    r["state"] = json!("failed");
                    Ok(())
                })
                .is_err()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::contract::{Start, sample};
    use super::*;

    #[test]
    fn local_database_meets_the_contract() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::open(&root.path().join("runs.db")).unwrap();
        contract::run(&LocalImplementation::new(&db), &|start| match start {
            Start::Run(run) => db.insert(run).unwrap(),
            Start::Request(record) => db.request_insert(record).unwrap(),
        });
    }

    #[test]
    fn listings_join_in_flight_and_finished_runs() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::open(&root.path().join("runs.db")).unwrap();
        let finished = sample(
            "11111111-1111-4111-8111-111111111111",
            7,
            "2026-10-24T12:00:00Z",
        );
        let running = sample(
            "33333333-3333-4333-8333-333333333333",
            8,
            "2026-10-25T12:00:00Z",
        );
        db.insert(&finished).unwrap();
        db.insert(&running).unwrap();
        LocalImplementation::new(&db)
            .finish_run(&finished.launch.id, "done", "", None)
            .unwrap();
        let all: Vec<_> = db
            .list(None, None, false)
            .unwrap()
            .into_iter()
            .map(|r| (r.launch.id, r.state))
            .collect();
        assert_eq!(
            all,
            [
                (running.launch.id.clone(), "running".to_owned()),
                (finished.launch.id.clone(), "done".to_owned())
            ]
        );
        let active = db.list(None, None, true).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].launch.id, running.launch.id);
        assert_eq!(db.get(&finished.launch.id).unwrap().unwrap().state, "done");
        let generating = json!({"id":"44444444-4444-4444-8444-444444444444","state":"generating","created_at":"2026-10-26T12:00:00Z","session_id":null});
        db.request_insert(&generating).unwrap();
        assert_eq!(db.requests(None).unwrap()[0]["state"], "generating");
    }
}
