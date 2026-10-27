//! The knowledge index (`knowledge-index.db`): what repository sources and verified notes say,
//! split into Markdown sections and searched by relevance. Full-text search (SQLite FTS5 with
//! BM25, accents folded) always works; with an embeddings endpoint configured, each section
//! also gets a vector and both rankings are fused, so a request finds a passage that says the
//! same thing in other words.
//!
//! The index is a cache of sources the map already authorizes: it grants nothing. A repository
//! is indexed when it is read (or refreshed by hand), and only its files' sizes and times are
//! compared on each read, so an unchanged checkout costs one directory walk.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

/// The index's file in the data directory.
pub const FILE: &str = "knowledge-index.db";
/// The documents of verified notes; never a resource ID, which starts with a letter.
pub const NOTES: &str = "@notes";
/// The requests of answers the user approved as examples.
pub const EXAMPLES: &str = "@examples";
pub const QUESTIONS: &str = "@questions";
pub fn feedback_source(question: &str) -> String {
    format!("@feedback:{question}")
}
/// Characters of one indexed section.
const SECTION: usize = 1500;
/// Files of one repository source, and the size of one file.
const MAX_FILES: usize = 5000;
const MAX_FILE: u64 = 1_000_000;
/// File types a repository source reads.
const FORMATS: [&str; 9] = [
    "md", "markdown", "txt", "rst", "adoc", "json", "toml", "yaml", "yml",
];
/// Folders a repository source never reads.
const SKIPPED: [&str; 6] = ["node_modules", "target", "vendor", "dist", "build", "bin"];
/// Sections embedded per search when some still lack a vector, so a large new source is
/// embedded over a few searches instead of delaying one answer.
pub const EMBED_PER_SEARCH: usize = 256;

/// A fact the user (or an agent on their behalf) verified, kept to answer with.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Note {
    /// Assigned when the note is created; `[a-z0-9-]`, at most 64 characters.
    #[serde(default)]
    pub id: String,
    pub title: String,
    pub text: String,
    /// IDs of the map's contexts the note belongs to; empty = every context.
    #[serde(default)]
    pub contexts: Vec<String>,
    /// Words or phrases that should find it.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Who wrote it: `user` (the app or `pta`), `agent` (an agent through the MCP server),
    /// `answer` (an answer the user approved) or `import`.
    #[serde(default = "user")]
    pub origin: String,
    /// What it is about, when it comes from somewhere: a Teams message, a work item, a page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}
fn user() -> String {
    "user".into()
}
impl Default for Note {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            text: String::new(),
            contexts: Vec::new(),
            keywords: Vec::new(),
            origin: user(),
            reference: None,
            created_at: 0,
            updated_at: 0,
        }
    }
}
pub const ORIGINS: [&str; 4] = ["user", "agent", "answer", "import"];

impl Note {
    /// Check a note before it is saved; `contexts` are the map's context IDs.
    pub fn validate(&self, contexts: &BTreeSet<&str>) -> Result<()> {
        let plain = |text: &str, max: usize| {
            text.chars().count() <= max
                && !text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
        };
        ensure!(
            !self.title.trim().is_empty() && plain(&self.title, 200),
            "a note needs a title of at most 200 characters"
        );
        ensure!(
            !self.text.trim().is_empty() && plain(&self.text, 20_000),
            "a note needs a text of at most 20000 characters"
        );
        ensure!(
            self.keywords.len() <= 30 && self.keywords.iter().all(|k| plain(k, 80)),
            "a note has at most 30 keywords of at most 80 characters"
        );
        ensure!(
            self.contexts.len() <= 20
                && self.contexts.iter().all(|c| contexts.contains(c.as_str())),
            "a note's contexts must exist in the knowledge map"
        );
        ensure!(
            ORIGINS.contains(&self.origin.as_str()),
            "unknown note origin"
        );
        if let Some(reference) = &self.reference {
            ensure!(plain(reference, 2000), "invalid note reference");
        }
        Ok(())
    }
}

/// A section that matched a search.
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    /// The resource ID, or `@notes`.
    pub source: String,
    /// The file path in the repository, or the note ID.
    pub key: String,
    pub title: String,
    pub heading: String,
    pub text: String,
    /// Higher is more relevant (fused rank).
    pub score: f64,
}

/// What one source has in the index.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SourceStatus {
    pub documents: usize,
    pub sections: usize,
    pub embedded: usize,
    /// When it was last compared with its files (Unix seconds).
    pub synced_at: Option<i64>,
}

/// What a repository sync changed.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Sync {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    /// Files left out because the source reached its limit.
    pub skipped: usize,
}

pub struct Index {
    db: Mutex<Connection>,
}

