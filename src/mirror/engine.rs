//! The mirror engine: find what changed locally, seal it and upload it in batches, and compare
//! the account store's copy with the local stores. It reads local records through `Local` and
//! reaches the Worker through `Transport`; it never writes a local store and never touches the
//! Teams pipeline. Sending a record again is always safe (the Worker's upserts are idempotent and
//! the sealed bytes of a repeated send are identical); this is unrelated to the never-retry rule
//! for Graph, whose nonces and in-flight jobs never reach this module.

use super::{
    Collection, Exported, hex_sha256,
    key::Content,
    ledger::{DELETED, Ledger, Row},
    plaintext, seal,
};
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

/// The largest sealed record the Worker accepts.
pub const SEALED_MAX: usize = 1024 * 1024;
const PAGE: usize = 500;

/// The local records a mirror reads: every finished record of a collection, not a page of them.
pub trait Local {
    fn collect(&self, collection: Collection) -> Result<Vec<Exported>>;
    /// This installation's ID in the account store, which names its own settings record.
    fn installation(&self) -> Option<String> {
        None
    }
    /// Whether this installation keeps the account's configuration in the store. An installation
    /// that takes it from its coordinator mirrors only its own history: its configuration is the
    /// coordinator's to write.
    fn configuration(&self) -> bool {
        true
    }
}

/// Why an exchange with the Worker did not answer what was asked.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Failure {
    /// Network, timeout or a server error: nothing is lost, the records wait.
    Offline,
    /// Access or the Worker refused the credentials.
    Revoked,
    /// The account's daily share is used up; behaves like being offline.
    Quota,
    /// The Worker has no Access settings yet.
    Unconfigured,
    /// Anything else the Worker refused, with its error code.
    Refused(String),
}

impl Failure {
    pub fn code(&self) -> String {
        match self {
            Failure::Offline => "offline".into(),
            Failure::Revoked => "revoked".into(),
            Failure::Quota => "quota".into(),
            Failure::Unconfigured => "unconfigured".into(),
            Failure::Refused(code) => code.clone(),
        }
    }
}

/// One sealed record as the protocol carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    pub collection: Collection,
    pub id: String,
    pub revision: u64,
    pub digest: String,
    pub schema: u32,
    pub deleted: bool,
    pub sealed: Option<Vec<u8>>,
    /// The revision this write replaces (compare-and-swap, Worker feature `cas`); `None` for a
    /// plain upsert.
    pub base: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Updated,
    Unchanged,
    Stale,
    Conflict,
    /// An atomic batch wrote nothing because another of its records was refused.
    Aborted,
}

/// One record as the Worker keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub collection: Collection,
    pub id: String,
    pub revision: u64,
    pub digest: String,
    pub schema: u32,
    pub deleted: bool,
    pub generation: u64,
    pub sealed: Option<Vec<u8>>,
    /// Whether this installation wrote the held revision (Worker feature `origin`); `None` from
    /// a Worker without it, read as this installation's.
    pub own: Option<bool>,
}

pub struct Page {
    pub records: Vec<Stored>,
    pub more: bool,
}

#[allow(async_fn_in_trait)]
pub trait Transport {
    /// Upsert 1 to 100 records; the outcomes come in the same order. An `atomic` batch is
    /// written all at once or not at all.
    async fn write(&self, records: &[Wire], atomic: bool) -> Result<Vec<Outcome>, Failure>;
    /// The records of a collection that changed after generation `since`, oldest first.
    async fn page(&self, collection: Collection, since: u64, limit: usize)
    -> Result<Page, Failure>;
    async fn get(&self, collection: Collection, id: &str) -> Result<Option<Stored>, Failure>;
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Records sent in one pass.
    pub records: usize,
    pub batch_records: usize,
    /// Sealed bytes in one request (the Worker's body limit is 4 MiB after base64).
    pub batch_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            records: 500,
            batch_records: 100,
            batch_bytes: 2_500_000,
        }
    }
}

/// A row waiting to be sent, with the content to send (none for a tombstone).
struct Pending {
    row: Row,
    value: Option<Value>,
}

