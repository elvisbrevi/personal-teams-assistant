//! The audit history: the finished record of every Teams message the assistant handled (its
//! status and audit: the question, evidence trace, proposal, answer and the user's feedback).
//! Each record is one document identified by the message's `resource`; its audit is its
//! revision. Messages still queued, processed, refined or sent are the worker's runtime state and
//! stay in the local `state::Store`, with the send nonces: the worker's state machine never waits
//! on a store, and a send to Graph is never retried. The audit may hold message text; a store
//! keeps what it was given and the callers decide what to show (`state::redact`).

use crate::{
    questions::IdentifiedQuestion,
    state::{Audit, Feedback, Store, TraceStep},
};
use anyhow::{Result, ensure};
use rusqlite::params;
use serde::Serialize;

/// Statuses of a message the worker has not finished with.
pub const IN_FLIGHT: [&str; 4] = ["pending", "processing", "refining", "sending"];

pub fn finished(status: &str) -> bool {
    !IN_FLIGHT.contains(&status)
}

/// One finished message as listed: the audit is kept as written, older fields included.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AuditEntry {
    pub resource: String,
    pub status: String,
    /// When the message was received (Unix seconds).
    pub created_at: i64,
    pub audit: serde_json::Value,
}

/// Which messages a listing returns, the most recently received first (ties by resource,
/// descending): received in `[since, until)` and, with `after`, older than that message.
#[derive(Clone, Debug, Default)]
pub struct Listing<'a> {
    pub resource: Option<&'a str>,
    /// Unix seconds.
    pub since: Option<i64>,
    pub until: Option<i64>,
    /// A message already listed (its reception time and resource): only older ones.
    pub after: Option<(i64, &'a str)>,
    pub limit: usize,
}

impl Listing<'_> {
    /// The SQL condition on `jobs` and its parameters from `?first`, in the listing's order.
    pub(crate) fn condition(&self, first: usize) -> String {
        let n = |i: usize| first + i;
        format!(
            "(?{a} IS NULL OR resource=?{a}) AND (?{b} IS NULL OR created_at>=?{b}) AND (?{c} IS NULL OR created_at<?{c}) AND (?{d} IS NULL OR created_at<?{d} OR (created_at=?{d} AND resource<?{e}))",
            a = n(0),
            b = n(1),
            c = n(2),
            d = n(3),
            e = n(4)
        )
    }
    pub(crate) fn parameters(&self) -> [rusqlite::types::Value; 5] {
        use rusqlite::types::Value;
        let text = |v: Option<&str>| v.map_or(Value::Null, |s| Value::Text(s.to_owned()));
        let number = |v: Option<i64>| v.map_or(Value::Null, Value::Integer);
        [
            text(self.resource),
            number(self.since),
            number(self.until),
            number(self.after.map(|(at, _)| at)),
            text(self.after.map(|(_, resource)| resource)),
        ]
    }
}

pub trait AuditStore: Send + Sync {
    /// Keep the finished audit of a message, its status being `audit.status`, replacing an
    /// earlier one; an in-flight status is refused.
    fn record(&self, resource: &str, audit: &Audit) -> Result<()>;
    /// Finished messages, the most recently received first; `resource` keeps that one.
    fn entries(&self, resource: Option<&str>, limit: usize) -> Result<Vec<AuditEntry>> {
        self.listing(&Listing {
            resource,
            limit,
            ..Default::default()
        })
    }
    /// Finished messages a listing selects, at most `limit` (and 1000).
    fn listing(&self, listing: &Listing) -> Result<Vec<AuditEntry>>;
    /// Set (or clear) the user's judgment of a finished message without changing anything
    /// else in its audit. False when no finished message has that resource.
    fn set_feedback(&self, resource: &str, feedback: Option<&Feedback>) -> Result<bool>;
    /// Record the configured question an unanswered (ignored or observed) message asked, with
    /// its trace step; an error for any other message.
    fn associate_question(&self, resource: &str, question: &IdentifiedQuestion) -> Result<()>;
}

/// The finished rows of the profile's `assistant.db` › `jobs`, next to the worker's in-flight
/// rows in the same table.
pub struct LocalAudit<'a> {
    store: &'a Store,
}

impl<'a> LocalAudit<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Every finished message as the account store mirrors it, by resource: its status, when it
    /// was received and its audit as written.
    pub fn export(&self) -> Result<Vec<crate::mirror::Exported>> {
        let db = self.store.connection();
        let mut stmt = db.prepare("SELECT resource,status,audit,created_at FROM jobs WHERE status NOT IN ('pending','processing','refining','sending') AND audit IS NOT NULL ORDER BY resource")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (resource, status, audit, created_at) = row?;
            out.push(crate::mirror::Exported {
                id: resource,
                value: serde_json::json!({
                    "status": status,
                    "created_at": created_at,
                    "audit": crate::state::audit_value(Some(&audit))?,
                }),
            });
        }
        Ok(out)
    }
}