impl Index {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS notes(id TEXT PRIMARY KEY,payload TEXT NOT NULL,updated_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS question_feedback(resource TEXT PRIMARY KEY,question_id TEXT NOT NULL,payload TEXT NOT NULL,updated_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS feedback_question ON question_feedback(question_id);
            CREATE TABLE IF NOT EXISTS examples(resource TEXT PRIMARY KEY,request TEXT NOT NULL,answer TEXT NOT NULL,context TEXT,updated_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS documents(id INTEGER PRIMARY KEY,source TEXT NOT NULL,key TEXT NOT NULL,title TEXT NOT NULL,stamp TEXT NOT NULL,contexts TEXT NOT NULL DEFAULT '[]',UNIQUE(source,key));
            CREATE TABLE IF NOT EXISTS chunks(id INTEGER PRIMARY KEY,document INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,heading TEXT NOT NULL,text TEXT NOT NULL,embedding BLOB);
            CREATE INDEX IF NOT EXISTS chunks_document ON chunks(document);
            CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(heading,text,content='chunks',content_rowid='id',tokenize='unicode61 remove_diacritics 2');
            CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
              INSERT INTO chunks_fts(rowid,heading,text) VALUES (new.id,new.heading,new.text);
            END;
            CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
              INSERT INTO chunks_fts(chunks_fts,rowid,heading,text) VALUES ('delete',old.id,old.heading,old.text);
            END;",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self { db: Mutex::new(db) })
    }

    /// Questions and their owner-associated requests share the existing hybrid index.
    pub fn sync_questions(&self, questions: &[crate::questions::Question]) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let keys: Vec<String> = tx
            .prepare("SELECT key FROM documents WHERE source=?1")?
            .query_map([QUESTIONS], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for key in keys {
            if !questions.iter().any(|q| q.id == key) {
                tx.execute(
                    "DELETE FROM documents WHERE source=?1 AND key=?2",
                    params![QUESTIONS, key],
                )?;
            }
        }
        let feedback_questions: Vec<String> = tx
            .prepare("SELECT DISTINCT question_id FROM question_feedback")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in feedback_questions {
            if !questions.iter().any(|q| q.id == id) {
                tx.execute("DELETE FROM question_feedback WHERE question_id=?1", [&id])?;
                tx.execute(
                    "DELETE FROM documents WHERE source=?1",
                    [feedback_source(&id)],
                )?;
            }
        }
        for q in questions {
            let stamp = serde_json::to_string(q)?;
            let current: Option<String> = tx
                .query_row(
                    "SELECT stamp FROM documents WHERE source=?1 AND key=?2",
                    params![QUESTIONS, q.id],
                    |r| r.get(0),
                )
                .optional()?;
            if current.as_deref() != Some(&stamp) {
                write_document(
                    &tx,
                    QUESTIONS,
                    &q.id,
                    &q.text,
                    &stamp,
                    "[]",
                    &format!("{}\n{}", q.text, q.examples.join("\n")),
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn save_evaluation(&self, value: &crate::questions::AnswerEvaluation) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute(
            "DELETE FROM documents WHERE key=?1 AND source LIKE '@feedback:%'",
            [&value.resource],
        )?;
        tx.execute("INSERT OR REPLACE INTO question_feedback(resource,question_id,payload,updated_at) VALUES (?1,?2,?3,?4)", params![value.resource, value.question_id, serde_json::to_string(value)?, now])?;
        write_document(
            &tx,
            &feedback_source(&value.question_id),
            &value.resource,
            "Answer evaluation",
            &now.to_string(),
            "[]",
            &value.request,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_evaluation(&self, resource: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "DELETE FROM question_feedback WHERE resource=?1",
            [resource],
        )?;
        db.execute(
            "DELETE FROM documents WHERE key=?1 AND source LIKE '@feedback:%'",
            [resource],
        )?;
        Ok(())
    }

    /// Rank evaluations within the matched question, then include recent judgments too:
    /// synonyms without embeddings must still benefit from the owner's corrections.
    pub fn evaluations(
        &self,
        question: &str,
        query: &str,
        vector: Option<&[f32]>,
        allowed: &[String],
    ) -> Result<Vec<crate::questions::AnswerEvaluation>> {
        let source = feedback_source(question);
        let ranked = self.search(&[&source], query, None, vector, 8)?;
        let db = self.db.lock().unwrap();
        let mut found: Vec<crate::questions::AnswerEvaluation> = db.prepare("SELECT payload FROM question_feedback WHERE question_id=?1 ORDER BY updated_at DESC,resource LIMIT 100")?.query_map([question], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().filter_map(|p| serde_json::from_str(&p).ok()).filter(|v: &crate::questions::AnswerEvaluation| v.sources.iter().all(|s| allowed.contains(s))).collect();
        found.sort_by_key(|v| {
            (
                v.rating != "bad",
                ranked
                    .iter()
                    .position(|h| h.key == v.resource)
                    .unwrap_or(100),
            )
        });
        let mut selected = Vec::new();
        for rating in ["bad", "good"] {
            selected.extend(found.iter().filter(|v| v.rating == rating).take(4).cloned());
        }
        for value in found {
            if selected.len() == 8 {
                break;
            }
            if !selected.iter().any(|v| v.resource == value.resource) {
                selected.push(value);
            }
        }
        Ok(selected)
    }

    /// Check every rejected answer, including older evaluations outside the prompt budget.
    pub fn is_rejected_answer(
        &self,
        question: &str,
        answer: &str,
        allowed: &[String],
    ) -> Result<bool> {
        let wanted = crate::questions::normalize(&crate::evidence::unlinked(answer));
        let db = self.db.lock().unwrap();
        let mut statement = db.prepare("SELECT payload FROM question_feedback WHERE question_id=?1 AND json_extract(payload,'$.rating')='bad'")?;
        let mut rows = statement.query([question])?;
        while let Some(row) = rows.next()? {
            let value: crate::questions::AnswerEvaluation =
                serde_json::from_str(&row.get::<_, String>(0)?)?;
            if value.sources.iter().all(|s| allowed.contains(s))
                && crate::questions::normalize(&value.answer) == wanted
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Bring a repository source up to date with its checkout: new and changed files are
    /// indexed again, removed ones dropped. Paths stay inside the checkout.
    pub fn sync_repository(&self, source: &str, root: &Path) -> Result<Sync> {
        let root = std::fs::canonicalize(root)?;
        ensure!(
            root.join(".git").exists(),
            "knowledge root must be a Git checkout"
        );
        let (files, skipped) = walk(&root)?;
        let mut db = self.db.lock().unwrap();
        let known: BTreeMap<String, String> = db
            .prepare("SELECT key,stamp FROM documents WHERE source=?1")?
            .query_map([source], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut summary = Sync {
            skipped,
            ..Default::default()
        };
        let tx = db.transaction()?;
        for (key, stamp) in &files {
            match known.get(key) {
                Some(old) if old == stamp => {
                    summary.unchanged += 1;
                    continue;
                }
                Some(_) => summary.updated += 1,
                None => summary.added += 1,
            }
            let full = root.join(key);
            let text = match std::fs::read_to_string(&full) {
                Ok(text) => text,
                // A file that is not UTF-8 text is not knowledge.
                Err(_) => {
                    tx.execute(
                        "DELETE FROM documents WHERE source=?1 AND key=?2",
                        params![source, key],
                    )?;
                    continue;
                }
            };
            write_document(&tx, source, key, &title(key, &text), stamp, "[]", &text)?;
        }
        for key in known.keys().filter(|k| !files.contains_key(*k)) {
            tx.execute(
                "DELETE FROM documents WHERE source=?1 AND key=?2",
                params![source, key],
            )?;
            summary.removed += 1;
        }
        tx.execute(
            "INSERT INTO meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![
                format!("synced:{source}"),
                chrono::Utc::now().timestamp().to_string()
            ],
        )?;
        tx.commit()?;
        Ok(summary)
    }

    /// Forget a source's documents (a source removed from the map).
    pub fn forget(&self, source: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute("DELETE FROM documents WHERE source=?1", [source])?;
        db.execute(
            "DELETE FROM meta WHERE key=?1",
            [format!("synced:{source}")],
        )?;
        Ok(())
    }

    /// Create a note (empty `id`) or replace one; returns it as saved.
    pub fn save_note(&self, mut note: Note) -> Result<Note> {
        let now = chrono::Utc::now().timestamp();
        let mut db = self.db.lock().unwrap();
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
            let existing: Option<String> = db
                .query_row("SELECT payload FROM notes WHERE id=?1", [&note.id], |r| {
                    r.get(0)
                })
                .optional()?;
            let existing: Note = serde_json::from_str(&existing.context("unknown note")?)?;
            note.created_at = existing.created_at;
        }
        note.updated_at = now;
        let tx = db.transaction()?;
        tx.execute(
            "INSERT INTO notes(id,payload,updated_at) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload,updated_at=excluded.updated_at",
            params![note.id, serde_json::to_string(&note)?, now],
        )?;
        // The keywords are searched with the text; the note is one document.
        let body = if note.keywords.is_empty() {
            note.text.clone()
        } else {
            format!("{}\n\n{}", note.text, note.keywords.join(", "))
        };
        write_document(
            &tx,
            NOTES,
            &note.id,
            &note.title,
            &now.to_string(),
            &serde_json::to_string(&note.contexts)?,
            &body,
        )?;
        tx.commit()?;
        Ok(note)
    }

    pub fn note(&self, id: &str) -> Result<Option<Note>> {
        let db = self.db.lock().unwrap();
        let payload: Option<String> = db
            .query_row("SELECT payload FROM notes WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        payload
            .map(|p| serde_json::from_str(&p).map_err(Into::into))
            .transpose()
    }

    pub fn remove_note(&self, id: &str) -> Result<bool> {
        let db = self.db.lock().unwrap();
        let removed = db.execute("DELETE FROM notes WHERE id=?1", [id])? > 0;
        db.execute(
            "DELETE FROM documents WHERE source=?1 AND key=?2",
            params![NOTES, id],
        )?;
        Ok(removed)
    }

    /// Notes, newest first; `context` keeps those in it (and those in every context).
    pub fn notes(&self, context: Option<&str>, limit: usize) -> Result<Vec<Note>> {
        let db = self.db.lock().unwrap();
        let notes: Vec<Note> = db
            .prepare("SELECT payload FROM notes ORDER BY updated_at DESC")?
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|p| p.ok().and_then(|p| serde_json::from_str(&p).ok()))
            .filter(|n: &Note| {
                context.is_none_or(|c| n.contexts.is_empty() || n.contexts.iter().any(|x| x == c))
            })
            .take(limit)
            .collect();
        Ok(notes)
    }

    /// Keep an answer the user approved as an example for similar requests (its request is
    /// searched; the whole answer is returned).
    pub fn save_example(
        &self,
        resource: &str,
        request: &str,
        answer: &str,
        context: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute(
            "INSERT INTO examples(resource,request,answer,context,updated_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(resource) DO UPDATE SET request=excluded.request,answer=excluded.answer,context=excluded.context,updated_at=excluded.updated_at",
            params![resource, request, answer, context, now],
        )?;
        let contexts = serde_json::to_string(&context.map(|c| vec![c]).unwrap_or_default())?;
        write_document(
            &tx,
            EXAMPLES,
            resource,
            "example",
            &now.to_string(),
            &contexts,
            request,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_example(&self, resource: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute("DELETE FROM examples WHERE resource=?1", [resource])?;
        db.execute(
            "DELETE FROM documents WHERE source=?1 AND key=?2",
            params![EXAMPLES, resource],
        )?;
        Ok(())
    }

    /// Approved examples for `query`, as (request, answer): only those whose request shares at
    /// least half of the query's meaningful words (two at least, when it has two), so a single
    /// common word does not bring in an unrelated answer. `context` keeps its examples and the
    /// general ones.
    pub fn examples(
        &self,
        query: &str,
        context: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let wanted = terms(query);
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let contexts = context.map(|c| vec![c.to_owned()]);
        let hits = self.search(&[EXAMPLES], query, contexts.as_deref(), None, limit * 4)?;
        let db = self.db.lock().unwrap();
        let mut out = Vec::new();
        for hit in hits {
            let shared = terms(&hit.text).intersection(&wanted).count();
            if shared * 2 < wanted.len() || shared < wanted.len().min(2) {
                continue;
            }
            let found: Option<(String, String)> = db
                .query_row(
                    "SELECT request,answer FROM examples WHERE resource=?1",
                    [&hit.key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some(example) = found
                && !out.contains(&example)
            {
                out.push(example);
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// What each source has in the index (`@notes` for the notes).
    pub fn status(&self) -> Result<BTreeMap<String, SourceStatus>> {
        let db = self.db.lock().unwrap();
        let mut out: BTreeMap<String, SourceStatus> = BTreeMap::new();
        let mut rows = db.prepare(
            "SELECT d.source,COUNT(DISTINCT d.id),COUNT(c.id),COUNT(c.embedding) FROM documents d LEFT JOIN chunks c ON c.document=d.id GROUP BY d.source",
        )?;
        for row in rows.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })? {
            let (source, documents, sections, embedded) = row?;
            out.insert(
                source,
                SourceStatus {
                    documents: documents as usize,
                    sections: sections as usize,
                    embedded: embedded as usize,
                    synced_at: None,
                },
            );
        }
        let mut synced = db.prepare("SELECT key,value FROM meta WHERE key LIKE 'synced:%'")?;
        for row in synced.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (key, value) = row?;
            out.entry(key.trim_start_matches("synced:").to_owned())
                .or_default()
                .synced_at = value.parse().ok();
        }
        Ok(out)
    }

    /// The most relevant sections of `sources` (`@notes` included by name) for `query`, by
    /// full-text rank, fused with vector similarity when `query_vector` is given. Notes and
    /// examples are kept only when they belong to every context or share one with `contexts`
    /// (when given).
    pub fn search(
        &self,
        sources: &[&str],
        query: &str,
        contexts: Option<&[String]>,
        query_vector: Option<&[f32]>,
        limit: usize,
    ) -> Result<Vec<Hit>> {
        if sources.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let db = self.db.lock().unwrap();
        let placeholders = vec!["?"; sources.len()].join(",");
        let keep = |source: &str, note_contexts: &str| {
            (source != NOTES && source != EXAMPLES)
                || contexts.is_none_or(|wanted| {
                    let found: Vec<String> =
                        serde_json::from_str(note_contexts).unwrap_or_default();
                    found.is_empty() || found.iter().any(|c| wanted.contains(c))
                })
        };
        type Row = (i64, String, String, String, String, String, String);
        let read = |r: &rusqlite::Row| -> rusqlite::Result<Row> {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        };
        let candidates = limit * 4;
        let mut rows: BTreeMap<i64, Row> = BTreeMap::new();
        let mut text_rank: Vec<i64> = Vec::new();
        if let Some(expression) = fts_query(query) {
            let sql = format!(
                "SELECT c.id,d.source,d.key,d.title,c.heading,c.text,d.contexts FROM chunks_fts JOIN chunks c ON c.id=chunks_fts.rowid JOIN documents d ON d.id=c.document WHERE chunks_fts MATCH ? AND d.source IN ({placeholders}) ORDER BY bm25(chunks_fts,2.0,1.0) LIMIT {candidates}"
            );
            let mut values: Vec<&str> = vec![expression.as_str()];
            values.extend(sources);
            let mut statement = db.prepare(&sql)?;
            for row in statement.query_map(params_from_iter(values), read)? {
                let row = row?;
                if keep(&row.1, &row.6) {
                    text_rank.push(row.0);
                    rows.insert(row.0, row);
                }
            }
        }
        let mut vector_rank: Vec<i64> = Vec::new();
        if let Some(wanted) = query_vector {
            let sql = format!(
                "SELECT c.id,c.embedding,d.source,d.contexts FROM chunks c JOIN documents d ON d.id=c.document WHERE c.embedding IS NOT NULL AND d.source IN ({placeholders})"
            );
            let mut scored: Vec<(f32, i64)> = Vec::new();
            let mut statement = db.prepare(&sql)?;
            for row in statement.query_map(params_from_iter(sources), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })? {
                let (id, blob, source, note_contexts) = row?;
                if !keep(&source, &note_contexts) {
                    continue;
                }
                if let Some(similarity) = cosine(wanted, &vector(&blob)) {
                    scored.push((similarity, id));
                }
            }
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            vector_rank = scored
                .into_iter()
                .take(candidates)
                .map(|(_, id)| id)
                .collect();
            let missing: Vec<i64> = vector_rank
                .iter()
                .copied()
                .filter(|id| !rows.contains_key(id))
                .collect();
            for id in missing {
                let row = db.query_row(
                    "SELECT c.id,d.source,d.key,d.title,c.heading,c.text,d.contexts FROM chunks c JOIN documents d ON d.id=c.document WHERE c.id=?1",
                    [id],
                    read,
                )?;
                rows.insert(id, row);
            }
        }
        // Reciprocal rank fusion: a section near the top of either ranking comes first.
        let mut fused: BTreeMap<i64, f64> = BTreeMap::new();
        for ranking in [&text_rank, &vector_rank] {
            for (rank, id) in ranking.iter().enumerate() {
                *fused.entry(*id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
            }
        }
        let mut ordered: Vec<(f64, i64)> = fused.into_iter().map(|(id, s)| (s, id)).collect();
        ordered.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        Ok(ordered
            .into_iter()
            .take(limit)
            .filter_map(|(score, id)| {
                let (_, source, key, title, heading, text, _) = rows.remove(&id)?;
                Some(Hit {
                    source,
                    key,
                    title,
                    heading,
                    text,
                    score,
                })
            })
            .collect())
    }

    /// The model whose vectors the index holds; vectors of another model are dropped.
    pub fn use_embeddings(&self, model: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        let current: Option<String> = db
            .query_row(
                "SELECT value FROM meta WHERE key='embedding_model'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if current.as_deref() != Some(model) {
            db.execute("UPDATE chunks SET embedding=NULL", [])?;
            db.execute(
                "INSERT INTO meta(key,value) VALUES ('embedding_model',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [model],
            )?;
        }
        Ok(())
    }

    /// Sections of `sources` without a vector yet, at most `limit`.
    pub fn unembedded(&self, sources: &[&str], limit: usize) -> Result<Vec<(i64, String)>> {
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        let db = self.db.lock().unwrap();
        let sql = format!(
            "SELECT c.id,d.title,c.heading,c.text FROM chunks c JOIN documents d ON d.id=c.document WHERE c.embedding IS NULL AND d.source IN ({}) ORDER BY c.id LIMIT {limit}",
            vec!["?"; sources.len()].join(",")
        );
        let mut statement = db.prepare(&sql)?;
        let rows = statement
            .query_map(params_from_iter(sources), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    format!(
                        "{}\n{}\n{}",
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?
                    ),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn save_embeddings(&self, vectors: &[(i64, Vec<f32>)]) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        for (id, values) in vectors {
            let blob: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            tx.execute(
                "UPDATE chunks SET embedding=?1 WHERE id=?2",
                params![blob, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// Replace one document and its sections.
fn write_document(
    tx: &rusqlite::Transaction,
    source: &str,
    key: &str,
    title: &str,
    stamp: &str,
    contexts: &str,
    text: &str,
) -> Result<()> {
    tx.execute(
        "DELETE FROM documents WHERE source=?1 AND key=?2",
        params![source, key],
    )?;
    tx.execute(
        "INSERT INTO documents(source,key,title,stamp,contexts) VALUES (?1,?2,?3,?4,?5)",
        params![source, key, title, stamp, contexts],
    )?;
    let document = tx.last_insert_rowid();
    let lower = title.to_lowercase();
    for (path, section) in sections(text) {
        // The path starts with the document's own heading when its title is that heading.
        let path = path
            .strip_prefix(lower.as_str())
            .map_or(path.as_str(), |rest| rest.trim_start_matches(" / "));
        let heading = if path.is_empty() {
            title.to_owned()
        } else {
            format!("{title} / {path}")
        };
        tx.execute(
            "INSERT INTO chunks(document,heading,text) VALUES (?1,?2,?3)",
            params![document, heading, section],
        )?;
    }
    Ok(())
}

/// A text as sections of at most `SECTION` characters, each with its heading path.
fn sections(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (heading, part) in crate::knowledge::parts(text, SECTION) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // A single paragraph longer than a section is cut into sections.
        let chars: Vec<char> = part.chars().collect();
        for piece in chars.chunks(SECTION) {
            out.push((heading.clone(), piece.iter().collect()));
        }
    }
    out
}

/// A document's title: its first Markdown heading, or its file name.
fn title(key: &str, text: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix("# ").map(str::trim))
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(200).collect())
        .unwrap_or_else(|| key.to_owned())
}

/// The files a repository source reads, by path relative to `root`, with a stamp of their size
/// and modification time; and how many were left out over the limit.
fn walk(root: &Path) -> Result<(BTreeMap<String, String>, usize)> {
    let mut files = BTreeMap::new();
    let mut skipped = 0;
    let mut pending: Vec<PathBuf> = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            // Never follow links: a link could lead outside the checkout.
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                if !SKIPPED.contains(&name.as_ref()) {
                    pending.push(entry.path());
                }
                continue;
            }
            let path = entry.path();
            let format = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_lowercase)
                .unwrap_or_default();
            if !meta.is_file() || meta.len() > MAX_FILE || !FORMATS.contains(&format.as_str()) {
                continue;
            }
            if files.len() >= MAX_FILES {
                skipped += 1;
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            files.insert(
                relative.to_string_lossy().replace('\\', "/"),
                format!("{}:{modified}", meta.len()),
            );
        }
    }
    Ok((files, skipped))
}

/// Words that say nothing about a topic, in Spanish and English.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "had", "her", "was",
    "one", "our", "out", "has", "have", "how", "what", "when", "where", "which", "who", "why",
    "with", "this", "that", "from", "does", "did", "about", "into", "your", "there", "their",
    "los", "las", "del", "que", "por", "para", "con", "una", "uno", "unos", "unas", "sus", "les",
    "como", "pero", "mas", "este", "esta", "esto", "estos", "estas", "ese", "esa", "eso", "entre",
    "cuando", "muy", "sin", "sobre", "tambien", "hasta", "hay", "donde", "quien", "desde", "todo",
    "todos", "nos", "durante", "ante", "ellos", "ellas", "antes", "algunos", "otro", "otra",
    "otros", "otras", "tanto", "mucho", "muchos", "nada", "cual", "cuales", "poco", "algo", "mis",
    "tus", "nuestro", "nuestra", "segun", "cual", "son", "ser", "fue", "era", "esta", "estan",
    "tiene", "tienen", "puedo", "puede", "hacer", "hace", "favor",
];

/// Fold accents and case, so «Conciliación» and «conciliacion» are the same word.
pub fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}

/// The meaningful words of a text, accent-folded, a longer one by its stem.
fn terms(text: &str) -> BTreeSet<String> {
    fold(text)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !STOPWORDS.contains(w))
        .map(|w| {
            if w.chars().count() >= 5 {
                w.chars().take(w.chars().count() - 1).collect()
            } else {
                w.to_owned()
            }
        })
        .collect()
}

/// The words of a request as an FTS5 expression: each meaningful word, a longer one also by
/// its stem («pagos» finds «pago»), any of them; `None` when nothing is left to search.
fn fts_query(query: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in fold(query).split(|c: char| !c.is_alphanumeric()) {
        if word.chars().count() < 3 || STOPWORDS.contains(&word) {
            continue;
        }
        // Only letters and digits reach the expression: no FTS5 syntax from the request.
        let term = if word.chars().count() >= 5 {
            let stem: String = word.chars().take(word.chars().count() - 1).collect();
            format!("{stem}*")
        } else {
            format!("\"{word}\"")
        };
        if !terms.contains(&term) {
            terms.push(term);
        }
        if terms.len() >= 24 {
            break;
        }
    }
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn vector(blob: &[u8]) -> Vec<f32> {
    blob.as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    (na > 0.0 && nb > 0.0).then(|| dot / (na.sqrt() * nb.sqrt()))
}

/// An OpenAI-compatible embeddings endpoint (`POST {url}/embeddings` with `{model, input}`),
/// such as OpenAI, Azure OpenAI's compatible route, Ollama or LM Studio.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Embeddings {
    /// Base URL ending before `/embeddings`: HTTPS, or HTTP only on this computer (loopback).
    pub url: String,
    pub model: String,
    /// A `secret://` reference mapped in `[secrets]` to the API key; none for a local server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
}
/// Settings of the knowledge index (`[knowledge_index]`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Vectors for semantic search; without them, full-text search alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embeddings: Option<Embeddings>,
}
impl Settings {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn validate(&self) -> Result<()> {
        if let Some(e) = &self.embeddings {
            let url = url::Url::parse(&e.url)?;
            let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
            ensure!(
                (url.scheme() == "https" || (url.scheme() == "http" && local))
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "the embeddings URL must be HTTPS (or HTTP on this computer), without credentials"
            );
            ensure!(
                !e.model.is_empty()
                    && e.model.len() <= 120
                    && e.model
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.:/@".contains(c)),
                "invalid embeddings model"
            );
            if let Some(reference) = &e.secret_ref {
                ensure!(
                    reference.starts_with("secret://"),
                    "invalid embeddings credential reference"
                );
            }
        }
        Ok(())
    }
}
impl Embeddings {
    /// What identifies the vectors: another endpoint or model makes them incomparable.
    pub fn identity(&self) -> String {
        format!("{} {}", self.url.trim_end_matches('/'), self.model)
    }
    /// One vector per text, in order. The texts are already authorized for external
    /// processing; the response is bounded.
    pub async fn embed(
        &self,
        client: &reqwest::Client,
        key: Option<&str>,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        // Shared accounts cannot address operator-local services through HTTPS
        // aliases or DNS rebinding. Reuse the public source client and IP pinning.
        let public_client;
        let client = if crate::security::account_only() {
            public_client = crate::knowledge::public_client(&self.url).await?;
            &public_client
        } else {
            client
        };
        #[derive(Deserialize)]
        struct Item {
            index: usize,
            embedding: Vec<f32>,
        }
        #[derive(Deserialize)]
        struct Reply {
            data: Vec<Item>,
        }
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(64) {
            let mut request = client
                .post(format!("{}/embeddings", self.url.trim_end_matches('/')))
                .timeout(std::time::Duration::from_secs(60))
                .json(&serde_json::json!({"model": self.model, "input": batch}));
            if let Some(key) = key {
                request = request.bearer_auth(key);
            }
            let response = request.send().await?;
            ensure!(
                response.status().is_success(),
                "embeddings request failed: HTTP {}",
                response.status().as_u16()
            );
            let bytes = crate::adapters::bounded_bytes(response, 16_000_000).await?;
            let mut reply: Reply = serde_json::from_slice(&bytes)?;
            ensure!(
                reply.data.len() == batch.len(),
                "embeddings reply does not match the request"
            );
            reply.data.sort_by_key(|i| i.index);
            out.extend(reply.data.into_iter().map(|i| i.embedding));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/x")).unwrap();
        std::fs::write(
            dir.path().join("docs/cobros.md"),
            "# Cobros\n\nProceso general.\n\n## Conciliación bancaria\n\nLa conciliación de pagos se ejecuta cada noche con el archivo del banco.\n\n## Reintentos\n\nUn cobro rechazado se reintenta tres veces.",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("README.md"),
            "# Guía\n\nDespliegue del servicio.",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("node_modules/x/readme.md"),
            "# Dependencia\n\nconciliación",
        )
        .unwrap();
        std::fs::write(dir.path().join("logo.png"), [0u8, 1, 2]).unwrap();
        dir
    }

    #[test]
    fn a_repository_is_indexed_incrementally_and_searched_by_its_words() {
        let repo = checkout();
        let data = tempfile::tempdir().unwrap();
        let index = Index::open(&data.path().join(FILE)).unwrap();
        let first = index.sync_repository("handbook", repo.path()).unwrap();
        assert_eq!((first.added, first.updated, first.removed), (2, 0, 0));
        // Accents and plural forms do not matter; the best section comes first.
        let hits = index
            .search(
                &["handbook"],
                "¿Cuándo se ejecuta la conciliacion de los pagos?",
                None,
                None,
                3,
            )
            .unwrap();
        assert_eq!(hits[0].key, "docs/cobros.md");
        assert_eq!(hits[0].heading, "Cobros / conciliación bancaria");
        assert!(hits[0].text.contains("cada noche"));
        // Another source's sections are not searched; dependencies are never indexed.
        assert!(
            index
                .search(&["other"], "conciliacion", None, None, 3)
                .unwrap()
                .is_empty()
        );
        assert!(hits.iter().all(|h| !h.key.contains("node_modules")));
        // Unchanged files are not read again; a changed one is, a removed one is dropped.
        let second = index.sync_repository("handbook", repo.path()).unwrap();
        assert_eq!((second.unchanged, second.updated), (2, 0));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            repo.path().join("README.md"),
            "# Guía\n\nRotación de certificados del servicio.",
        )
        .unwrap();
        std::fs::remove_file(repo.path().join("docs/cobros.md")).unwrap();
        let third = index.sync_repository("handbook", repo.path()).unwrap();
        assert_eq!((third.updated, third.removed), (1, 1));
        assert!(
            index
                .search(&["handbook"], "conciliacion", None, None, 3)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            index
                .search(&["handbook"], "certificados", None, None, 3)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(index.status().unwrap()["handbook"].documents, 1);
        index.forget("handbook").unwrap();
        assert!(!index.status().unwrap().contains_key("handbook"));
    }

    #[test]
    fn notes_are_found_by_text_and_keywords_within_their_contexts() {
        let data = tempfile::tempdir().unwrap();
        let index = Index::open(&data.path().join(FILE)).unwrap();
        let contexts = BTreeSet::from(["cobros", "pagos"]);
        let note = Note {
            title: "Hotfix de producción".into(),
            text: "Un hotfix de producción se registra como Bug Desarrollo, no como HU.".into(),
            contexts: vec!["cobros".into()],
            keywords: vec!["incidente".into()],
            ..Default::default()
        };
        note.validate(&contexts).unwrap();
        let saved = index.save_note(note).unwrap();
        assert!(!saved.id.is_empty() && saved.created_at > 0);
        let general = index
            .save_note(Note {
                title: "Horario".into(),
                text: "El soporte atiende de 9 a 18.".into(),
                ..Default::default()
            })
            .unwrap();
        // The keyword finds it; a context keeps its notes and the general ones.
        let found = index
            .search(&[NOTES], "¿Cómo registro un incidente?", None, None, 5)
            .unwrap();
        assert_eq!(found[0].key, saved.id);
        let cobros = vec!["cobros".to_owned()];
        let pagos = vec!["pagos".to_owned()];
        assert_eq!(
            index
                .search(&[NOTES], "hotfix", Some(&cobros), None, 5)
                .unwrap()
                .len(),
            1
        );
        assert!(
            index
                .search(&[NOTES], "hotfix", Some(&pagos), None, 5)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            index
                .search(&[NOTES], "soporte", Some(&pagos), None, 5)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            index.notes(Some("pagos"), 10).unwrap(),
            vec![general.clone()]
        );
        // Editing keeps its creation date; removing drops it from the search.
        let mut edited = saved.clone();
        edited.text = "Un hotfix se registra como Bug Desarrollo vinculado al incidente.".into();
        let edited = index.save_note(edited).unwrap();
        assert_eq!(edited.created_at, saved.created_at);
        assert!(
            index
                .search(&[NOTES], "HU", None, None, 5)
                .unwrap()
                .is_empty()
        );
        assert!(index.remove_note(&saved.id).unwrap());
        assert!(
            index
                .search(&[NOTES], "hotfix", None, None, 5)
                .unwrap()
                .is_empty()
        );
        // Unknown contexts, empty texts and control characters are refused.
        let mut bad = general;
        bad.contexts = vec!["otro".into()];
        assert!(bad.validate(&contexts).is_err());
        bad.contexts.clear();
        bad.text = "  ".into();
        assert!(bad.validate(&contexts).is_err());
        bad.text = "a\u{7}".into();
        assert!(bad.validate(&contexts).is_err());
    }

    #[test]
    fn vectors_find_what_other_words_say_and_change_with_the_model() {
        let data = tempfile::tempdir().unwrap();
        let index = Index::open(&data.path().join(FILE)).unwrap();
        index.use_embeddings("a").unwrap();
        for (title, text) in [("Uno", "alfa"), ("Dos", "beta")] {
            index
                .save_note(Note {
                    title: title.into(),
                    text: text.into(),
                    ..Default::default()
                })
                .unwrap();
        }
        let pending = index.unembedded(&[NOTES], 10).unwrap();
        assert_eq!(pending.len(), 2);
        // «Dos» points the same way as the query; no word is shared.
        let vectors: Vec<(i64, Vec<f32>)> = pending
            .iter()
            .map(|(id, text)| {
                (
                    *id,
                    if text.contains("Dos") {
                        vec![0.0, 1.0]
                    } else {
                        vec![1.0, 0.0]
                    },
                )
            })
            .collect();
        index.save_embeddings(&vectors).unwrap();
        let hits = index
            .search(&[NOTES], "gamma", None, Some(&[0.1, 0.9]), 1)
            .unwrap();
        assert_eq!(hits[0].title, "Dos");
        assert_eq!(index.status().unwrap()[NOTES].embedded, 2);
        // Another model's vectors are not comparable: they are dropped.
        index.use_embeddings("b").unwrap();
        assert_eq!(index.unembedded(&[NOTES], 10).unwrap().len(), 2);
    }

    #[test]
    fn approved_examples_guide_only_similar_requests() {
        let data = tempfile::tempdir().unwrap();
        let index = Index::open(&data.path().join(FILE)).unwrap();
        index
            .save_example(
                "chats/a/messages/1",
                "¿Cómo registro un hotfix de producción?",
                "Se registra como Bug Desarrollo vinculado al incidente.",
                Some("cobros"),
            )
            .unwrap();
        index
            .save_example(
                "chats/a/messages/2",
                "¿Hay reunión mañana?",
                "Sí, a las 10.",
                None,
            )
            .unwrap();
        let found = index
            .examples(
                "¿cómo se registran los hotfix en producción?",
                Some("cobros"),
                2,
            )
            .unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].1.contains("Bug Desarrollo"));
        // One shared common word is not similar enough; another context's example stays out.
        assert!(
            index
                .examples("¿Cómo está la producción del reporte?", None, 2)
                .unwrap()
                .is_empty()
        );
        assert!(
            index
                .examples("registro hotfix producción", Some("pagos"), 2)
                .unwrap()
                .is_empty()
        );
        // Saving again replaces it; removing forgets it.
        index
            .save_example(
                "chats/a/messages/1",
                "¿Cómo registro un hotfix de producción?",
                "Como Bug.",
                None,
            )
            .unwrap();
        assert_eq!(
            index
                .examples("registro hotfix producción", None, 2)
                .unwrap()[0]
                .1,
            "Como Bug."
        );
        index.remove_example("chats/a/messages/1").unwrap();
        assert!(
            index
                .examples("registro hotfix producción", None, 2)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn requests_never_inject_search_syntax() {
        assert_eq!(fts_query("¿Qué es esto?"), None);
        assert_eq!(
            fts_query("pagos NEAR(\"x\") OR conciliación*").as_deref(),
            Some("pago* OR \"near\" OR conciliacio*")
        );
    }

    #[test]
    fn embeddings_endpoints_are_https_or_local() {
        let settings = |url: &str| Settings {
            embeddings: Some(Embeddings {
                url: url.into(),
                model: "text-embedding-3-small".into(),
                secret_ref: Some("secret://openai".into()),
            }),
        };
        assert!(settings("https://api.openai.com/v1").validate().is_ok());
        assert!(settings("http://127.0.0.1:11434/v1").validate().is_ok());
        assert!(settings("http://example.com/v1").validate().is_err());
        assert!(
            settings("https://user:pass@example.com/v1")
                .validate()
                .is_err()
        );
        assert!(Settings::default().validate().is_ok());
    }
}
