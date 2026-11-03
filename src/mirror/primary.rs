//! The account store as the source of truth of the account's configuration (phase 3c, mode
//! `primary`): configuration records are written there first under compare-and-swap, and a
//! pull brings what the store holds back into the local stores, which stay a complete replica
//! (so everything keeps reading locally, offline included). A local change the store never
//! acknowledged is never overwritten by a pull: it is a conflict the user resolves.
//!
//! Only the configuration collections ([`Collection::is_configuration`]) are pulled and written
//! through; every other collection keeps being mirrored from the local stores by the outbox.

use super::{
    Collection,
    engine::{
        CONFLICT, Engine, Failure, Local, Outcome, Transport, Wire, acknowledge, fresh, hash,
    },
    hex_sha256,
    ledger::{DELETED, Row},
    plaintext, seal,
};
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const PAGE: usize = 500;

/// The meta key of the last generation of `collection` pulled into the local stores.
fn pulled_key(collection: Collection) -> String {
    format!("pulled:{}", collection.name())
}

/// Writes what the account store holds into the local stores.
pub trait Apply {
    /// Make the local records of `collection` match these changes, all at once: `Some(value)`
    /// creates or replaces the record, `None` removes it.
    fn apply(&self, collection: Collection, changes: &[(String, Option<Value>)]) -> Result<()>;
}

/// What a pull did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Pulled {
    /// Records written into the local stores.
    pub applied: u64,
    /// Records the store and this installation already agreed on.
    pub acknowledged: u64,
    /// Records changed both here (never acknowledged by the store) and in the store: kept as
    /// they are here until the user chooses.
    pub conflicts: u64,
    /// Records this installation's key cannot open (another account's key, or damage).
    pub undecryptable: u64,
}

/// One configuration record to write: `None` removes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub collection: Collection,
    pub id: String,
    pub value: Option<Value>,
}

/// Why a write-through was not accepted. Nothing was written to the store.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The store holds another version than the one this installation last agreed on.
    Conflict,
    /// A record of this change is in conflict already; the user must choose first.
    Unresolved,
    /// A sealed record exceeds what the Worker accepts.
    TooLarge,
    Failure(Failure),
}

/// A write-through the store accepted: once the local stores hold the change, `rows` are what
/// the ledger keeps (written by [`Accepted::settle`]).
#[derive(Debug, Default)]
pub struct Accepted {
    pub rows: Vec<Row>,
}

impl Accepted {
    pub fn settle(self, ledger: &super::ledger::Ledger) -> Result<()> {
        ledger.put_all(&self.rows)
    }
}

/// What a record holds once opened: its natural ID, content and plaintext hash.
type Opened = (String, Option<Value>, String);

