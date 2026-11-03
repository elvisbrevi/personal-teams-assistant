//! What the Worker has acknowledged of every record, in the installation-local `cloud/mirror.db`.
//! A row is pending when the content the local stores hold (`target_hash`) is not the content the
//! Worker acknowledged (`acked_hash`). Revisions are allocated here and recorded before a send, so
//! a send that is repeated after a crash carries the same revision and the same bytes.

use super::Collection;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Mutex};

/// The hash a row carries for a record that no longer exists locally (a tombstone).
pub const DELETED: &str = "deleted";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub collection: Collection,
    pub natural: String,
    pub blind: String,
    /// The highest revision used for this record, acknowledged or not.
    pub revision: u64,
    /// The highest revision the Worker is known to hold beyond ours (after a `stale`/`conflict`).
    pub floor: u64,
    pub acked_revision: u64,
    pub acked_hash: String,
    pub acked_digest: String,
    /// The content being sent at `revision`; empty when nothing is pending.
    pub target_hash: String,
    pub pending_since: Option<i64>,
    pub attempts: u32,
    pub next_at: i64,
    pub error: String,
}

impl Row {
    pub fn pending(&self) -> bool {
        !self.target_hash.is_empty()
    }
}

pub struct Ledger {
    db: Mutex<Connection>,
}

const COLUMNS: &str = "collection,natural,blind,revision,floor,acked_revision,acked_hash,acked_digest,target_hash,pending_since,attempts,next_at,error";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let name: String = r.get(0)?;
    Ok(Row {
        collection: Collection::parse(&name).ok_or(rusqlite::Error::InvalidQuery)?,
        natural: r.get(1)?,
        blind: r.get(2)?,
        revision: r.get::<_, i64>(3)? as u64,
        floor: r.get::<_, i64>(4)? as u64,
        acked_revision: r.get::<_, i64>(5)? as u64,
        acked_hash: r.get(6)?,
        acked_digest: r.get(7)?,
        target_hash: r.get(8)?,
        pending_since: r.get(9)?,
        attempts: r.get::<_, i64>(10)? as u32,
        next_at: r.get(11)?,
        error: r.get(12)?,
    })
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            crate::security::private_dir(parent)?;
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS records(
               collection TEXT NOT NULL, natural TEXT NOT NULL, blind TEXT NOT NULL,
               revision INTEGER NOT NULL DEFAULT 0, floor INTEGER NOT NULL DEFAULT 0,
               acked_revision INTEGER NOT NULL DEFAULT 0, acked_hash TEXT NOT NULL DEFAULT '',
               acked_digest TEXT NOT NULL DEFAULT '', target_hash TEXT NOT NULL DEFAULT '',
               pending_since INTEGER, attempts INTEGER NOT NULL DEFAULT 0,
               next_at INTEGER NOT NULL DEFAULT 0, error TEXT NOT NULL DEFAULT '',
               PRIMARY KEY(collection, natural));
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let _ = crate::security::protect_file(path);
        Ok(Self { db: Mutex::new(db) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn rows(&self, collection: Collection) -> Result<Vec<Row>> {
        let db = self.lock();
        let mut statement = db.prepare(&format!(
            "SELECT {COLUMNS} FROM records WHERE collection=?1 ORDER BY natural"
        ))?;
        let rows = statement.query_map([collection.name()], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&self, collection: Collection, natural: &str) -> Result<Option<Row>> {
        Ok(self
            .lock()
            .query_row(
                &format!("SELECT {COLUMNS} FROM records WHERE collection=?1 AND natural=?2"),
                params![collection.name(), natural],
                row,
            )
            .optional()?)
    }

    /// Write one row as given, replacing the earlier one.
    pub fn put(&self, r: &Row) -> Result<()> {
        self.lock().execute(
            &format!("INSERT OR REPLACE INTO records({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"),
            params![
                r.collection.name(),
                r.natural,
                r.blind,
                r.revision as i64,
                r.floor as i64,
                r.acked_revision as i64,
                r.acked_hash,
                r.acked_digest,
                r.target_hash,
                r.pending_since,
                r.attempts as i64,
                r.next_at,
                r.error
            ],
        )?;
        Ok(())
    }

    /// Write several rows in one transaction.
    pub fn put_all(&self, rows: &[Row]) -> Result<()> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        for r in rows {
            tx.execute(
                &format!("INSERT OR REPLACE INTO records({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"),
                params![
                    r.collection.name(),
                    r.natural,
                    r.blind,
                    r.revision as i64,
                    r.floor as i64,
                    r.acked_revision as i64,
                    r.acked_hash,
                    r.acked_digest,
                    r.target_hash,
                    r.pending_since,
                    r.attempts as i64,
                    r.next_at,
                    r.error
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Forget a record that was never acknowledged and no longer exists locally.
    pub fn forget(&self, collection: Collection, natural: &str) -> Result<()> {
        self.lock().execute(
            "DELETE FROM records WHERE collection=?1 AND natural=?2",
            params![collection.name(), natural],
        )?;
        Ok(())
    }

    /// Start over: everything is pending again and revisions continue above the Worker's. Used
    /// when the Worker's copy was lost or restored, and by the initial upload after a failure.
    pub fn reset_acknowledgements(&self) -> Result<()> {
        self.lock().execute(
            "UPDATE records SET acked_revision=0, acked_hash='', acked_digest='', target_hash='', pending_since=NULL, attempts=0, next_at=0, error=''",
            [],
        )?;
        Ok(())
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .lock()
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn clear_meta(&self, key: &str) -> Result<()> {
        self.lock()
            .execute("DELETE FROM meta WHERE key=?1", [key])?;
        Ok(())
    }
}

impl Ledger {
    /// Configuration records the account store holds in another version than this
    /// installation's unacknowledged change (primary mode).
    pub fn conflicts(&self) -> Result<u64> {
        Ok(self.lock().query_row(
            "SELECT COUNT(*) FROM records WHERE error=?1",
            [super::engine::CONFLICT],
            |r| r.get::<_, i64>(0).map(|n| n as u64),
        )?)
    }
}

/// Counts of the outbox, per collection and in total.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Outbox {
    pub pending: u64,
    /// Rows that cannot be sent as they are (a record larger than the Worker accepts).
    pub blocked: u64,
    /// Rows whose last attempt failed and wait to be retried.
    pub retrying: u64,
    /// Age in seconds of the oldest pending row.
    pub oldest_seconds: Option<u64>,
    pub synced: u64,
}

impl Ledger {
    pub fn outbox(&self, collection: Option<Collection>, now: i64) -> Result<Outbox> {
        let db = self.lock();
        let name = collection.map(Collection::name);
        let sql =
            |what: &str| format!("SELECT {what} FROM records WHERE (?1 IS NULL OR collection=?1)");
        let count = |condition: &str| -> rusqlite::Result<u64> {
            db.query_row(
                &format!("{} AND {condition}", sql("COUNT(*)")),
                [name],
                |r| r.get::<_, i64>(0).map(|n| n as u64),
            )
        };
        let oldest: Option<i64> = db.query_row(
            &format!("{} AND target_hash<>''", sql("MIN(pending_since)")),
            [name],
            |r| r.get(0),
        )?;
        Ok(Outbox {
            pending: count("target_hash<>''")?,
            blocked: count("target_hash<>'' AND error='too_large'")?,
            retrying: count("target_hash<>'' AND attempts>0 AND error<>'too_large'")?,
            oldest_seconds: oldest.map(|at| (now - at).max(0) as u64),
            synced: count("target_hash='' AND acked_revision>0 AND acked_hash<>'deleted'")?,
        })
    }
}
