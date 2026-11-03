//! The notes aggregate: the account's verified notes, its answer evaluations and its approved
//! examples, the knowledge records the user (or an agent on their behalf) wrote. Each record is
//! one document of its collection: a note by its `id` (its `updated_at` is its revision), an
//! evaluation and an example by the Teams message `resource` they judge (their content is their
//! revision). The validation and access rules stay with their callers (`Note::validate`,
//! sensitive data, contexts, the coordinator's conflict tokens); a store only keeps and returns
//! what it was given.
//!
//! The search index is not part of the aggregate: it is a local cache derived from these records
//! and the repository checkouts, and ranked selections (`Index::evaluations`,
//! `Index::examples`) read it next to a store.

use crate::{
    index::{EXAMPLES, Index, NOTES, Note, feedback_source, write_document},
    questions::AnswerEvaluation,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// An answer the user approved as an example for similar requests: its request is searched and
/// the whole answer is returned.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Example {
    /// The Teams message that was answered.
    pub resource: String,
    pub request: String,
    pub answer: String,
    /// The map's context it belongs to; none = every context.
    pub context: Option<String>,
}

pub trait NotesStore: Send + Sync {
    fn note(&self, id: &str) -> Result<Option<Note>>;
    /// Notes, newest first; `context` keeps those in it (and those in every context).
    fn notes(&self, context: Option<&str>, limit: usize) -> Result<Vec<Note>>;
    /// Create a note (empty `id`: a new ID and creation time) or replace a known one, keeping
    /// its creation time; returns it as saved, with the time of the change.
    fn save_note(&self, note: Note) -> Result<Note>;
    /// Store a note exactly as given, keeping its ID and timestamps (a note the account's
    /// coordinator already saved).
    fn put_note(&self, note: &Note) -> Result<()>;
    /// Make the notes exactly these (a synchronized snapshot), all at once.
    fn replace_notes(&self, notes: &[Note]) -> Result<()>;
    /// Whether there was a note to remove.
    fn remove_note(&self, id: &str) -> Result<bool>;

    /// Keep the evaluation of the answer to `value.resource`, replacing an earlier one.
    fn save_evaluation(&self, value: &AnswerEvaluation) -> Result<()>;
    fn remove_evaluation(&self, resource: &str) -> Result<()>;
    /// The evaluations of answers to `question`, the most recently saved first.
    fn evaluations(&self, question: &str) -> Result<Vec<AnswerEvaluation>>;
    /// Forget the evaluations of every question not in `questions` (questions removed from the
    /// map).
    fn retain_evaluations(&self, questions: &[&str]) -> Result<()>;

    /// Keep an approved example, replacing an earlier one of the same message.
    fn save_example(&self, example: &Example) -> Result<()>;
    fn remove_example(&self, resource: &str) -> Result<()>;
    fn example(&self, resource: &str) -> Result<Option<Example>>;
}

/// The records in the profile's knowledge index (`knowledge-index.db`), next to the search
/// documents derived from them: each write changes a record and its document in one transaction,
/// so the local search always finds what the records hold.
pub struct LocalNotes<'a> {
    index: &'a Index,
}

impl<'a> LocalNotes<'a> {
    pub fn new(index: &'a Index) -> Self {
        Self { index }
    }

    /// A write of a mirrored record: the mirror is told once it is committed.
    fn write<T>(&self, f: impl FnOnce(&rusqlite::Transaction) -> Result<T>) -> Result<T> {
        let written = self.index.write(f)?;
        crate::mirror::wake();
        Ok(written)
    }

    /// The verified notes, by ID, as the account store mirrors them.
    pub fn export_notes(&self) -> Result<Vec<crate::mirror::Exported>> {
        self.index.read(|db| {
            let mut stmt = db.prepare("SELECT id,payload,updated_at FROM notes ORDER BY id")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, payload, updated_at) = row?;
                out.push(crate::mirror::Exported {
                    id,
                    value: serde_json::json!({
                        "updated_at": updated_at,
                        "note": serde_json::from_str::<serde_json::Value>(&payload)?,
                    }),
                });
            }
            Ok(out)
        })
    }

    /// The answer evaluations, by Teams message, as the account store mirrors them.
    pub fn export_evaluations(&self) -> Result<Vec<crate::mirror::Exported>> {
        self.index.read(|db| {
            let mut stmt = db.prepare(
                "SELECT resource,question_id,payload,updated_at FROM question_feedback ORDER BY resource",
            )?;
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
                let (resource, question, payload, updated_at) = row?;
                out.push(crate::mirror::Exported {
                    id: resource,
                    value: serde_json::json!({
                        "question_id": question,
                        "updated_at": updated_at,
                        "evaluation": serde_json::from_str::<serde_json::Value>(&payload)?,
                    }),
                });
            }
            Ok(out)
        })
    }

    /// The approved examples, by Teams message, as the account store mirrors them.
    pub fn export_examples(&self) -> Result<Vec<crate::mirror::Exported>> {
        self.index.read(|db| {
            let mut stmt = db.prepare(
                "SELECT resource,request,answer,context,updated_at FROM examples ORDER BY resource",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (resource, request, answer, context, updated_at) = row?;
                out.push(crate::mirror::Exported {
                    id: resource,
                    value: serde_json::json!({
                        "request": request,
                        "answer": answer,
                        "context": context,
                        "updated_at": updated_at,
                    }),
                });
            }
            Ok(out)
        })
    }
}