impl AuditStore for LocalAudit<'_> {
    fn record(&self, resource: &str, audit: &Audit) -> Result<()> {
        ensure!(
            finished(&audit.status),
            "an in-flight message is not part of the audit history"
        );
        self.store.connection().execute(
            "INSERT INTO jobs(resource,status,audit) VALUES (?1,?2,?3) ON CONFLICT(resource) DO UPDATE SET status=excluded.status,audit=excluded.audit",
            params![resource, audit.status, serde_json::to_string(audit)?],
        )?;
        crate::mirror::wake();
        Ok(())
    }

    fn listing(&self, listing: &Listing) -> Result<Vec<AuditEntry>> {
        let db = self.store.connection();
        let mut stmt = db.prepare(&format!("SELECT resource,status,audit,created_at FROM jobs WHERE status NOT IN ('pending','processing','refining','sending') AND {} ORDER BY created_at DESC, resource DESC LIMIT ?6", listing.condition(1)))?;
        let [a, b, c, d, e] = listing.parameters();
        let rows = stmt.query_map(
            params![a, b, c, d, e, listing.limit.min(1000) as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (resource, status, text, created_at) = row?;
            out.push(AuditEntry {
                resource,
                status,
                created_at,
                audit: crate::state::audit_value(text.as_deref())?,
            });
        }
        Ok(out)
    }

    fn set_feedback(&self, resource: &str, feedback: Option<&Feedback>) -> Result<bool> {
        let changed = self.store.connection().execute(
            "UPDATE jobs SET audit=CASE WHEN ?2 IS NULL THEN json_remove(audit,'$.feedback') ELSE json_set(audit,'$.feedback',json(?2)) END WHERE resource=?1 AND audit IS NOT NULL AND status NOT IN ('pending','processing','refining','sending')",
            params![resource, feedback.map(serde_json::to_string).transpose()?],
        )?;
        crate::mirror::wake();
        Ok(changed > 0)
    }

    fn associate_question(&self, resource: &str, question: &IdentifiedQuestion) -> Result<()> {
        let changed = self.store.connection().execute("UPDATE jobs SET audit=json_set(audit,'$.identified_question',json(?2),'$.reason','question_associated','$.trace[#]',json(?3)) WHERE resource=?1 AND status IN ('ignored','dry_run')", params![resource, serde_json::to_string(question)?, serde_json::to_string(&TraceStep { at: chrono::Utc::now().timestamp_millis(), step: "question association".into(), detail: question.id.clone() })?])?;
        ensure!(
            changed == 1,
            "message changed while associating the question"
        );
        crate::mirror::wake();
        Ok(())
    }
}

/// What every `AuditStore` must do, whatever keeps the records. `store` starts empty and is
/// changed by the suite.
#[cfg(test)]
pub(crate) mod contract {
    use super::*;

    pub(crate) fn run(store: &dyn AuditStore) {
        finished_messages_are_kept_and_replaced(store);
        listings_page_by_time_and_cursor(store);
        in_flight_statuses_are_refused(store);
        feedback_changes_only_itself(store);
        questions_are_associated_only_with_unanswered_messages(store);
    }

    fn audit(status: &str, sent: Option<&str>) -> Audit {
        Audit {
            status: status.into(),
            reason: "supported_answer".into(),
            sent: sent.map(Into::into),
            ..Default::default()
        }
    }

    fn resources(store: &dyn AuditStore) -> Vec<String> {
        store
            .entries(None, 10)
            .unwrap()
            .into_iter()
            .map(|e| e.resource)
            .collect()
    }

