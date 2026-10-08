//! A separate database: opening it never recovers or changes the Teams worker's jobs.
use super::{Entry, Summary, activity_key};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{path::Path, sync::Mutex};

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
            "CREATE TABLE IF NOT EXISTS totals(day TEXT PRIMARY KEY,summary TEXT NOT NULL);",
        )?;
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
            "UPDATE runs SET status='interrupted' WHERE status='running'",
            [],
        )?;
        Ok(())
    }
    pub fn claim_run(&self, slot: Option<&str>, day: &str) -> Result<Option<String>> {
        let id = uuid::Uuid::new_v4().to_string();
        let changed=self.db.lock().unwrap().execute("INSERT OR IGNORE INTO runs(id,slot,day,status,created_at) VALUES(?1,?2,?3,'running',?4)",params![id,slot,day,Utc::now().timestamp()])?;
        Ok((changed == 1).then_some(id))
    }
    pub fn finish_run(&self, id: &str, summary: Option<&Summary>) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE runs SET status=?2,summary=?3 WHERE id=?1",
            params![
                id,
                if summary.is_some() {
                    "completed"
                } else {
                    "failed"
                },
                serde_json::to_string(&summary)?
            ],
        )?;
        Ok(())
    }
    pub fn runs(&self, day: Option<&str>) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut stmt=db.prepare("SELECT id,day,status,summary,created_at FROM runs WHERE (?1 IS NULL OR day=?1) ORDER BY created_at DESC LIMIT 100")?;
        let rows = stmt.query_map([day], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        rows.map(|r|{let (id,day,status,summary,at)=r?;Ok(json!({"id":id,"day":day,"status":status,"summary":serde_json::from_str::<Value>(&summary)?,"created_at":at}))}).collect()
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
    pub fn entries(&self, day: Option<&str>) -> Result<Vec<Entry>> {
        let db = self.db.lock().unwrap();
        let mut stmt=db.prepare("SELECT payload FROM entries WHERE (?1 IS NULL OR day=?1) ORDER BY updated_at DESC LIMIT 1000")?;
        stmt.query_map([day], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
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
    pub fn save_summary(&self, day: &str, summary: &Summary) -> Result<()> {
        self.db.lock().unwrap().execute("INSERT INTO totals(day,summary) VALUES(?1,?2) ON CONFLICT(day) DO UPDATE SET summary=excluded.summary",params![day,serde_json::to_string(summary)?])?;
        Ok(())
    }
    pub fn summary(&self, day: &str) -> Result<Option<Summary>> {
        let text: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT summary FROM totals WHERE day=?1", [day], |r| {
                r.get(0)
            })
            .optional()?;
        text.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }
}