impl NotesStore for LocalNotes<'_> {
    fn note(&self, id: &str) -> Result<Option<Note>> {
        self.index.read(|db| {
            let payload: Option<String> = db
                .query_row("SELECT payload FROM notes WHERE id=?1", [id], |r| r.get(0))
                .optional()?;
            payload
                .map(|p| serde_json::from_str(&p).map_err(Into::into))
                .transpose()
        })
    }

    fn notes(&self, context: Option<&str>, limit: usize) -> Result<Vec<Note>> {
        self.index.read(|db| {
            let notes: Vec<Note> = db
                .prepare("SELECT payload FROM notes ORDER BY updated_at DESC")?
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(|p| p.ok().and_then(|p| serde_json::from_str(&p).ok()))
                .filter(|n: &Note| {
                    context
                        .is_none_or(|c| n.contexts.is_empty() || n.contexts.iter().any(|x| x == c))
                })
                .take(limit)
                .collect();
            Ok(notes)
        })
    }

    fn save_note(&self, mut note: Note) -> Result<Note> {
        let now = chrono::Utc::now().timestamp();
        self.write(|tx| {
            if note.id.is_empty() {
                note.id = uuid::Uuid::new_v4().to_string();
                note.created_at = now;
            } else {
                ensure!(
                    note.id.len() <= 64
                        && note
                            .id
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                    "invalid note ID"
                );
                let existing: Option<String> = tx
                    .query_row("SELECT payload FROM notes WHERE id=?1", [&note.id], |r| {
                        r.get(0)
                    })
                    .optional()?;
                let existing: Note = serde_json::from_str(&existing.context("unknown note")?)?;
                note.created_at = existing.created_at;
            }
            note.updated_at = now;
            put_note(tx, &note)
        })?;
        Ok(note)
    }

    fn put_note(&self, note: &Note) -> Result<()> {
        self.write(|tx| put_note(tx, note))
    }

    fn replace_notes(&self, notes: &[Note]) -> Result<()> {
        self.write(|tx| {
            let held: Vec<(String, String)> = tx
                .prepare("SELECT id,payload FROM notes")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            let wanted: BTreeSet<&str> = notes.iter().map(|n| n.id.as_str()).collect();
            for (id, _) in held.iter().filter(|(id, _)| !wanted.contains(id.as_str())) {
                tx.execute("DELETE FROM notes WHERE id=?1", [id])?;
                tx.execute(
                    "DELETE FROM documents WHERE source=?1 AND key=?2",
                    params![NOTES, id],
                )?;
            }
            for note in notes {
                let payload = serde_json::to_string(note)?;
                if held
                    .iter()
                    .any(|(id, held)| id == &note.id && held == &payload)
                {
                    continue;
                }
                put_note(tx, note)?;
            }
            Ok(())
        })
    }

    fn remove_note(&self, id: &str) -> Result<bool> {
        self.write(|tx| {
            let removed = tx.execute("DELETE FROM notes WHERE id=?1", [id])? > 0;
            tx.execute(
                "DELETE FROM documents WHERE source=?1 AND key=?2",
                params![NOTES, id],
            )?;
            Ok(removed)
        })
    }

    fn save_evaluation(&self, value: &AnswerEvaluation) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.write(|tx| {
            tx.execute(
                "DELETE FROM documents WHERE key=?1 AND source LIKE '@feedback:%'",
                [&value.resource],
            )?;
            tx.execute("INSERT OR REPLACE INTO question_feedback(resource,question_id,payload,updated_at) VALUES (?1,?2,?3,?4)", params![value.resource, value.question_id, serde_json::to_string(value)?, now])?;
            write_document(
                tx,
                &feedback_source(&value.question_id),
                &value.resource,
                "Answer evaluation",
                &now.to_string(),
                "[]",
                &value.request,
            )
        })
    }

    fn remove_evaluation(&self, resource: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM question_feedback WHERE resource=?1",
                [resource],
            )?;
            tx.execute(
                "DELETE FROM documents WHERE key=?1 AND source LIKE '@feedback:%'",
                [resource],
            )?;
            Ok(())
        })
    }

    fn evaluations(&self, question: &str) -> Result<Vec<AnswerEvaluation>> {
        self.index.read(|db| {
            Ok(db
                .prepare("SELECT payload FROM question_feedback WHERE question_id=?1 ORDER BY updated_at DESC,resource")?
                .query_map([question], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter_map(|p| serde_json::from_str(&p).ok())
                .collect())
        })
    }

    fn retain_evaluations(&self, questions: &[&str]) -> Result<()> {
        self.write(|tx| {
            let evaluated: Vec<String> = tx
                .prepare("SELECT DISTINCT question_id FROM question_feedback")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in evaluated {
                if !questions.contains(&id.as_str()) {
                    tx.execute("DELETE FROM question_feedback WHERE question_id=?1", [&id])?;
                    tx.execute(
                        "DELETE FROM documents WHERE source=?1",
                        [feedback_source(&id)],
                    )?;
                }
            }
            Ok(())
        })
    }

    fn save_example(&self, example: &Example) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let Example {
            resource,
            request,
            answer,
            context,
        } = example;
        self.write(|tx| {
            tx.execute(
                "INSERT INTO examples(resource,request,answer,context,updated_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(resource) DO UPDATE SET request=excluded.request,answer=excluded.answer,context=excluded.context,updated_at=excluded.updated_at",
                params![resource, request, answer, context, now],
            )?;
            let contexts = serde_json::to_string(&context.iter().collect::<Vec<_>>())?;
            write_document(
                tx,
                EXAMPLES,
                resource,
                "example",
                &now.to_string(),
                &contexts,
                request,
            )
        })
    }

    fn remove_example(&self, resource: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM examples WHERE resource=?1", [resource])?;
            tx.execute(
                "DELETE FROM documents WHERE source=?1 AND key=?2",
                params![EXAMPLES, resource],
            )?;
            Ok(())
        })
    }

    fn example(&self, resource: &str) -> Result<Option<Example>> {
        self.index.read(|db| {
            Ok(db
                .query_row(
                    "SELECT request,answer,context FROM examples WHERE resource=?1",
                    [resource],
                    |r| {
                        Ok(Example {
                            resource: resource.to_owned(),
                            request: r.get(0)?,
                            answer: r.get(1)?,
                            context: r.get(2)?,
                        })
                    },
                )
                .optional()?)
        })
    }
}

