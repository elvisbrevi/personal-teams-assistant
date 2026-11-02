//! The activity record: what Activities saved for the user, day by day. Each daily task snapshot
//! and each day's summary is one document identified by its day (its content is its revision);
//! each finished run is one document identified by its `id`; the decisions and claims of older
//! versions are kept as they were and only read. Starting, describing and recovering a run, the
//! slot that keeps a scheduled run from repeating and the legacy entries' recovery are the
//! scheduler's runtime state and stay in the local `activity::store::Database`. Daily runs only
//! read Azure DevOps; a store never writes there.

use crate::{
    activity::{Entry, Summary, store::Database},
    tools::tasks::Day,
};
use anyhow::Result;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub trait ActivityStore: Send + Sync {
    /// Keep the day's task snapshot, replacing the earlier one of that day.
    fn save_day(&self, day: &Day) -> Result<()>;
    /// Snapshots from `since` to `until` (yyyy-mm-dd, inclusive), newest day first; a day
    /// without one shows the tasks older versions confirmed.
    fn days(&self, since: &str, until: &str) -> Result<Vec<Day>>;
    /// Keep the day's summary, replacing the earlier one.
    fn save_summary(&self, day: &str, summary: &Summary) -> Result<()>;
    fn summary(&self, day: &str) -> Result<Option<Summary>>;
    /// What was recorded from `since` to `until` as entries, newest day first: the older
    /// versions' decisions, replaced by the day's snapshot where there is one.
    fn recorded(&self, since: &str, until: &str) -> Result<Vec<Entry>>;
    /// Finish a run the scheduler started: `completed` with its summary, otherwise `failed`
    /// with the error; from then on it is part of the record.
    fn finish_run(&self, id: &str, summary: Option<&Summary>, error: &str) -> Result<()>;
    /// Finished runs, the newest first (at most 100); `day` keeps that day's.
    fn runs(&self, day: Option<&str>) -> Result<Vec<Value>>;
}

/// The profile's `activity-registration.db`, which also holds the scheduler's running runs.
pub struct LocalActivity<'a> {
    db: &'a Database,
}

impl<'a> LocalActivity<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl ActivityStore for LocalActivity<'_> {
    fn save_day(&self, day: &Day) -> Result<()> {
        self.db.connection().execute("INSERT INTO task_days(day,payload) VALUES(?1,?2) ON CONFLICT(day) DO UPDATE SET payload=excluded.payload", params![day.day, serde_json::to_string(day)?])?;
        Ok(())
    }

    fn days(&self, since: &str, until: &str) -> Result<Vec<Day>> {
        let db = self.db.connection();
        let mut days = task_days(&db, since, until)?;
        let snapshot_days: std::collections::BTreeSet<_> =
            days.iter().map(|d| d.day.clone()).collect();
        // Retain the confirmed tasks saved by older versions; new snapshots supersede them.
        for e in between(&db, since, until)?
            .into_iter()
            .filter(|e| e.state == "done")
        {
            if snapshot_days.contains(&e.day) {
                continue;
            }
            let Some(parent) = e.parents.iter().find(|p| Some(p.id) == e.draft.parent) else {
                continue;
            };
            let (Some(id), Some(url)) = (e.task_id, e.task_url) else {
                continue;
            };
            let index = match days.iter().position(|d| d.day == e.day) {
                Some(index) => index,
                None => {
                    days.push(Day {
                        day: e.day.clone(),
                        ..Default::default()
                    });
                    days.len() - 1
                }
            };
            let day = &mut days[index];
            let item = |id, title, url, state, kind| crate::tools::todo::Item {
                id,
                title,
                url,
                state,
                kind,
                category: String::new(),
                project: parent.project.clone(),
                created_at: None,
                dates: vec![],
            };
            day.hours += e.draft.hours.unwrap_or_default();
            day.tasks.push(crate::tools::tasks::Task {
                item: item(id, e.draft.title, url, "".into(), "Task".into()),
                source: String::new(),
                organization: parent.organization.clone(),
                user: String::new(),
                description: e.draft.description,
                hours: e.draft.hours.unwrap_or_default(),
                parent: Some(item(
                    parent.id,
                    parent.title.clone(),
                    parent.url.clone(),
                    parent.state.clone(),
                    parent.kind.clone(),
                )),
            });
        }
        days.sort_by(|a, b| b.day.cmp(&a.day));
        Ok(days)
    }

    fn save_summary(&self, day: &str, summary: &Summary) -> Result<()> {
        self.db.connection().execute("INSERT INTO totals(day,summary) VALUES(?1,?2) ON CONFLICT(day) DO UPDATE SET summary=excluded.summary",params![day,serde_json::to_string(summary)?])?;
        Ok(())
    }