/// What a pass did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Flushed {
    pub sent: u64,
    pub batches: u64,
    /// Records the Worker refused (retried later with a backoff).
    pub refused: u64,
    /// Configuration records the account store holds in another version (primary mode): they
    /// wait for a pull or for the user to choose.
    pub conflicts: u64,
    /// Round-trip milliseconds of each batch.
    pub batch_millis: Vec<u64>,
    pub failure: Option<String>,
}

pub struct Engine<'a, L: Local, T: Transport> {
    pub local: &'a L,
    pub transport: &'a T,
    pub ledger: &'a Ledger,
    pub content: &'a Content,
    /// The account store is the source of truth of the configuration (mode `primary`): its
    /// records are sent naming the revision they replace, and one the store holds in another
    /// version is a conflict instead of a revision to continue above.
    pub cas: bool,
}

/// What a row for a record that was never sent starts as.
pub(super) fn fresh(content: &Content, collection: Collection, id: &str) -> Row {
    Row {
        collection,
        blind: seal::blind_id(content, collection, id),
        natural: id.into(),
        revision: 0,
        floor: 0,
        acked_revision: 0,
        acked_hash: String::new(),
        acked_digest: String::new(),
        target_hash: String::new(),
        pending_since: None,
        attempts: 0,
        next_at: 0,
        error: String::new(),
    }
}

/// The error of a configuration row the account store holds in another version.
pub const CONFLICT: &str = "conflict";

pub(super) fn hash(id: &str, value: &Value) -> String {
    hex_sha256(&plaintext(id, value))
}

fn backoff(attempts: u32) -> i64 {
    (30i64 << attempts.min(7)).min(3600)
}

/// Point a row at the content the stores hold now, allocating the next revision when that is not
/// what it already sends or what the Worker acknowledged. True when the row changed.
fn retarget(row: &mut Row, desired: &str, now: i64) -> bool {
    if row.target_hash == desired {
        return false;
    }
    // Acknowledged already; a revision allocated for a send that never settled stays unused.
    if row.target_hash.is_empty() && row.acked_hash == desired {
        return false;
    }
    row.revision = row.revision.max(row.acked_revision).max(row.floor) + 1;
    row.target_hash = desired.into();
    row.pending_since.get_or_insert(now);
    row.attempts = 0;
    row.next_at = 0;
    row.error.clear();
    true
}