/// Write a note row and its searchable document.
fn put_note(tx: &rusqlite::Transaction, note: &Note) -> Result<()> {
    tx.execute(
        "INSERT INTO notes(id,payload,updated_at) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload,updated_at=excluded.updated_at",
        params![note.id, serde_json::to_string(note)?, note.updated_at],
    )?;
    // The keywords are searched with the text; the note is one document.
    let body = if note.keywords.is_empty() {
        note.text.clone()
    } else {
        format!("{}\n\n{}", note.text, note.keywords.join(", "))
    };
    write_document(
        tx,
        NOTES,
        &note.id,
        &note.title,
        &note.updated_at.to_string(),
        &serde_json::to_string(&note.contexts)?,
        &body,
    )
}

/// What every `NotesStore` must do, whatever keeps the records. `store` starts empty and is
/// changed by the suite.
#[cfg(test)]
pub(crate) mod contract {
    use super::*;

    pub(crate) fn run(store: &dyn NotesStore) {
        notes_are_created_replaced_and_removed(store);
        notes_are_listed_newest_first_within_a_context(store);
        a_snapshot_replaces_every_note(store);
        evaluations_are_kept_per_message_and_question(store);
        examples_are_kept_per_message(store);
    }

    fn note(id: &str, contexts: &[&str], updated_at: i64) -> Note {
        Note {
            id: id.into(),
            title: format!("Title {id}"),
            text: format!("Text of {id}"),
            contexts: contexts.iter().map(|c| c.to_string()).collect(),
            keywords: vec!["contoso".into()],
            origin: "agent".into(),
            reference: Some("https://example.com/page".into()),
            created_at: 1,
            updated_at,
        }
    }

    fn evaluation(resource: &str, question: &str, rating: &str) -> AnswerEvaluation {
        AnswerEvaluation {
            resource: resource.into(),
            question_id: question.into(),
            request: format!("Request of {resource}"),
            answer: "An answer".into(),
            rating: rating.into(),
            comment: "A comment".into(),
            sources: vec!["handbook".into()],
        }
    }

