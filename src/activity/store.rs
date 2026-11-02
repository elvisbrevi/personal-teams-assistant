//! A separate database: opening it never recovers or changes the Teams worker's jobs. The
//! user's record in it (snapshots, summaries, finished runs, legacy decisions) is read and
//! written through `store::activity`; the scheduler's running runs and recovery stay here.
use super::{Entry, activity_key};
use crate::store::activity::{self, ActivityStore, LocalActivity};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
};

pub struct Database {
    db: Mutex<Connection>,
}
impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY,slot TEXT UNIQUE,day TEXT NOT NULL,status TEXT NOT NULL,summary TEXT NOT NULL DEFAULT '{}',created_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS entries(id TEXT PRIMARY KEY,day TEXT NOT NULL,state TEXT NOT NULL,payload TEXT NOT NULL,updated_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS entries_day ON entries(day);
            CREATE TABLE IF NOT EXISTS claims(activity TEXT PRIMARY KEY,entry TEXT NOT NULL);")?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS totals(day TEXT PRIMARY KEY,summary TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS task_days(day TEXT PRIMARY KEY,payload TEXT NOT NULL);",
        )?;
        let has_diagnostics = db
            .prepare("PRAGMA table_info(runs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|name| name == "diagnostics");
        if !has_diagnostics {
            db.execute_batch(
                "ALTER TABLE runs ADD COLUMN diagnostics TEXT NOT NULL DEFAULT '{}';",
            )?;
        }
        Ok(Self { db: Mutex::new(db) })
    }
    /// Called once by the owning host at startup. A write in flight is never retried.
    pub fn recover(&self) -> Result<()> {
        let entries = self.in_states("state IN ('sending','refining')")?;
        for mut entry in entries {
            let previous = entry.state.clone();
            if previous == "sending" {
                entry.state = "uncertain".into();
                entry.issue="Restart during task creation. Inspect Azure DevOps; this write will not be retried.".into();
            } else if previous == "refining" {
                entry.state = "pending".into();
                entry.issue = "Restart during clarification. Add context again if needed.".into();
            } else {
                continue;
            }
            self.update(&entry, &previous)?;
        }
        self.db.lock().unwrap().execute(
            "UPDATE runs SET status='interrupted', diagnostics=json_set(diagnostics,'$.error',?1,'$.finished_at',?2) WHERE status='running'",
            params!["The host stopped before this run finished. Previously saved daily task records were retained.", Utc::now().timestamp()],
        )?;
        Ok(())
    }
    pub fn claim_run(&self, slot: Option<&str>, day: &str) -> Result<Option<String>> {
        let id = uuid::Uuid::new_v4().to_string();
        let changed=self.db.lock().unwrap().execute("INSERT OR IGNORE INTO runs(id,slot,day,status,created_at) VALUES(?1,?2,?3,'running',?4)",params![id,slot,day,Utc::now().timestamp()])?;
        Ok((changed == 1).then_some(id))
    }
    pub fn describe_run(&self, id: &str, diagnostics: &Value) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE runs SET diagnostics=?2 WHERE id=?1 AND status='running'",
            params![id, serde_json::to_string(diagnostics)?],
        )?;
        Ok(())
    }
    pub fn stage_run(&self, id: &str, stage: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE runs SET diagnostics=json_set(diagnostics,'$.stage',?2) WHERE id=?1 AND status='running'",
            params![id, stage],
        )?;
        Ok(())
    }
    pub fn claimed(&self, key: &str) -> Result<bool> {
        Ok(self.db.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM claims WHERE activity=?1)",
            [key],
            |r| r.get(0),
        )?)
    }
    pub fn insert(&self, entry: &Entry) -> Result<bool> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        for a in &entry.activities {
            if tx.execute(
                "INSERT OR IGNORE INTO claims(activity,entry) VALUES(?1,?2)",
                params![activity_key(a), entry.id],
            )? == 0
            {
                return Ok(false);
            }
        }
        tx.execute(
            "INSERT INTO entries(id,day,state,payload,updated_at) VALUES(?1,?2,?3,?4,?5)",
            params![
                entry.id,
                entry.day,
                entry.state,
                serde_json::to_string(entry)?,
                Utc::now().timestamp()
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }
    pub fn update(&self, entry: &Entry, expected: &str) -> Result<()> {
        let mut entry = entry.clone();
        entry.updated_at = Utc::now().timestamp();
        let changed = self.db.lock().unwrap().execute(
            "UPDATE entries SET state=?2,payload=?3,updated_at=?4 WHERE id=?1 AND state=?5",
            params![
                entry.id,
                entry.state,
                serde_json::to_string(&entry)?,
                entry.updated_at,
                expected
            ],
        )?;
        ensure!(
            changed == 1,
            "activity state changed; refresh before resolving"
        );
        Ok(())
    }
    pub fn entry(&self, id: &str) -> Result<Option<Entry>> {
        let text: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT payload FROM entries WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        text.map(|s| serde_json::from_str(&s).context("invalid activity entry"))
            .transpose()
    }
    /// Entries from `since` to `until` (yyyy-mm-dd, inclusive), newest day first.
    pub fn between(&self, since: &str, until: &str) -> Result<Vec<Entry>> {
        activity::between(&self.db.lock().unwrap(), since, until)
    }
    pub fn entries(&self, day: Option<&str>) -> Result<Vec<Entry>> {
        let db = self.db.lock().unwrap();
        let mut stmt=db.prepare("SELECT payload FROM entries WHERE (?1 IS NULL OR day=?1) ORDER BY updated_at DESC LIMIT 1000")?;
        stmt.query_map([day], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    /// Entries recorded from `since` to `until` (yyyy-mm-dd, inclusive), newest day first,
    /// read without opening the database for writing: the Teams pipeline reads them as context
    /// while the host's recorder owns the file. A missing database has no entries.
    pub fn recorded(path: &Path, since: &str, until: &str) -> Result<Vec<Entry>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        LocalActivity::new(&Self { db: Mutex::new(db) }).recorded(since, until)
    }
    /// Every run, running or finished, the newest first (at most 100); `day` keeps that day's.
    pub fn runs(&self, day: Option<&str>) -> Result<Vec<Value>> {
        let mut runs = activity::runs(&self.connection(), "status='running'", day)?;
        runs.extend(LocalActivity::new(self).runs(day)?);
        runs.sort_by_key(|r| std::cmp::Reverse(r["created_at"].as_i64()));
        runs.truncate(100);
        Ok(runs)
    }
    /// The connection, for the profile's activity record in the same database (`LocalActivity`).
    pub(crate) fn connection(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap()
    }
    fn in_states(&self, condition: &str) -> Result<Vec<Entry>> {
        // Only fixed code-owned predicates are passed here.
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(&format!(
            "SELECT payload FROM entries WHERE {condition} ORDER BY updated_at DESC"
        ))?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn pending(&self) -> Result<Vec<Entry>> {
        self.in_states("state IN ('pending','refining')")
    }
}
