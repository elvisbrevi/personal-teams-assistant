use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
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
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch(r#"PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS jobs(resource TEXT PRIMARY KEY,status TEXT NOT NULL DEFAULT 'pending',attempts INTEGER NOT NULL DEFAULT 0,next_at INTEGER NOT NULL DEFAULT 0,audit TEXT,created_at INTEGER NOT NULL DEFAULT (unixepoch()));
            CREATE TABLE IF NOT EXISTS subscriptions(id TEXT PRIMARY KEY,resource TEXT NOT NULL UNIQUE,expires_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS vault(name TEXT PRIMARY KEY,value BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY,time INTEGER NOT NULL DEFAULT (unixepoch()),kind TEXT NOT NULL,detail TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS conversation_context(conversation TEXT PRIMARY KEY,question TEXT NOT NULL,answer TEXT NOT NULL,updated_at INTEGER NOT NULL DEFAULT (unixepoch()));
            CREATE TABLE IF NOT EXISTS activity_cache(key TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at INTEGER NOT NULL DEFAULT (unixepoch()));
            UPDATE jobs SET status='pending' WHERE status='processing' AND resource NOT LIKE 'simulation:%';
            UPDATE jobs SET status='uncertain',audit='{"reason":"restart_during_send"}' WHERE status='sending';"#)?;
        Ok(Self { db: Mutex::new(db) })
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