    fn notes_are_created_replaced_and_removed(store: &dyn NotesStore) {
        let saved = store
            .save_note(Note {
                title: "Release window".into(),
                text: "Releases go out on Tuesdays.".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(!saved.id.is_empty() && saved.created_at > 0);
        assert!(saved.updated_at >= saved.created_at);
        assert_eq!(store.note(&saved.id).unwrap(), Some(saved.clone()));
        let mut edited = saved.clone();
        edited.text = "Releases go out on Thursdays.".into();
        edited.created_at = 0;
        let edited = store.save_note(edited).unwrap();
        assert_eq!(edited.created_at, saved.created_at, "creation is kept");
        assert_eq!(store.note(&saved.id).unwrap(), Some(edited));
        assert!(store.save_note(note("unknown-note", &[], 0)).is_err());
        assert!(store.save_note(note("Not An ID", &[], 0)).is_err());
        assert!(store.note("unknown-note").unwrap().is_none());
        assert!(store.remove_note(&saved.id).unwrap());
        assert!(!store.remove_note(&saved.id).unwrap());
        assert!(store.note(&saved.id).unwrap().is_none());
    }

    fn notes_are_listed_newest_first_within_a_context(store: &dyn NotesStore) {
        let general = note("general", &[], 10);
        let billing = note("billing", &["billing"], 30);
        let sales = note("sales", &["sales"], 20);
        for note in [&general, &billing, &sales] {
            store.put_note(note).unwrap();
        }
        assert_eq!(
            store.note("billing").unwrap().as_ref(),
            Some(&billing),
            "kept exactly as given"
        );
        assert_eq!(
            store.notes(None, 10).unwrap(),
            vec![billing.clone(), sales.clone(), general.clone()]
        );
        assert_eq!(
            store.notes(Some("billing"), 10).unwrap(),
            vec![billing.clone(), general.clone()]
        );
        assert_eq!(store.notes(None, 1).unwrap(), vec![billing]);
    }

    fn a_snapshot_replaces_every_note(store: &dyn NotesStore) {
        let mut changed = note("billing", &["billing"], 40);
        changed.text = "Changed elsewhere".into();
        let added = note("added", &[], 5);
        store
            .replace_notes(&[changed.clone(), added.clone()])
            .unwrap();
        assert_eq!(store.notes(None, 10).unwrap(), vec![changed, added]);
        assert!(store.note("general").unwrap().is_none());
        store.replace_notes(&[]).unwrap();
        assert!(store.notes(None, 10).unwrap().is_empty());
    }

    fn evaluations_are_kept_per_message_and_question(store: &dyn NotesStore) {
        let resources = |question: &str| -> BTreeSet<String> {
            store
                .evaluations(question)
                .unwrap()
                .into_iter()
                .map(|v| v.resource)
                .collect()
        };
        store
            .save_evaluation(&evaluation("m1", "daily", "bad"))
            .unwrap();
        store
            .save_evaluation(&evaluation("m2", "daily", "good"))
            .unwrap();
        store
            .save_evaluation(&evaluation("m3", "weekly", "good"))
            .unwrap();
        assert_eq!(
            resources("daily"),
            BTreeSet::from(["m1".into(), "m2".into()])
        );
        assert!(store.evaluations("unknown").unwrap().is_empty());
        // Saving the same message again replaces its evaluation, even under another question.
        store
            .save_evaluation(&evaluation("m1", "daily", "good"))
            .unwrap();
        let daily = store.evaluations("daily").unwrap();
        assert_eq!(daily.len(), 2);
        assert!(daily.iter().all(|v| v.rating == "good"));
        store
            .save_evaluation(&evaluation("m1", "weekly", "bad"))
            .unwrap();
        assert_eq!(resources("daily"), BTreeSet::from(["m2".into()]));
        assert_eq!(
            resources("weekly"),
            BTreeSet::from(["m1".into(), "m3".into()])
        );
        store.remove_evaluation("m2").unwrap();
        store.remove_evaluation("never-rated").unwrap();
        assert!(store.evaluations("daily").unwrap().is_empty());
        store
            .save_evaluation(&evaluation("m4", "daily", "bad"))
            .unwrap();
        store.retain_evaluations(&["daily"]).unwrap();
        assert!(store.evaluations("weekly").unwrap().is_empty());
        assert_eq!(resources("daily"), BTreeSet::from(["m4".into()]));
        store.retain_evaluations(&[]).unwrap();
        assert!(store.evaluations("daily").unwrap().is_empty());
    }

    fn examples_are_kept_per_message(store: &dyn NotesStore) {
        let mut example = Example {
            resource: "chats/x/messages/1".into(),
            request: "When is support open?".into(),
            answer: "From 9:00 to 18:00.".into(),
            context: Some("support".into()),
        };
        store.save_example(&example).unwrap();
        assert_eq!(
            store.example(&example.resource).unwrap().as_ref(),
            Some(&example)
        );
        example.answer = "From 8:00 to 17:00.".into();
        example.context = None;
        store.save_example(&example).unwrap();
        assert_eq!(
            store.example(&example.resource).unwrap().as_ref(),
            Some(&example)
        );
        store.remove_example(&example.resource).unwrap();
        store.remove_example("never-approved").unwrap();
        assert!(store.example(&example.resource).unwrap().is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FILE;

    fn index() -> (tempfile::TempDir, Index) {
        let data = tempfile::tempdir().unwrap();
        let index = Index::open(&data.path().join(FILE)).unwrap();
        (data, index)
    }

    fn keys(index: &Index, source: &str, query: &str) -> Vec<String> {
        index
            .search(&[source], query, None, None, 10)
            .unwrap()
            .into_iter()
            .map(|h| h.key)
            .collect()
    }

    #[test]
    fn local_index_meets_the_contract() {
        let (_data, index) = index();
        contract::run(&LocalNotes::new(&index));
    }

    #[test]
    fn notes_and_their_search_documents_change_together() {
        let (_data, index) = index();
        let store = LocalNotes::new(&index);
        let saved = store
            .save_note(Note {
                title: "Release window".into(),
                text: "Releases go out on Tuesdays.".into(),
                keywords: vec!["deployment".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(keys(&index, NOTES, "deployment"), vec![saved.id.clone()]);
        let mut edited = saved.clone();
        edited.text = "Releases go out on Thursdays.".into();
        store.save_note(edited).unwrap();
        assert!(keys(&index, NOTES, "Tuesdays").is_empty());
        assert_eq!(keys(&index, NOTES, "Thursdays"), vec![saved.id.clone()]);
        store.remove_note(&saved.id).unwrap();
        assert!(keys(&index, NOTES, "Thursdays").is_empty());
        let kept = Note {
            id: "kept".into(),
            title: "Kept".into(),
            text: "Support answers in Spanish.".into(),
            ..Default::default()
        };
        let dropped = Note {
            id: "dropped".into(),
            title: "Dropped".into(),
            text: "Support answers in English.".into(),
            ..Default::default()
        };
        store.replace_notes(&[kept.clone(), dropped]).unwrap();
        store.replace_notes(&[kept]).unwrap();
        assert_eq!(keys(&index, NOTES, "support"), vec!["kept".to_owned()]);
    }

    #[test]
    fn evaluations_and_examples_are_searched_by_their_requests() {
        let (_data, index) = index();
        let store = LocalNotes::new(&index);
        let evaluation = AnswerEvaluation {
            resource: "m1".into(),
            question_id: "daily".into(),
            request: "Summary of the invoices".into(),
            answer: "An answer".into(),
            rating: "bad".into(),
            comment: String::new(),
            sources: Vec::new(),
        };
        store.save_evaluation(&evaluation).unwrap();
        let daily = feedback_source("daily");
        assert_eq!(keys(&index, &daily, "invoices"), vec!["m1".to_owned()]);
        // Moving the message to another question moves its document.
        store
            .save_evaluation(&AnswerEvaluation {
                question_id: "weekly".into(),
                ..evaluation.clone()
            })
            .unwrap();
        assert!(keys(&index, &daily, "invoices").is_empty());
        store.retain_evaluations(&["daily"]).unwrap();
        assert!(keys(&index, &feedback_source("weekly"), "invoices").is_empty());
        store.save_evaluation(&evaluation).unwrap();
        store.remove_evaluation("m1").unwrap();
        assert!(keys(&index, &daily, "invoices").is_empty());

        let example = Example {
            resource: "chats/x/messages/1".into(),
            request: "When is support open?".into(),
            answer: "From 9:00 to 18:00.".into(),
            context: Some("support".into()),
        };
        store.save_example(&example).unwrap();
        let support = vec!["support".to_owned()];
        let sales = vec!["sales".to_owned()];
        let found = |contexts: &[String]| {
            index
                .search(&[EXAMPLES], "support open", Some(contexts), None, 5)
                .unwrap()
                .len()
        };
        assert_eq!(found(&support), 1);
        assert_eq!(found(&sales), 0, "another context's example stays out");
        store.remove_example(&example.resource).unwrap();
        assert_eq!(found(&support), 0);
    }
}