    fn summary(&self, day: &str) -> Result<Option<Summary>> {
        let text: Option<String> = self
            .db
            .connection()
            .query_row("SELECT summary FROM totals WHERE day=?1", [day], |r| {
                r.get(0)
            })
            .optional()?;
        text.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    fn recorded(&self, since: &str, until: &str) -> Result<Vec<Entry>> {
        let db = self.db.connection();
        let mut entries = between(&db, since, until)?;
        // A database read before a version that kept snapshots has no table for them.
        if db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='task_days')",
            [],
            |r| r.get::<_, bool>(0),
        )? {
            for day in task_days(&db, since, until)? {
                entries.retain(|e| e.day != day.day);
                entries.extend(day.entries());
            }
        }
        Ok(entries)
    }

    fn finish_run(&self, id: &str, summary: Option<&Summary>, error: &str) -> Result<()> {
        self.db.connection().execute(
            "UPDATE runs SET status=?2,summary=?3,diagnostics=json_set(diagnostics,'$.error',?4,'$.finished_at',?5) WHERE id=?1",
            params![
                id,
                if summary.is_some() {
                    "completed"
                } else {
                    "failed"
                },
                serde_json::to_string(&summary)?,
                error,
                Utc::now().timestamp(),
            ],
        )?;
        Ok(())
    }

    fn runs(&self, day: Option<&str>) -> Result<Vec<Value>> {
        runs(&self.db.connection(), "status<>'running'", day)
    }
}