impl<L: Local, T: Transport> Engine<'_, L, T> {
    /// Compare the local stores with the ledger and record what must be sent. Returns the
    /// pending rows with their content, oldest first.
    fn sweep(&self, now: i64) -> Result<Vec<Pending>> {
        let mut pending = Vec::new();
        for collection in Collection::ALL {
            if collection.is_configuration() && !self.local.configuration() {
                continue;
            }
            let exported = self.local.collect(collection)?;
            let mut held: BTreeMap<String, Row> = self
                .ledger
                .rows(collection)?
                .into_iter()
                .map(|r| (r.natural.clone(), r))
                .collect();
            let mut changed = Vec::new();
            let mut present = BTreeSet::new();
            for Exported { id, value } in exported {
                let desired = hash(&id, &value);
                let mut row = held
                    .remove(&id)
                    .unwrap_or_else(|| fresh(self.content, collection, &id));
                if retarget(&mut row, &desired, now) {
                    changed.push(row.clone());
                }
                present.insert(id);
                if row.pending() {
                    pending.push(Pending {
                        row,
                        value: Some(value),
                    });
                }
            }
            // Rows whose record is gone: a tombstone, unless nothing was ever sent for them.
            for (natural, mut row) in held {
                if present.contains(&natural) {
                    continue;
                }
                if row.revision == 0 && row.acked_revision == 0 {
                    self.ledger.forget(collection, &natural)?;
                    continue;
                }
                if retarget(&mut row, DELETED, now) {
                    changed.push(row.clone());
                }
                if row.pending() {
                    pending.push(Pending { row, value: None });
                }
            }
            self.ledger.put_all(&changed)?;
        }
        pending.sort_by(|a, b| {
            (a.row.pending_since, a.row.collection, &a.row.natural).cmp(&(
                b.row.pending_since,
                b.row.collection,
                &b.row.natural,
            ))
        });
        Ok(pending)
    }

    /// The records that are pending now, without sending anything.
    pub fn plan(&self, now: i64) -> Result<u64> {
        Ok(self.sweep(now)?.len() as u64)
    }

    /// Find what changed and upload up to `limits.records` of it.
    pub async fn flush(&self, now: i64, limits: Limits) -> Result<Flushed> {
        let pending = self.sweep(now)?;
        let mut flushed = Flushed::default();
        let eligible: Vec<Pending> = pending
            .into_iter()
            .filter(|p| p.row.next_at <= now)
            .take(limits.records)
            .collect();
        let mut batch: Vec<(Row, Wire)> = Vec::new();
        let mut bytes = 0usize;
        let mut batches: Vec<Vec<(Row, Wire)>> = Vec::new();
        for Pending { mut row, value } in eligible {
            let base =
                (self.cas && row.collection.is_configuration()).then_some(row.acked_revision);
            let wire = match value {
                None => Wire {
                    collection: row.collection,
                    id: row.blind.clone(),
                    revision: row.revision,
                    digest: hex_sha256(b""),
                    schema: row.collection.schema(),
                    deleted: true,
                    sealed: None,
                    base,
                },
                Some(value) => {
                    let bytes = plaintext(&row.natural, &value);
                    let sealed = seal::seal(
                        self.content,
                        row.collection,
                        &row.blind,
                        row.revision,
                        &bytes,
                    )?;
                    if sealed.len() > SEALED_MAX {
                        row.error = "too_large".into();
                        row.next_at = i64::MAX;
                        self.ledger.put(&row)?;
                        continue;
                    }
                    Wire {
                        collection: row.collection,
                        id: row.blind.clone(),
                        revision: row.revision,
                        digest: hex_sha256(&sealed),
                        schema: row.collection.schema(),
                        deleted: false,
                        sealed: Some(sealed),
                        base,
                    }
                }
            };
            let size = wire.sealed.as_ref().map_or(0, Vec::len);
            if !batch.is_empty()
                && (batch.len() >= limits.batch_records || bytes + size > limits.batch_bytes)
            {
                batches.push(std::mem::take(&mut batch));
                bytes = 0;
            }
            bytes += size;
            batch.push((row, wire));
        }
        if !batch.is_empty() {
            batches.push(batch);
        }
        for batch in batches {
            let wires: Vec<Wire> = batch.iter().map(|(_, w)| w.clone()).collect();
            let started = Instant::now();
            let reply = self.transport.write(&wires, false).await;
            flushed.batches += 1;
            flushed
                .batch_millis
                .push(started.elapsed().as_millis() as u64);
            match reply {
                Ok(outcomes) if outcomes.len() == batch.len() => {
                    let mut rows = Vec::new();
                    for ((mut row, wire), outcome) in batch.into_iter().zip(outcomes) {
                        match outcome {
                            Outcome::Created | Outcome::Updated | Outcome::Unchanged => {
                                acknowledge(&mut row, &wire);
                                flushed.sent += 1;
                            }
                            Outcome::Stale | Outcome::Conflict | Outcome::Aborted => {
                                // The Worker holds another revision of this record (a restored
                                // ledger or database): continue above it, or accept an identical one.
                                match self.transport.get(row.collection, &row.blind).await {
                                    Ok(Some(held))
                                        if held.revision == wire.revision
                                            && held.digest == wire.digest
                                            && held.deleted == wire.deleted =>
                                    {
                                        acknowledge(&mut row, &wire);
                                        flushed.sent += 1;
                                    }
                                    Ok(held) if wire.base.is_some() => {
                                        // The store's copy is the source of truth: wait for a
                                        // pull to bring it here, or for the user to choose.
                                        row.floor = row.floor.max(held.map_or(0, |h| h.revision));
                                        row.error = CONFLICT.into();
                                        row.next_at = i64::MAX;
                                        flushed.conflicts += 1;
                                    }
                                    Ok(held) => {
                                        row.floor = row.floor.max(held.map_or(0, |h| h.revision));
                                        row.revision = row.revision.max(row.floor) + 1;
                                        row.attempts += 1;
                                        row.error = "drift".into();
                                        flushed.refused += 1;
                                    }
                                    Err(failure) => {
                                        self.ledger.put_all(&rows)?;
                                        return self.finish(flushed, failure, now);
                                    }
                                }
                            }
                        }
                        rows.push(row);
                    }
                    self.ledger.put_all(&rows)?;
                }
                Ok(_) => {
                    let rows = refuse(batch, "bad_reply", now);
                    flushed.refused += rows.len() as u64;
                    self.ledger.put_all(&rows)?;
                }
                Err(Failure::Refused(code)) => {
                    let rows = refuse(batch, &code, now);
                    flushed.refused += rows.len() as u64;
                    self.ledger.put_all(&rows)?;
                }
                Err(failure) => {
                    let rows: Vec<Row> = batch
                        .into_iter()
                        .map(|(mut row, _)| {
                            row.attempts += 1;
                            row.error = failure.code();
                            row
                        })
                        .collect();
                    self.ledger.put_all(&rows)?;
                    return self.finish(flushed, failure, now);
                }
            }
        }
        if flushed.refused == 0 && flushed.conflicts == 0 {
            self.ledger.set_meta("last_success_at", &now.to_string())?;
            self.ledger.clear_meta("last_error")?;
        } else if flushed.refused == 0 {
            self.ledger.set_meta("last_error", CONFLICT)?;
        } else {
            self.ledger.set_meta("last_error", "refused")?;
        }
        Ok(flushed)
    }

    fn finish(&self, mut flushed: Flushed, failure: Failure, _now: i64) -> Result<Flushed> {
        self.ledger.set_meta("last_error", &failure.code())?;
        flushed.failure = Some(failure.code());
        Ok(flushed)
    }

    /// Compare the account store's copy with the local stores: per collection the counts and a
    /// digest over the sorted `(id, revision, ciphertext digest)` computed from the ledger and
    /// from the Worker's listing, plus a full decryption of every record checked against what
    /// was sent.
    pub async fn verify(&self) -> Result<std::result::Result<Report, Failure>> {
        let mut collections = Vec::new();
        for collection in Collection::ALL {
            let remote = match self.listing(collection).await {
                Ok(remote) => remote,
                Err(failure) => return Ok(Err(failure)),
            };
            if collection.is_configuration() && !self.local.configuration() {
                collections.push(CollectionReport::others(collection, remote.len() as u64));
                continue;
            }
            collections.push(self.verify_collection(collection, remote)?.0);
        }
        Ok(Ok(Report {
            ok: collections.iter().all(|c| c.ok),
            collections,
        }))
    }

    /// Everything the Worker holds of `collection`, page by page.
    async fn listing(&self, collection: Collection) -> Result<Vec<Stored>, Failure> {
        let mut remote = Vec::new();
        let mut since = 0u64;
        loop {
            let page = self.transport.page(collection, since, PAGE).await?;
            since = page
                .records
                .iter()
                .map(|r| r.generation)
                .max()
                .unwrap_or(since);
            let more = page.more && !page.records.is_empty();
            remote.extend(page.records);
            if !more {
                break;
            }
        }
        Ok(remote)
    }

    /// A targeted repair after a verification found differences: every record this
    /// installation had acknowledged that the Worker lacks, holds at another revision or digest,
    /// or holds in a form that does not open or does not match is queued again at its next
    /// revision (the next flush sends it, above the Worker's revision after a `stale`). Records
    /// only the Worker holds are left alone and counted. `configuration: false` leaves the
    /// configuration collections to the pull and the conflicts (primary). Sends nothing itself.
    pub async fn repair(
        &self,
        configuration: bool,
    ) -> Result<std::result::Result<Vec<Repaired>, Failure>> {
        let mut repaired = Vec::new();
        for collection in Collection::ALL {
            if collection.is_configuration() && !(configuration && self.local.configuration()) {
                continue;
            }
            let remote = match self.listing(collection).await {
                Ok(remote) => remote,
                Err(failure) => return Ok(Err(failure)),
            };
            let (report, broken) = self.verify_collection(collection, remote)?;
            let requeued = self.ledger.requeue(collection, &broken)?;
            if requeued > 0 || report.unknown_remote > 0 {
                repaired.push(Repaired {
                    name: collection.name(),
                    requeued,
                    unknown_remote: report.unknown_remote,
                });
            }
        }
        Ok(Ok(repaired))
    }

    /// The collection's report and the blinded IDs of the acknowledged records the Worker does
    /// not hold as they were sent.
    fn verify_collection(
        &self,
        collection: Collection,
        mut remote: Vec<Stored>,
    ) -> Result<(CollectionReport, Vec<String>)> {
        // History is each installation's own: another computer of the account mirrors its records
        // and verifies them itself. The configuration is the account's and is compared whole.
        let held = remote.len();
        if !collection.is_configuration() {
            remote.retain(|s| s.own != Some(false));
        }
        // Another installation's own settings are kept for it alone, and a tombstone of a record
        // this installation never had (one removed before it joined) has nothing to compare.
        let known: BTreeSet<String> = self
            .ledger
            .rows(collection)?
            .into_iter()
            .map(|r| r.blind)
            .collect();
        remote.retain(|s| known.contains(&s.id) || (!s.deleted && !self.foreign(collection, s)));
        let others = (held - remote.len()) as u64;
        let local: BTreeMap<String, String> = self
            .local
            .collect(collection)?
            .into_iter()
            .map(|e| {
                let h = hash(&e.id, &e.value);
                (e.id, h)
            })
            .collect();
        let rows: Vec<Row> = self
            .ledger
            .rows(collection)?
            .into_iter()
            .filter(|r| r.acked_revision > 0)
            .collect();
        let by_blind: BTreeMap<&str, &Row> = rows.iter().map(|r| (r.blind.as_str(), r)).collect();
        let stored: BTreeMap<&str, &Stored> = remote.iter().map(|s| (s.id.as_str(), s)).collect();

        let digest = |lines: Vec<String>| {
            let mut lines = lines;
            lines.sort();
            hex_sha256(lines.join("\n").as_bytes())
        };
        let local_digest = digest(
            rows.iter()
                .map(|r| {
                    format!(
                        "{}|{}|{}|{}",
                        r.blind,
                        r.acked_revision,
                        r.acked_digest,
                        u8::from(r.acked_hash == DELETED)
                    )
                })
                .collect(),
        );
        let remote_digest = digest(
            remote
                .iter()
                .map(|s| {
                    format!(
                        "{}|{}|{}|{}",
                        s.id,
                        s.revision,
                        s.digest,
                        u8::from(s.deleted)
                    )
                })
                .collect(),
        );

        let mut report = CollectionReport {
            name: collection.name(),
            local: local.len() as u64,
            remote: remote.iter().filter(|s| !s.deleted).count() as u64,
            tombstones: remote.iter().filter(|s| s.deleted).count() as u64,
            pending: 0,
            missing_remote: 0,
            unknown_remote: 0,
            differing: 0,
            undecryptable: 0,
            content_differs: 0,
            others,
            local_digest,
            remote_digest,
            ok: false,
        };
        let mut broken = Vec::new();
        for row in &rows {
            if !stored.contains_key(row.blind.as_str()) {
                report.missing_remote += 1;
                broken.push(row.blind.clone());
            }
        }
        for held in &remote {
            let Some(row) = by_blind.get(held.id.as_str()) else {
                report.unknown_remote += 1;
                continue;
            };
            if row.acked_revision != held.revision
                || row.acked_digest != held.digest
                || (row.acked_hash == DELETED) != held.deleted
            {
                report.differing += 1;
                broken.push(held.id.clone());
                continue;
            }
            let Some(sealed) = held.sealed.as_deref().filter(|_| !held.deleted) else {
                continue;
            };
            if hex_sha256(sealed) != held.digest {
                report.differing += 1;
                broken.push(held.id.clone());
                continue;
            }
            let Some(plain) = seal::open(
                self.content,
                collection,
                &held.id,
                held.revision,
                held.schema,
                sealed,
            ) else {
                report.undecryptable += 1;
                broken.push(held.id.clone());
                continue;
            };
            let named = serde_json::from_slice::<Value>(&plain)
                .ok()
                .and_then(|v| v["id"].as_str().map(str::to_owned));
            let consistent = named
                .as_deref()
                .is_some_and(|n| seal::blind_id(self.content, collection, n) == held.id)
                && hex_sha256(&plain) == row.acked_hash;
            if !consistent {
                report.content_differs += 1;
                broken.push(held.id.clone());
            }
        }
        // What changed locally since the last upload is the outbox's, not a difference.
        let acked: BTreeMap<&str, &str> = rows
            .iter()
            .map(|r| (r.natural.as_str(), r.acked_hash.as_str()))
            .collect();
        for (id, current) in &local {
            if acked.get(id.as_str()) != Some(&current.as_str()) {
                report.pending += 1;
            }
        }
        for row in &rows {
            if row.acked_hash != DELETED && !local.contains_key(&row.natural) {
                report.pending += 1;
            }
        }
        report.ok = report.missing_remote == 0
            && report.unknown_remote == 0
            && report.differing == 0
            && report.undecryptable == 0
            && report.content_differs == 0
            && report.local_digest == report.remote_digest;
        Ok((report, broken))
    }
}

