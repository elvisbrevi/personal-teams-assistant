//! The mirror of the account's records in the account store (phase 3b of the cloud storage
//! design): the content key and the sealing that keep every record encrypted before it leaves,
//! the ledger of what the Worker acknowledged, and the engine that uploads changes and verifies
//! the copy. Local stays the source of truth; nothing here writes to a local store, and the
//! engine reaches the Worker and the stores only through two small traits so it can be tested
//! without either.
//!
//! The "outbox" is the ledger: a record is pending when what the local stores hold now is not
//! what the Worker acknowledged. Only identifiers and hashes are queued, never content, so the
//! queue is bounded by the number of local records and a write path that does not announce itself
//! (a recovery at start, a restored database) is still caught by the next sweep.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Announces that a store wrote a record the mirror keeps; the host's loop sends it soon. A write
/// that does not announce itself is found by the loop's regular sweep.
pub static WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub fn wake() {
    WAKE.notify_one();
}

pub mod engine;
pub mod key;
pub mod ledger;
pub mod primary;
pub mod seal;

/// The collections of the account store (`cloud/src/protocol.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Collection {
    Settings,
    Organization,
    Notes,
    Evaluations,
    Examples,
    Audits,
    ActivitySnapshots,
    ActivitySummaries,
    ActivityRuns,
    ImplementationRuns,
    ImplementationRequests,
}

impl Collection {
    pub const ALL: [Collection; 11] = [
        Self::Settings,
        Self::Organization,
        Self::Notes,
        Self::Evaluations,
        Self::Examples,
        Self::Audits,
        Self::ActivitySnapshots,
        Self::ActivitySummaries,
        Self::ActivityRuns,
        Self::ImplementationRuns,
        Self::ImplementationRequests,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Organization => "organization",
            Self::Notes => "notes",
            Self::Evaluations => "evaluations",
            Self::Examples => "examples",
            Self::Audits => "audits",
            Self::ActivitySnapshots => "activity_snapshots",
            Self::ActivitySummaries => "activity_summaries",
            Self::ActivityRuns => "activity_runs",
            Self::ImplementationRuns => "implementation_runs",
            Self::ImplementationRequests => "implementation_requests",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    /// The account's shared configuration: with the account store as the source of truth
    /// (mode `primary`), these are written there first, under compare-and-swap, and pulled from
    /// it; every other collection keeps being mirrored from the local stores.
    pub fn is_configuration(self) -> bool {
        matches!(self, Self::Settings | Self::Organization | Self::Notes)
    }

    /// The version of the record's payload format, bound into the sealed record.
    pub fn schema(self) -> u32 {
        1
    }
}

/// One local record as a mirror reads it: its natural identifier and its content.
#[derive(Clone, Debug, PartialEq)]
pub struct Exported {
    pub id: String,
    pub value: Value,
}

/// The canonical bytes of a JSON value: object keys sorted at every depth, so the same content
/// always has the same bytes (and the same hash) whatever order a store produced it in.
pub fn canonical(value: &Value) -> Vec<u8> {
    fn write(value: &Value, out: &mut Vec<u8>) {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push(b'{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    out.extend(serde_json::to_vec(key).unwrap_or_default());
                    out.push(b':');
                    write(&map[key], out);
                }
                out.push(b'}');
            }
            Value::Array(items) => {
                out.push(b'[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    write(item, out);
                }
                out.push(b']');
            }
            scalar => out.extend(serde_json::to_vec(scalar).unwrap_or_default()),
        }
    }
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

pub fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What a record holds before it is sealed: its natural identifier (so a copy restores without the
/// blinded one) and its content.
pub fn plaintext(id: &str, value: &Value) -> Vec<u8> {
    canonical(&serde_json::json!({"id": id, "data": value}))
}

#[cfg(test)]
pub(crate) mod tests;