/// Runs in the states `condition` names (a fixed, code-owned predicate), newest first.
pub(crate) fn runs(db: &Connection, condition: &str, day: Option<&str>) -> Result<Vec<Value>> {
    let mut stmt = db.prepare(&format!("SELECT id,day,status,summary,created_at,diagnostics FROM runs WHERE {condition} AND (?1 IS NULL OR day=?1) ORDER BY created_at DESC,rowid DESC LIMIT 100"))?;
    let rows = stmt.query_map([day], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    rows.map(|r| {
        let (id, day, status, summary, at, diagnostics) = r?;
        let summary = serde_json::from_str::<Value>(&summary)?;
        // The stored empty default means no result yet, including recovered runs.
        let summary = if summary == json!({}) {
            Value::Null
        } else {
            summary
        };
        Ok(json!({"id":id,"day":day,"status":status,"summary":summary,"created_at":at,"diagnostics":serde_json::from_str::<Value>(&diagnostics)?}))
    }).collect()
}

fn task_days(db: &Connection, since: &str, until: &str) -> Result<Vec<Day>> {
    let mut stmt =
        db.prepare("SELECT payload FROM task_days WHERE day>=?1 AND day<=?2 ORDER BY day DESC")?;
    stmt.query_map([since, until], |r| r.get::<_, String>(0))?
        .map(|r| Ok(serde_json::from_str(&r?)?))
        .collect()
}

pub(crate) fn between(db: &Connection, since: &str, until: &str) -> Result<Vec<Entry>> {
    let mut stmt = db.prepare(
        "SELECT payload FROM entries WHERE day>=?1 AND day<=?2 ORDER BY day DESC, updated_at DESC LIMIT 1000",
    )?;
    stmt.query_map([since, until], |r| r.get::<_, String>(0))?
        .map(|r| Ok(serde_json::from_str(&r?)?))
        .collect()
}

/// What every `ActivityStore` must do, whatever keeps the records. `store` starts empty and is
/// changed by the suite; `start` starts a run of a day the way the scheduler does and returns
/// its ID.
#[cfg(test)]
pub(crate) mod contract {
    use super::*;
    use crate::tools::{tasks::Task, todo::Item};

    pub(crate) fn run(store: &dyn ActivityStore, start: &dyn Fn(&str) -> String) {
        snapshots_are_kept_per_day_newest_first(store);
        summaries_are_kept_per_day(store);
        snapshots_are_recorded_as_entries(store);
        finished_runs_join_the_record(store, start);
    }

    fn task(id: u64, title: &str) -> Task {
        Task {
            item: Item {
                id,
                title: title.into(),
                url: format!("https://dev.azure.com/contoso/Payments/_workitems/edit/{id}"),
                project: "Payments".into(),
                state: "Done".into(),
                kind: "Task".into(),
                category: "task".into(),
                created_at: None,
                dates: vec![],
            },
            source: "ado".into(),
            organization: "https://dev.azure.com/contoso".into(),
            user: "me@example.test".into(),
            description: format!("Description of {title}"),
            hours: 2.,
            parent: None,
        }
    }

    fn day(day: &str, tasks: Vec<Task>) -> Day {
        Day {
            day: day.into(),
            time_zone: "America/Santiago".into(),
            hours: tasks.iter().map(|t| t.hours).sum(),
            tasks,
            target_hours: 9.,
            saved_at: 1,
            ..Default::default()
        }
    }

    fn text<T: serde::Serialize>(value: &T) -> String {
        serde_json::to_string(value).unwrap()
    }

    fn snapshots_are_kept_per_day_newest_first(store: &dyn ActivityStore) {
        let first = day("2026-10-13", vec![task(1, "Reviewed receipts")]);
        let second = day("2026-10-14", vec![task(2, "Fixed the export")]);
        store.save_day(&first).unwrap();
        store.save_day(&second).unwrap();
        assert_eq!(
            text(&store.days("2026-10-01", "2026-10-31").unwrap()),
            text(&vec![second.clone(), first.clone()])
        );
        assert_eq!(
            text(&store.days("2026-10-14", "2026-10-14").unwrap()),
            text(&vec![second])
        );
        // Saving a day again replaces its snapshot.
        let replaced = day(
            "2026-10-13",
            vec![task(1, "Reviewed receipts"), task(3, "Answered support")],
        );
        store.save_day(&replaced).unwrap();
        assert_eq!(
            text(&store.days("2026-10-13", "2026-10-13").unwrap()),
            text(&vec![replaced])
        );
        assert!(store.days("2026-09-01", "2026-09-30").unwrap().is_empty());
    }

    fn summaries_are_kept_per_day(store: &dyn ActivityStore) {
        assert!(store.summary("2026-10-13").unwrap().is_none());
        let mut summary = Summary {
            recorded_hours: 4.,
            remaining_hours: 5.,
            warnings: vec!["Daily target not reached".into()],
            ..Default::default()
        };
        store.save_summary("2026-10-13", &summary).unwrap();
        assert_eq!(
            text(&store.summary("2026-10-13").unwrap()),
            text(&Some(&summary))
        );
        summary.remaining_hours = 0.;
        summary.warnings.clear();
        store.save_summary("2026-10-13", &summary).unwrap();
        assert_eq!(
            text(&store.summary("2026-10-13").unwrap()),
            text(&Some(&summary))
        );
    }

    fn snapshots_are_recorded_as_entries(store: &dyn ActivityStore) {
        let entries = store.recorded("2026-10-13", "2026-10-14").unwrap();
        let titles: Vec<_> = entries.iter().map(|e| e.draft.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Fixed the export", "Reviewed receipts", "Answered support"]
        );
        assert!(entries.iter().all(|e| e.state == "recorded"));
        store.save_day(&day("2026-10-14", vec![])).unwrap();
        let empty = store.recorded("2026-10-14", "2026-10-14").unwrap();
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].state, "recorded_empty");
        assert!(
            store
                .recorded("2026-09-01", "2026-09-30")
                .unwrap()
                .is_empty()
        );
    }

    fn finished_runs_join_the_record(store: &dyn ActivityStore, start: &dyn Fn(&str) -> String) {
        let completed = start("2026-10-13");
        let failed = start("2026-10-14");
        assert!(
            store.runs(None).unwrap().is_empty(),
            "a running run is not part of the record"
        );
        let summary = Summary {
            recorded_hours: 4.,
            ..Default::default()
        };
        store.finish_run(&completed, Some(&summary), "").unwrap();
        store
            .finish_run(&failed, None, "Tasks could not be read.")
            .unwrap();
        let ids = |day| -> Vec<String> {
            store
                .runs(day)
                .unwrap()
                .iter()
                .map(|r| r["id"].as_str().unwrap().to_owned())
                .collect()
        };
        let mut all = ids(None);
        all.sort();
        let mut expected = vec![completed.clone(), failed.clone()];
        expected.sort();
        assert_eq!(all, expected);
        let done = &store.runs(Some("2026-10-13")).unwrap()[0];
        assert_eq!(done["id"], completed.as_str());
        assert_eq!(done["status"], "completed");
        assert_eq!(done["summary"]["recorded_hours"], 4.);
        assert!(done["diagnostics"]["finished_at"].as_i64().unwrap() > 0);
        let broken = &store.runs(Some("2026-10-14")).unwrap()[0];
        assert_eq!(broken["status"], "failed");
        assert!(broken["summary"].is_null());
        assert_eq!(broken["diagnostics"]["error"], "Tasks could not be read.");
        assert!(ids(Some("2026-10-15")).is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_database_meets_the_contract() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("activity-registration.db")).unwrap();
        contract::run(&LocalActivity::new(&db), &|day| {
            db.claim_run(None, day).unwrap().unwrap()
        });
    }

    #[test]
    fn the_run_listing_joins_running_and_finished_runs() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("activity-registration.db")).unwrap();
        let finished = db.claim_run(Some("slot-1"), "2026-10-13").unwrap().unwrap();
        LocalActivity::new(&db)
            .finish_run(&finished, None, "Tasks could not be read.")
            .unwrap();
        let running = db.claim_run(Some("slot-2"), "2026-10-14").unwrap().unwrap();
        db.stage_run(&running, "Reading existing tasks").unwrap();
        let listed: Vec<_> = db
            .runs(None)
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(listed, [running.clone(), finished.clone()]);
        assert_eq!(db.runs(Some("2026-10-14")).unwrap()[0]["status"], "running");
        // A restart interrupts the running run, which joins the record.
        db.recover().unwrap();
        let record = LocalActivity::new(&db).runs(Some("2026-10-14")).unwrap();
        assert_eq!(record[0]["status"], "interrupted");
    }

    #[test]
    fn a_missing_database_has_nothing_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity-registration.db");
        assert!(
            Database::recorded(&path, "2026-10-01", "2026-10-31")
                .unwrap()
                .is_empty()
        );
        assert!(!path.exists(), "reading never creates the database");
    }
}
