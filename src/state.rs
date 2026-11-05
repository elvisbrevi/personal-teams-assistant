use crate::store::audit::{self, AuditEntry, AuditStore, Listing, LocalAudit};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identified_question: Option<crate::questions::IdentifiedQuestion>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub available_sources: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub selected_sources: Vec<String>,
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
    /// What the message asked: `question`, `activity_review`, `greeting`, `shared_link` or
    /// `link`; for a message left unanswered, `statement` (informational) or `personal` (a
    /// request for the person), which an answer written at the user's request keeps.
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
    /// Context the user gave in the app to improve an answer that was not sent (redacted
    /// text; never sent to Teams). Present while it is improved and after.
    pub refinement: Option<String>,
    /// Sources the user chose in the app for an answer written at their request; empty means
    /// the sources authorized for the conversation.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chosen_sources: Vec<String>,
    /// The context (subject) the request named, whose sources and instructions shaped it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Who wrote the message: Graph user ID and display name (eligible messages only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    /// The person profile the answer was written for (`[[people]]` in the map).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// The user's judgment of the answer, given in the app or by an agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<Feedback>,
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
/// The user's judgment of an answer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Feedback {
    /// `good` or `bad`.
    pub rating: String,
    /// What was right or wrong, in the user's words (redacted).
    pub comment: String,
    /// The request and answer guide later answers to similar requests (structure and tone).
    pub example: bool,
    /// The context the example belongs to; none = every context.
    pub context: Option<String>,
    /// When it was given (Unix seconds).
    pub at: i64,
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
/// A stored audit as JSON: a job still queued has none yet, an empty object, never null.
pub(crate) fn audit_value(text: Option<&str>) -> Result<serde_json::Value> {
    Ok(text
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({})))
}
/// Remove the message content from an audit (what `audit` shows without `content`).
pub fn redact(audit: &mut serde_json::Value) {
    let Some(obj) = audit.as_object_mut() else {
        return;
    };
    for field in [
        "proposed",
        "sent",
        "question",
        "resolved_question",
        "topic",
        "refinement",
    ] {
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
impl Store {
    /// Attach to an existing database without recovering jobs. Used by account probes.
    pub fn attach(path: &Path) -> Result<Self> {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        Ok(Self { db: Mutex::new(db) })
    }
    fn initialize(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch(r#"PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS jobs(resource TEXT PRIMARY KEY,status TEXT NOT NULL DEFAULT 'pending',attempts INTEGER NOT NULL DEFAULT 0,next_at INTEGER NOT NULL DEFAULT 0,audit TEXT,created_at INTEGER NOT NULL DEFAULT (unixepoch()));
            CREATE TABLE IF NOT EXISTS subscriptions(id TEXT PRIMARY KEY,resource TEXT NOT NULL UNIQUE,expires_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS vault(name TEXT PRIMARY KEY,value BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY,time INTEGER NOT NULL DEFAULT (unixepoch()),kind TEXT NOT NULL,detail TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS conversation_context(conversation TEXT PRIMARY KEY,question TEXT NOT NULL,answer TEXT NOT NULL,updated_at INTEGER NOT NULL DEFAULT (unixepoch()));
            CREATE TABLE IF NOT EXISTS outputs(nonce TEXT PRIMARY KEY,conversation TEXT NOT NULL,message_id TEXT,created_at INTEGER NOT NULL DEFAULT (unixepoch()));
            CREATE TABLE IF NOT EXISTS activity_cache(key TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at INTEGER NOT NULL DEFAULT (unixepoch()));"#)?;
        Ok(Self { db: Mutex::new(db) })
    }
    pub fn open(path: &Path) -> Result<Self> {
        let store = Self::initialize(path)?;
        store.db.lock().unwrap().execute_batch(r#"
            UPDATE jobs SET status='pending' WHERE status='processing' AND resource NOT LIKE 'simulation:%';
            UPDATE jobs SET status='uncertain',audit='{"reason":"restart_during_send"}' WHERE status='sending';
            UPDATE jobs SET status='ignored',audit=json_set(coalesce(audit,'{}'),'$.status','ignored','$.reason','refinement_failed') WHERE status='refining';"#)?;
        Ok(store)
    }
    /// The connection, for the profile's local stores in the same database (`LocalAudit`).
    pub(crate) fn connection(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap()
    }
    /// Append a host diagnostic without recovering or changing in-flight jobs.
    pub(crate) fn append_event(path: &Path, kind: &str, detail: &str) -> Result<()> {
        Self::initialize(path)?.event(kind, detail)
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
            return Ok(out);
        }
        let store = Self { db: Mutex::new(db) };
        let listing = Listing {
            resource,
            limit,
            ..Default::default()
        };
        for mut entry in store.listed(&listing)? {
            if !content {
                redact(&mut entry.audit);
            }
            out.push(serde_json::json!({"resource":entry.resource,"status":entry.status,"created_at":entry.created_at,"audit":entry.audit}));
        }
        Ok(out)
    }
    /// A page of the messages a listing selects, newest first: in-flight and finished ones,
    /// `next_cursor` (the last one's resource) when more remain, how many the window holds and
    /// when the oldest kept message was received. Nothing is ever deleted from the audit, so
    /// the cursor reaches the first message. A page stops early before 1.5 MB of JSON.
    pub fn inspect_page(
        path: &Path,
        listing: &Listing,
        content: bool,
    ) -> Result<serde_json::Value> {
        if !path.exists() {
            return Ok(
                serde_json::json!({"entries":[],"next_cursor":null,"total":0,"oldest_at":null}),
            );
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let (total, oldest): (i64, Option<i64>) = db.query_row(
            &format!(
                "SELECT COUNT(*),(SELECT MIN(created_at) FROM jobs) FROM jobs WHERE {}",
                Listing {
                    after: None,
                    ..listing.clone()
                }
                .condition(1)
            ),
            rusqlite::params_from_iter(
                Listing {
                    after: None,
                    ..listing.clone()
                }
                .parameters(),
            ),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let store = Self { db: Mutex::new(db) };
        let limit = listing.limit.clamp(1, 1000);
        let mut entries = store.listed(&Listing {
            limit: limit + 1,
            ..listing.clone()
        })?;
        let mut more = entries.len() > limit;
        entries.truncate(limit);
        let (mut out, mut size) = (Vec::new(), 0);
        for mut entry in entries {
            if !content {
                redact(&mut entry.audit);
            }
            let value = serde_json::json!({"resource":entry.resource,"status":entry.status,"created_at":entry.created_at,"audit":entry.audit});
            size += value.to_string().len();
            if size > 1_500_000 && !out.is_empty() {
                more = true;
                break;
            }
            out.push(value);
        }
        let next_cursor = more
            .then(|| {
                out.last()
                    .and_then(|e| e["resource"].as_str().map(str::to_owned))
            })
            .flatten();
        Ok(
            serde_json::json!({"entries":out,"next_cursor":next_cursor,"total":total,"oldest_at":oldest}),
        )
    }
    /// When a message was received, to list the ones before it.
    pub fn received_at(path: &Path, resource: &str) -> Result<Option<i64>> {
        use rusqlite::OptionalExtension;
        if !path.exists() {
            return Ok(None);
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(db
            .query_row(
                "SELECT created_at FROM jobs WHERE resource=?1",
                [resource],
                |r| r.get(0),
            )
            .optional()?)
    }
    /// The worker's in-flight messages, then the finished ones from the audit history, merged
    /// in the listing's order.
    fn listed(&self, listing: &Listing) -> Result<Vec<AuditEntry>> {
        let limit = listing.limit.min(1000);
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(&format!("SELECT resource,status,audit,created_at FROM jobs WHERE status IN ('pending','processing','refining','sending') AND {} ORDER BY created_at DESC, resource DESC LIMIT ?6", listing.condition(1)))?;
        let [a, b, c, d, e] = listing.parameters();
        let mut entries = Vec::new();
        for row in stmt.query_map(params![a, b, c, d, e, limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })? {
            let (resource, status, text, created_at) = row?;
            entries.push(AuditEntry {
                resource,
                status,
                created_at,
                audit: audit_value(text.as_deref())?,
            });
        }
        drop(stmt);
        drop(db);
        entries.extend(LocalAudit::new(self).listing(&Listing {
            limit,
            ..listing.clone()
        })?);
        entries.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.resource.cmp(&a.resource))
        });
        entries.truncate(limit);
        Ok(entries)
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
    /// Write a message's status and audit: a finished one goes to the audit history, an
    /// in-flight one stays the worker's.
    pub fn record(&self, resource: &str, audit: &Audit) -> Result<()> {
        if audit::finished(&audit.status) {
            return LocalAudit::new(self).record(resource, audit);
        }
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
    fn feedback_changes_only_itself_and_never_a_job_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assistant.db");
        let store = Store::open(&path).unwrap();
        for resource in ["done", "busy"] {
            store.enqueue(resource).unwrap();
        }
        let done = Audit {
            status: "sent".into(),
            reason: "supported_answer".into(),
            sent: Some("Respuesta".into()),
            ..Default::default()
        };
        store.record("done", &done).unwrap();
        store
            .record(
                "busy",
                &Audit {
                    status: "processing".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let feedback = Feedback {
            rating: "good".into(),
            comment: "Clara".into(),
            example: true,
            context: None,
            at: 1,
        };
        let history = LocalAudit::new(&store);
        assert!(history.set_feedback("done", Some(&feedback)).unwrap());
        let saved = store.audit("done").unwrap().unwrap();
        assert_eq!(saved.feedback, Some(feedback.clone()));
        assert_eq!(saved.sent.as_deref(), Some("Respuesta"));
        assert_eq!(store.status("done").unwrap().as_deref(), Some("sent"));
        assert!(!history.set_feedback("busy", Some(&feedback)).unwrap());
        assert!(!history.set_feedback("missing", Some(&feedback)).unwrap());
        assert!(history.set_feedback("done", None).unwrap());
        assert_eq!(store.audit("done").unwrap().unwrap().feedback, None);
    }
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
    fn the_listing_joins_in_flight_and_finished_messages_without_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assistant.db");
        let store = Store::open(&path).unwrap();
        for resource in ["queued", "answered"] {
            store.enqueue(resource).unwrap();
        }
        store
            .connection()
            .execute(
                "UPDATE jobs SET created_at=created_at-60 WHERE resource='answered'",
                [],
            )
            .unwrap();
        store
            .record(
                "answered",
                &Audit {
                    status: "sent".into(),
                    sent: Some("Respuesta".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let rows = Store::inspect(&path, false, 10, None, false).unwrap();
        let listed: Vec<_> = rows
            .iter()
            .map(|r| r["resource"].as_str().unwrap())
            .collect();
        assert_eq!(listed, ["queued", "answered"], "newest first");
        assert_eq!(rows[1]["status"], "sent");
        assert!(rows[1]["audit"].get("sent").is_none(), "content stays out");
        let rows = Store::inspect(&path, false, 1, Some("answered"), true).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["audit"]["sent"], "Respuesta");
        assert_eq!(
            Store::inspect(&path, false, 1, None, true).unwrap().len(),
            1
        );
    }
    #[test]
    fn audit_pages_reach_the_first_message_through_their_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assistant.db");
        let store = Store::open(&path).unwrap();
        // Five messages, three of them received in the same second, and one still in flight.
        for (resource, age) in [
            ("m1", 300),
            ("m2", 200),
            ("m3", 100),
            ("m4", 100),
            ("m5", 100),
        ] {
            store.enqueue(resource).unwrap();
            store
                .connection()
                .execute(
                    "UPDATE jobs SET created_at=1700000000-?2 WHERE resource=?1",
                    rusqlite::params![resource, age],
                )
                .unwrap();
            if resource != "m5" {
                LocalAudit::new(&store)
                    .record(
                        resource,
                        &Audit {
                            status: "ignored".into(),
                            reason: "ineligible_message".into(),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
        }
        let page = |after: Option<(i64, &str)>, until: Option<i64>| {
            Store::inspect_page(
                &path,
                &Listing {
                    after,
                    until,
                    limit: 2,
                    ..Default::default()
                },
                false,
            )
            .unwrap()
        };
        let resources = |page: &serde_json::Value| -> Vec<String> {
            page["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["resource"].as_str().unwrap().to_owned())
                .collect()
        };
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let at = cursor
                .as_deref()
                .map(|c| Store::received_at(&path, c).unwrap().unwrap());
            let next = page(at.zip(cursor.as_deref()), None);
            assert_eq!(next["total"], 5);
            assert_eq!(next["oldest_at"], 1_700_000_000 - 300);
            seen.extend(resources(&next));
            match next["next_cursor"].as_str() {
                Some(c) => cursor = Some(c.to_owned()),
                None => break,
            }
        }
        assert_eq!(seen, ["m5", "m4", "m3", "m2", "m1"]);
        // A date cursor keeps what was received before it.
        assert_eq!(
            resources(&page(None, Some(1_700_000_000 - 150))),
            ["m2", "m1"]
        );
        assert!(Store::received_at(&path, "unknown").unwrap().is_none());
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