impl<L: Local, T: Transport> Engine<'_, L, T> {
    /// Whether a held `settings` record this installation never acknowledged is another
    /// installation's own settings: opened to read its natural ID, or, for a tombstone (the shared
    /// settings are never removed), one another installation wrote.
    pub(super) fn foreign(&self, collection: Collection, held: &Stored) -> bool {
        if collection != Collection::Settings || held.own == Some(true) {
            return false;
        }
        if held.deleted {
            return held.own == Some(false);
        }
        let Some(sealed) = held.sealed.as_deref() else {
            return false;
        };
        let Some(plain) = seal::open(
            self.content,
            collection,
            &held.id,
            held.revision,
            held.schema,
            sealed,
        ) else {
            return false;
        };
        serde_json::from_slice::<Value>(&plain)
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_owned))
            .is_some_and(|id| {
                super::foreign_installation(&id, self.local.installation().as_deref())
            })
    }
}

pub(super) fn acknowledge(row: &mut Row, wire: &Wire) {
    row.acked_revision = wire.revision;
    row.acked_hash = row.target_hash.clone();
    row.acked_digest = wire.digest.clone();
    row.target_hash.clear();
    row.pending_since = None;
    row.attempts = 0;
    row.next_at = 0;
    row.error.clear();
}

fn refuse(batch: Vec<(Row, Wire)>, code: &str, now: i64) -> Vec<Row> {
    batch
        .into_iter()
        .map(|(mut row, _)| {
            row.attempts += 1;
            row.error = code.into();
            row.next_at = now + backoff(row.attempts);
            row
        })
        .collect()
}