impl<L: Local, T: Transport> Engine<'_, L, T> {
    /// Bring what the store changed since the last pull into the local stores. With `replace`,
    /// every configuration record is read again and the store's version wins over local
    /// changes (the caller backs them up first).
    pub async fn pull(
        &self,
        apply: &impl Apply,
        replace: bool,
    ) -> Result<std::result::Result<Pulled, Failure>> {
        let mut pulled = Pulled::default();
        for collection in Collection::ALL.into_iter().filter(|c| c.is_configuration()) {
            let mut since = if replace {
                0
            } else {
                self.ledger
                    .meta(&pulled_key(collection))?
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
            };
            let mut remote = Vec::new();
            loop {
                let page = match self.transport.page(collection, since, PAGE).await {
                    Ok(page) => page,
                    Err(failure) => return Ok(Err(failure)),
                };
                let more = page.more && !page.records.is_empty();
                since = page
                    .records
                    .iter()
                    .map(|r| r.generation)
                    .max()
                    .unwrap_or(since);
                remote.extend(page.records);
                if !more {
                    break;
                }
            }
            let local: BTreeMap<String, String> = self
                .local
                .collect(collection)?
                .into_iter()
                .map(|e| {
                    let h = hash(&e.id, &e.value);
                    (e.id, h)
                })
                .collect();
            let rows: BTreeMap<String, Row> = self
                .ledger
                .rows(collection)?
                .into_iter()
                .map(|r| (r.blind.clone(), r))
                .collect();
            let mut changes = Vec::new();
            let mut written = Vec::new();
            for held in remote {
                let row = rows.get(&held.id);
                let unresolved = row.is_some_and(|r| r.error == CONFLICT);
                if row.is_some_and(|r| held.revision <= r.acked_revision)
                    && !(replace && unresolved)
                {
                    continue;
                }
                // Another installation's own settings: kept for it, never installed here.
                if row.is_none() && self.foreign(collection, &held) {
                    continue;
                }
                let opened: Opened = if held.deleted {
                    // A tombstone of a record this installation never had changes nothing.
                    let Some(row) = row else { continue };
                    (row.natural.clone(), None, DELETED.into())
                } else {
                    match self.open(collection, &held) {
                        Some(opened) => opened,
                        None => {
                            pulled.undecryptable += 1;
                            continue;
                        }
                    }
                };
                let (natural, value, remote_hash) = opened;
                let here = local.get(&natural).map_or(DELETED, String::as_str);
                let agreed = row.map_or(DELETED, |r| {
                    if r.acked_revision == 0 {
                        DELETED
                    } else {
                        r.acked_hash.as_str()
                    }
                });
                let mut next = row
                    .cloned()
                    .unwrap_or_else(|| fresh(self.content, collection, &natural));
                if here == remote_hash {
                    pulled.acknowledged += 1;
                } else if here == agreed || replace {
                    changes.push((natural.clone(), value));
                    pulled.applied += 1;
                } else {
                    next.floor = next.floor.max(held.revision);
                    next.error = CONFLICT.into();
                    next.next_at = i64::MAX;
                    pulled.conflicts += 1;
                    written.push(next);
                    continue;
                }
                next.revision = next.revision.max(held.revision);
                next.floor = next.floor.max(held.revision);
                next.target_hash = remote_hash;
                acknowledge(
                    &mut next,
                    &Wire {
                        collection,
                        id: held.id.clone(),
                        revision: held.revision,
                        digest: held.digest.clone(),
                        schema: held.schema,
                        deleted: held.deleted,
                        sealed: None,
                        base: None,
                    },
                );
                written.push(next);
            }
            if !changes.is_empty() {
                apply.apply(collection, &changes)?;
            }
            self.ledger.put_all(&written)?;
            self.ledger
                .set_meta(&pulled_key(collection), &since.to_string())?;
        }
        Ok(Ok(pulled))
    }

    /// Open a record the store holds, checking that its natural ID is the one it is filed under.
    fn open(&self, collection: Collection, held: &super::engine::Stored) -> Option<Opened> {
        let sealed = held.sealed.as_deref()?;
        if hex_sha256(sealed) != held.digest {
            return None;
        }
        let plain = seal::open(
            self.content,
            collection,
            &held.id,
            held.revision,
            held.schema,
            sealed,
        )?;
        let value: Value = serde_json::from_slice(&plain).ok()?;
        let natural = value["id"].as_str()?.to_owned();
        if seal::blind_id(self.content, collection, &natural) != held.id {
            return None;
        }
        let data = value.get("data")?.clone();
        Some((natural, Some(data), hex_sha256(&plain)))
    }

    /// Write configuration records to the store before the local stores change, naming the
    /// revision each replaces, in one atomic batch. Nothing is queued: a refusal leaves the store
    /// and the ledger's acknowledgements as they were.
    pub async fn write(
        &self,
        changes: &[Change],
    ) -> Result<std::result::Result<Accepted, Refusal>> {
        let mut batch: Vec<(Row, Wire)> = Vec::new();
        for change in changes {
            debug_assert!(change.collection.is_configuration());
            let mut row = self
                .ledger
                .get(change.collection, &change.id)?
                .unwrap_or_else(|| fresh(self.content, change.collection, &change.id));
            if row.error == CONFLICT {
                return Ok(Err(Refusal::Unresolved));
            }
            let desired = change
                .value
                .as_ref()
                .map_or_else(|| DELETED.to_owned(), |v| hash(&change.id, v));
            let agreed = if row.acked_revision == 0 {
                DELETED
            } else {
                row.acked_hash.as_str()
            };
            if desired == agreed {
                continue;
            }
            let base = row.acked_revision;
            row.revision = row.revision.max(row.acked_revision).max(row.floor) + 1;
            // The revision is recorded before the send, so it is never reused for other bytes;
            // what the local stores hold stays the agreed content until the change is settled.
            self.ledger.put(&row)?;
            row.target_hash = desired;
            let wire = match &change.value {
                None => Wire {
                    collection: row.collection,
                    id: row.blind.clone(),
                    revision: row.revision,
                    digest: hex_sha256(b""),
                    schema: row.collection.schema(),
                    deleted: true,
                    sealed: None,
                    base: Some(base),
                },
                Some(value) => {
                    let bytes = plaintext(&row.natural, value);
                    let sealed = seal::seal(
                        self.content,
                        row.collection,
                        &row.blind,
                        row.revision,
                        &bytes,
                    )?;
                    if sealed.len() > super::engine::SEALED_MAX {
                        return Ok(Err(Refusal::TooLarge));
                    }
                    Wire {
                        collection: row.collection,
                        id: row.blind.clone(),
                        revision: row.revision,
                        digest: hex_sha256(&sealed),
                        schema: row.collection.schema(),
                        deleted: false,
                        sealed: Some(sealed),
                        base: Some(base),
                    }
                }
            };
            batch.push((row, wire));
        }
        if batch.is_empty() {
            return Ok(Ok(Accepted::default()));
        }
        let wires: Vec<Wire> = batch.iter().map(|(_, w)| w.clone()).collect();
        let outcomes = match self.transport.write(&wires, true).await {
            Ok(outcomes) if outcomes.len() == wires.len() => outcomes,
            Ok(_) => return Ok(Err(Refusal::Failure(Failure::Refused("bad_reply".into())))),
            Err(failure) => return Ok(Err(Refusal::Failure(failure))),
        };
        if outcomes
            .iter()
            .any(|o| matches!(o, Outcome::Stale | Outcome::Conflict | Outcome::Aborted))
        {
            return Ok(Err(Refusal::Conflict));
        }
        let rows = batch
            .into_iter()
            .map(|(mut row, wire)| {
                acknowledge(&mut row, &wire);
                row
            })
            .collect();
        Ok(Ok(Accepted { rows }))
    }
}
