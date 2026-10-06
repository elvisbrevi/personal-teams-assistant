use anyhow::Result;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Mutex};

pub struct Store {
    db: Mutex<Connection>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub resource: String,
    pub attempts: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub resource: String,
    pub expires_at: i64,
}
#[derive(Clone, Debug)]
pub struct ConversationContext {
    pub question: String,
    pub answer: String,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Audit {
    pub status: String,
    pub reason: String,
    pub source: Option<String>,
    pub confidences: Vec<f64>,
    pub tools: Vec<String>,
    pub provider: Option<String>,
    pub proposed: Option<String>,
    pub sent: Option<String>,
    pub graph_message_id: Option<String>,
    pub detailed: Option<bool>,
    pub answer_limit: Option<usize>,
    pub used_sources: Vec<String>,
    pub partial: bool,
    pub coverage_warnings: Vec<String>,
    pub references: Vec<crate::evidence::Reference>,
    pub teams_messages: Vec<crate::evidence::TeamsMessage>,
    /// Holding notice for a slow answer: `sending`, `sent` or `uncertain`. Never sent twice.
    pub holding_reply: Option<String>,
    /// Redacted text of an eligible incoming message; never stored for ineligible ones.
    pub question: Option<String>,
    /// The request interpreted with the conversation context, and the topic searched.
    pub resolved_question: Option<String>,
    pub topic: Option<String>,
    /// `direct`, `group` or `self`.
    pub conversation_kind: Option<String>,
    /// When the incoming message was written (Unix milliseconds).
    pub received_at: Option<i64>,
    /// Earlier messages of the conversation given to the model as context.
    pub history_messages: usize,
    /// Providers that failed before the one in `provider` answered, as `label: failure`.
    pub provider_fallbacks: Vec<String>,
    /// What the message asked: `question`, `activity_review` or `greeting`.
    pub intent: Option<String>,
    /// How cited sources were chosen: `llm`, `code` (no model could choose them) or `none`.
    /// Audits written before 0.6.1 may say `jev`.
    pub reference_selection: Option<String>,
    /// Informative final review of audits written before 0.6.1 (`allow 0.91`, `unavailable`).
    /// No longer produced.
    pub final_check: Option<String>,
    /// Notice sent to the personal chat when an answer was withheld: `sending`, `sent` or
    /// `uncertain`. Never sent twice.
    pub withheld_notice: Option<String>,
    /// Links written to Azure DevOps for this message (a confirmed plan in the personal
    /// chat), each recorded as `sending` before its request and never retried.
    pub links: Vec<crate::ado::link::LinkRecord>,
    /// Processing steps in order. Details name decisions and counts, never message content.
    pub trace: Vec<TraceStep>,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TraceStep {
    /// Unix milliseconds.
    pub at: i64,
    pub step: String,
    pub detail: String,
}
impl Audit {
    pub fn step(&mut self, step: &str, detail: impl Into<String>) {
        self.trace.push(TraceStep {
            at: chrono::Utc::now().timestamp_millis(),
            step: step.into(),
            detail: detail.into(),
        });
    }
}
impl Store {
    /// Attach to an existing database without recovering jobs. Used by account probes.
    pub fn attach(path: &Path) -> Result<Self> {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        Ok(Self { db: Mutex::new(db) })
    }
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch(r#"PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS jobs(resource TEXT PRIMARY KEY,status TEXT NOT NULL DEFAULT 'pending',attempts INTEGER NOT NULL DEFAULT 0,next_at INTEGER NOT NULL DEFAULT 0,audit TEXT,created_at INTEGER NOT NULL DEFAULT (unixepoch()));
            CREATE TABLE IF NOT EXISTS subscriptions(id TEXT PRIMARY KEY,resource TEXT NOT NULL UNIQUE,expires_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS vault(name TEXT PRIMARY KEY,value BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY,time INTEGER NOT NULL DEFAULT (unixepoch()),kind TEXT NOT NULL,detail TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS conversation_context(conversation TEXT PRIMARY KEY,question TEXT NOT NULL,answer TEXT NOT NULL,updated_at INTEGER NOT NULL DEFAULT (unixepoch()));
            CREATE TABLE IF NOT EXISTS outputs(nonce TEXT PRIMARY KEY,conversation TEXT NOT NULL,message_id TEXT,created_at INTEGER NOT NULL DEFAULT (unixepoch()));
            CREATE TABLE IF NOT EXISTS activity_cache(key TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at INTEGER NOT NULL DEFAULT (unixepoch()));
            UPDATE jobs SET status='pending' WHERE status='processing' AND resource NOT LIKE 'simulation:%';
            UPDATE jobs SET status='uncertain',audit='{"reason":"restart_during_send"}' WHERE status='sending';"#)?;
        Ok(Self { db: Mutex::new(db) })
    }
    /// Diagnostic connections never recover or mutate jobs.
    pub fn self_chat_diagnostics(path: &Path) -> Result<serde_json::Value> {
        if !path.exists() {
            return Ok(serde_json::Value::Null);
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let cursor = db
            .query_row(
                "SELECT value,updated_at FROM activity_cache WHERE key='self_chat_cursor'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;
        // Pre-upgrade databases have no outputs table yet.
        let outputs_table: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='outputs')",
            [],
            |r| r.get(0),
        )?;
        let unresolved: usize = if outputs_table {
            db.query_row(
                "SELECT count(*) FROM outputs WHERE message_id IS NULL",
                [],
                |r| r.get(0),
            )?
        } else {
            0
        };
        let mut pending = Vec::new();
        if outputs_table {
            let mut stmt=db.prepare("SELECT nonce,conversation,created_at FROM outputs WHERE message_id IS NULL ORDER BY created_at DESC LIMIT 50")?;
            for row in stmt.query_map([],|r|Ok(serde_json::json!({"nonce":r.get::<_,String>(0)?,"conversation":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?})))? {pending.push(row?);}
        }
        Ok(
            serde_json::json!({"cursor":cursor,"unresolved_outputs":unresolved,"pending_outputs":pending}),
        )
    }
    pub fn read_encrypted_token(path: &Path) -> Result<Option<Vec<u8>>> {
        if !path.exists() {
            return Ok(None);
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(db
            .query_row("SELECT value FROM vault WHERE name='oauth'", [], |r| {
                r.get(0)
            })
            .optional()?)
    }
    pub fn has_token(path: &Path) -> Result<bool> {
        if !path.exists() {
            return Ok(false);
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM vault WHERE name='oauth')",
            [],
            |r| r.get(0),
        )?)
    }
    pub fn logout(path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db.execute("DELETE FROM vault WHERE name='oauth'", [])?;
        Ok(())
    }
    pub fn inspect(
        path: &Path,
        events: bool,
        limit: usize,
        resource: Option<&str>,
        content: bool,
    ) -> Result<Vec<serde_json::Value>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut out = Vec::new();
        if events {
            let mut stmt =
                db.prepare("SELECT time,kind,detail FROM events ORDER BY id DESC LIMIT ?1")?;
            for row in stmt.query_map([limit.min(1000)], |r| Ok(serde_json::json!({"time":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"detail":r.get::<_,String>(2)?})))? { out.push(row?); }
        } else {
            let mut stmt = db.prepare("SELECT resource,status,audit,created_at FROM jobs WHERE (?1 IS NULL OR resource=?1) ORDER BY created_at DESC LIMIT ?2")?;
            for row in stmt.query_map(params![resource, limit.min(1000)], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })? {
                let (resource, status, text, created_at) = row?;
                // A job still queued has no audit yet: an empty object, never null.
                let mut audit: serde_json::Value = text
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()?
                    .unwrap_or_else(|| serde_json::json!({}));
                if !content && let Some(obj) = audit.as_object_mut() {
                    for field in ["proposed", "sent", "question", "resolved_question", "topic"] {
                        obj.remove(field);
                    }
                    if let Some(messages) = obj
                        .get_mut("teams_messages")
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        for message in messages {
                            if let Some(m) = message.as_object_mut() {
                                m.remove("text");
                            }
                        }
                    }
                }
                out.push(serde_json::json!({"resource":resource,"status":status,"created_at":created_at,"audit":audit}));
            }
        }
        Ok(out)
    }
    pub fn has_unresolved_output(&self, conversation: &str) -> Result<bool> {
        Ok(self.db.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM outputs WHERE conversation=?1 AND message_id IS NULL)",
            [conversation],
            |r| r.get(0),
        )?)
    }
    pub fn output_intent(&self, nonce: &str) -> Result<Option<(String, i64)>> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT conversation,created_at FROM outputs WHERE nonce=?1 AND message_id IS NULL",
                [nonce],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }
    pub fn begin_output(&self, conversation: &str) -> Result<String> {
        let nonce = uuid::Uuid::new_v4().to_string();
        self.db.lock().unwrap().execute(
            "INSERT INTO outputs(nonce,conversation) VALUES (?1,?2)",
            params![nonce, conversation],
        )?;
        Ok(nonce)
    }
    pub fn finish_output(&self, nonce: &str, id: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE outputs SET message_id=?2 WHERE nonce=?1",
            params![nonce, id],
        )?;
        Ok(())
    }
    pub fn is_output(&self, conversation: &str, id: &str, html: &str) -> Result<bool> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT nonce,message_id FROM outputs WHERE conversation=?1")?;
        for row in stmt.query_map([conversation], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })? {
            let (nonce, sent_id) = row?;
            // Exact durable HTML link marker, supported by Graph HTML message bodies.
            if sent_id.as_deref() == Some(id)
                || html.contains(&format!("https://personalteams.invalid/output/{nonce}"))
            {
                if sent_id.is_none() {
                    db.execute(
                        "UPDATE outputs SET message_id=?2 WHERE nonce=?1",
                        params![nonce, id],
                    )?;
                }
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn enqueue(&self, resource: &str) -> Result<bool> {
        Ok(self.db.lock().unwrap().execute(
            "INSERT OR IGNORE INTO jobs(resource) VALUES (?1)",
            [resource],
        )? == 1)
    }
    pub fn begin_simulation(&self, resource: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO jobs(resource,status) VALUES (?1,'processing')",
            [resource],
        )?;
        Ok(())
    }
    pub fn context(&self, conversation: &str) -> Result<Option<ConversationContext>> {
        Ok(self.db.lock().unwrap().query_row(
            "SELECT question,answer FROM conversation_context WHERE conversation=?1 AND updated_at>=unixepoch()-1800",
            [conversation], |r| Ok(ConversationContext { question:r.get(0)?, answer:r.get(1)? })
        ).optional()?)
    }
    pub fn save_context(&self, conversation: &str, question: &str, answer: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "DELETE FROM conversation_context WHERE updated_at<unixepoch()-1800",
            [],
        )?;
        db.execute("INSERT INTO conversation_context(conversation,question,answer) VALUES (?1,?2,?3) ON CONFLICT(conversation) DO UPDATE SET question=excluded.question,answer=excluded.answer,updated_at=unixepoch()",params![conversation,question,answer])?;
        Ok(())
    }
    pub fn activity_cache(&self, key: &str) -> Result<Option<(String, i64)>> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT value,updated_at FROM activity_cache WHERE key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }
    pub fn clear_activity_cache(&self, key: &str) -> Result<()> {
        self.db
            .lock()
            .unwrap()
            .execute("DELETE FROM activity_cache WHERE key=?1", [key])?;
        Ok(())
    }
    pub fn save_activity_cache(&self, key: &str, value: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "DELETE FROM activity_cache WHERE updated_at<unixepoch()-2592000",
            [],
        )?;
        db.execute("INSERT INTO activity_cache(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=unixepoch()", params![key,value])?;
        Ok(())
    }
    pub fn next_job(&self) -> Result<Option<Job>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let job = tx.query_row("SELECT resource,attempts FROM jobs WHERE status='pending' AND next_at<=unixepoch() ORDER BY created_at LIMIT 1", [], |r| Ok(Job { resource:r.get(0)?, attempts:r.get(1)? })).optional()?;
        if let Some(j) = &job {
            tx.execute(
                "UPDATE jobs SET status='processing',attempts=attempts+1 WHERE resource=?1",
                [&j.resource],
            )?;
        }
        tx.commit()?;
        Ok(job)
    }
    pub fn retry(&self, job: &Job) -> Result<()> {
        let status = if job.attempts >= 4 {
            "failed"
        } else {
            "pending"
        };
        let mut audit = self.audit(&job.resource)?.unwrap_or_default();
        audit.status = status.into();
        audit.reason = "read_or_provider_error".into();
        self.db.lock().unwrap().execute("UPDATE jobs SET status=?2,next_at=unixepoch()+?3,audit=?4 WHERE resource=?1 AND status='processing'", params![job.resource,status,2i64.pow(job.attempts+1),serde_json::to_string(&audit)?])?;
        Ok(())
    }
    pub fn record(&self, resource: &str, audit: &Audit) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE jobs SET status=?2,audit=?3 WHERE resource=?1",
            params![resource, audit.status, serde_json::to_string(audit)?],
        )?;
        Ok(())
    }
    pub fn audit(&self, resource: &str) -> Result<Option<Audit>> {
        let text: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT audit FROM jobs WHERE resource=?1",
                [resource],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        text.map(|v| serde_json::from_str(&v).map_err(Into::into))
            .transpose()
    }
    pub fn status(&self, resource: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT status FROM jobs WHERE resource=?1",
                [resource],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn put_token(&self, value: &[u8]) -> Result<()> {
        self.db.lock().unwrap().execute("INSERT INTO vault VALUES ('oauth',?1) ON CONFLICT(name) DO UPDATE SET value=excluded.value", [value])?;
        Ok(())
    }
    pub fn token(&self) -> Result<Option<Vec<u8>>> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM vault WHERE name='oauth'", [], |r| {
                r.get(0)
            })
            .optional()?)
    }
    pub fn subscriptions(&self) -> Result<Vec<Subscription>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT id,resource,expires_at FROM subscriptions")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Subscription {
                    id: r.get(0)?,
                    resource: r.get(1)?,
                    expires_at: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn subscription_health(path: &Path) -> Result<(usize, Option<String>)> {
        if !path.exists() {
            return Ok((0, None));
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let active: usize = db.query_row(
            "SELECT count(*) FROM subscriptions WHERE expires_at > unixepoch()",
            [],
            |r| r.get(0),
        )?;
        let issue = db.query_row(
            "SELECT detail FROM events WHERE kind='subscription' AND time >= unixepoch()-300 ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        ).optional()?;
        Ok((active, issue))
    }
    pub fn save_subscription(&self, s: &Subscription) -> Result<()> {
        self.db.lock().unwrap().execute("INSERT INTO subscriptions VALUES (?1,?2,?3) ON CONFLICT(resource) DO UPDATE SET id=excluded.id,expires_at=excluded.expires_at", params![s.id,s.resource,s.expires_at])?;
        Ok(())
    }
    pub fn remove_subscription(&self, id: &str) -> Result<()> {
        self.db
            .lock()
            .unwrap()
            .execute("DELETE FROM subscriptions WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn expire_subscription(&self, id: &str) -> Result<()> {
        self.db
            .lock()
            .unwrap()
            .execute("UPDATE subscriptions SET expires_at=0 WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn event(&self, kind: &str, detail: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO events(kind,detail) VALUES (?1,?2)",
            params![kind, detail],
        )?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_job_in_progress_lists_an_empty_audit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assistant.db");
        assert!(
            Store::open(&path)
                .unwrap()
                .enqueue("chats/c/messages/1")
                .unwrap()
        );
        let rows = Store::inspect(&path, false, 10, None, false).unwrap();
        assert_eq!(rows[0]["audit"], serde_json::json!({}));
    }
    #[test]
    fn activity_index_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity.db");
        Store::open(&path)
            .unwrap()
            .save_activity_cache("repo-a", "{\"value\":[]}")
            .unwrap();
        let reopened = Store::open(&path).unwrap();
        let (value, updated_at) = reopened.activity_cache("repo-a").unwrap().unwrap();
        assert_eq!(value, "{\"value\":[]}");
        assert!(updated_at > 0);
    }
    #[test]
    fn dedup_and_restart_do_not_resend() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        {
            let s = Store::open(&p).unwrap();
            assert!(s.enqueue("chats/c/messages/1").unwrap());
            assert!(!s.enqueue("chats/c/messages/1").unwrap());
            assert!(s.next_job().unwrap().is_some());
            assert!(s.next_job().unwrap().is_none());
            s.record(
                "chats/c/messages/1",
                &Audit {
                    status: "sending".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let s = Store::open(&p).unwrap();
        assert_eq!(
            s.status("chats/c/messages/1").unwrap().as_deref(),
            Some("uncertain")
        );
        assert!(s.next_job().unwrap().is_none());
    }
}