    fn finished_messages_are_kept_and_replaced(store: &dyn AuditStore) {
        store
            .record("chats/c/messages/1", &audit("sent", Some("An answer")))
            .unwrap();
        store
            .record("chats/c/messages/2", &audit("ignored", None))
            .unwrap();
        let mut listed = resources(store);
        listed.sort();
        assert_eq!(listed, ["chats/c/messages/1", "chats/c/messages/2"]);
        let one = store.entries(Some("chats/c/messages/1"), 10).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].status, "sent");
        assert_eq!(one[0].audit["sent"], "An answer");
        assert!(one[0].created_at > 0);
        assert_eq!(store.entries(None, 1).unwrap().len(), 1);
        // Recording again replaces the status and the audit.
        store
            .record("chats/c/messages/2", &audit("dry_run", None))
            .unwrap();
        let two = store.entries(Some("chats/c/messages/2"), 10).unwrap();
        assert_eq!(two[0].status, "dry_run");
        assert_eq!(two[0].audit["status"], "dry_run");
        assert!(store.entries(Some("unknown"), 10).unwrap().is_empty());
    }

    fn listings_page_by_time_and_cursor(store: &dyn AuditStore) {
        let all = store.entries(None, 10).unwrap();
        assert!(all.len() >= 2);
        // Received in the same second: the resource breaks the tie, so a cursor misses none.
        let (newest, next) = (&all[0], &all[1]);
        let after = store
            .listing(&Listing {
                after: Some((newest.created_at, &newest.resource)),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(after[0].resource, next.resource);
        assert!(after.iter().all(|e| e.resource != newest.resource));
        let window = |since, until| {
            store
                .listing(&Listing {
                    since,
                    until,
                    limit: 10,
                    ..Default::default()
                })
                .unwrap()
                .len()
        };
        // `since` includes its second and `until` excludes its own, whether or not the messages
        // were received in the same second.
        let oldest = all.iter().map(|e| e.created_at).min().unwrap();
        let earlier = all
            .iter()
            .filter(|e| e.created_at < newest.created_at)
            .count();
        assert_eq!(window(Some(newest.created_at + 1), None), 0);
        assert_eq!(window(None, Some(newest.created_at)), earlier);
        assert_eq!(
            window(Some(newest.created_at), Some(newest.created_at + 1)),
            all.len() - earlier
        );
        assert_eq!(window(Some(oldest), Some(newest.created_at + 1)), all.len());
    }

    fn in_flight_statuses_are_refused(store: &dyn AuditStore) {
        for status in IN_FLIGHT {
            assert!(
                store
                    .record("chats/c/messages/9", &audit(status, None))
                    .is_err()
            );
        }
        assert!(
            store
                .entries(Some("chats/c/messages/9"), 10)
                .unwrap()
                .is_empty()
        );
    }

    fn feedback_changes_only_itself(store: &dyn AuditStore) {
        let feedback = Feedback {
            rating: "good".into(),
            comment: "Clear".into(),
            example: true,
            context: None,
            at: 1,
        };
        let resource = "chats/c/messages/1";
        assert!(store.set_feedback(resource, Some(&feedback)).unwrap());
        let entry = &store.entries(Some(resource), 1).unwrap()[0];
        assert_eq!(entry.status, "sent");
        assert_eq!(entry.audit["sent"], "An answer");
        assert_eq!(
            entry.audit["feedback"],
            serde_json::to_value(&feedback).unwrap()
        );
        assert!(!store.set_feedback("unknown", Some(&feedback)).unwrap());
        assert!(store.set_feedback(resource, None).unwrap());
        let entry = &store.entries(Some(resource), 1).unwrap()[0];
        assert!(entry.audit.get("feedback").is_none());
        assert_eq!(entry.audit["sent"], "An answer");
    }

    fn questions_are_associated_only_with_unanswered_messages(store: &dyn AuditStore) {
        let question = IdentifiedQuestion {
            id: "daily".into(),
            text: "What did I do today?".into(),
        };
        store
            .associate_question("chats/c/messages/2", &question)
            .unwrap();
        let entry = &store.entries(Some("chats/c/messages/2"), 1).unwrap()[0];
        assert_eq!(entry.audit["identified_question"]["id"], "daily");
        assert_eq!(entry.audit["reason"], "question_associated");
        let trace = entry.audit["trace"].as_array().unwrap();
        assert_eq!(trace.last().unwrap()["step"], "question association");
        assert!(
            store
                .associate_question("chats/c/messages/1", &question)
                .is_err(),
            "an answered message keeps its question"
        );
        assert!(store.associate_question("unknown", &question).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_jobs_table_meets_the_contract() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("assistant.db")).unwrap();
        contract::run(&LocalAudit::new(&store));
    }

    #[test]
    fn in_flight_rows_stay_out_of_the_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("assistant.db")).unwrap();
        let history = LocalAudit::new(&store);
        store.enqueue("queued").unwrap();
        store.enqueue("busy").unwrap();
        store
            .record(
                "busy",
                &Audit {
                    status: "refining".into(),
                    proposed: Some("A draft".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(history.entries(None, 10).unwrap().is_empty());
        assert!(
            !history
                .set_feedback(
                    "busy",
                    Some(&Feedback {
                        rating: "good".into(),
                        ..Default::default()
                    })
                )
                .unwrap(),
            "a message being refined is never rated"
        );
        // The worker finishing the message makes it part of the history.
        store
            .record(
                "busy",
                &Audit {
                    status: "dry_run".into(),
                    proposed: Some("A draft".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let listed = history.entries(None, 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].resource, "busy");
        assert_eq!(store.status("busy").unwrap().as_deref(), Some("dry_run"));
    }
}