/// What a targeted repair queued again in one collection.
#[derive(Clone, Debug, Serialize)]
pub struct Repaired {
    pub name: &'static str,
    /// Records queued to be sent again.
    pub requeued: u64,
    /// Records only the Worker holds, left alone.
    pub unknown_remote: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CollectionReport {
    pub name: &'static str,
    /// Records in the local stores.
    pub local: u64,
    /// Live and deleted records the Worker holds.
    pub remote: u64,
    pub tombstones: u64,
    /// Local records whose newest content has not been acknowledged yet (the outbox's).
    pub pending: u64,
    pub missing_remote: u64,
    pub unknown_remote: u64,
    pub differing: u64,
    pub undecryptable: u64,
    pub content_differs: u64,
    /// History records other installations of the account wrote: kept and verified by them.
    pub others: u64,
    pub local_digest: String,
    pub remote_digest: String,
    pub ok: bool,
}

impl CollectionReport {
    /// A collection this installation does not keep in the store: what it holds is others'.
    fn others(collection: Collection, held: u64) -> Self {
        let none = hex_sha256(b"");
        Self {
            name: collection.name(),
            local: 0,
            remote: 0,
            tombstones: 0,
            pending: 0,
            missing_remote: 0,
            unknown_remote: 0,
            differing: 0,
            undecryptable: 0,
            content_differs: 0,
            others: held,
            local_digest: none.clone(),
            remote_digest: none,
            ok: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub ok: bool,
    pub collections: Vec<CollectionReport>,
}
