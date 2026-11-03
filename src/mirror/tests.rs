use super::{
    Collection, Exported, canonical,
    engine::{Engine, Failure, Limits, Local, Outcome, Page, SEALED_MAX, Stored, Transport, Wire},
    hex_sha256,
    key::{self, Content},
    ledger::Ledger,
    seal,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn hmac_matches_the_rfc_4231_vectors() {
    assert_eq!(
        hex(&seal::hmac_sha256(&[0x0b; 20], &[b"Hi There"])),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    assert_eq!(
        hex(&seal::hmac_sha256(
            b"Jefe",
            &[b"what do ya want ", b"for nothing?"]
        )),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // A key longer than the block is hashed first (RFC 4231 test case 6).
    assert_eq!(
        hex(&seal::hmac_sha256(
            &[0xaa; 131],
            &[b"Test Using Larger Than Block-Size Key - Hash Key First"]
        )),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

#[test]
fn canonical_bytes_do_not_depend_on_key_order() {
    let a: Value = serde_json::from_str(r#"{"b":[1,{"z":1,"a":2}],"a":"x"}"#).unwrap();
    let b: Value = serde_json::from_str(r#"{"a":"x","b":[1,{"a":2,"z":1}]}"#).unwrap();
    assert_eq!(canonical(&a), canonical(&b));
    assert_eq!(
        String::from_utf8(canonical(&a)).unwrap(),
        r#"{"a":"x","b":[1,{"a":2,"z":1}]}"#
    );
}

#[test]
fn sealed_records_authenticate_only_for_their_exact_place() {
    let content = Content::generate();
    let id = seal::blind_id(&content, Collection::Notes, "note-1");
    let sealed = seal::seal(&content, Collection::Notes, &id, 3, b"secret text").unwrap();
    assert!(!sealed.windows(11).any(|w| w == b"secret text"));
    let open = |content: &Content, c, id: &str, revision, schema| {
        seal::open(content, c, id, revision, schema, &sealed)
    };
    assert_eq!(
        open(&content, Collection::Notes, &id, 3, 1).as_deref(),
        Some(&b"secret text"[..])
    );
    assert!(open(&content, Collection::Notes, &id, 4, 1).is_none());
    assert!(open(&content, Collection::Notes, &id, 3, 2).is_none());
    assert!(open(&content, Collection::Audits, &id, 3, 1).is_none());
    assert!(open(&content, Collection::Notes, "other", 3, 1).is_none());
    // Another account with the same key bytes still fails: the content account is bound.
    let mut other = content.clone();
    other.account = uuid::Uuid::new_v4().to_string();
    assert!(open(&other, Collection::Notes, &id, 3, 1).is_none());
    assert!(open(&Content::generate(), Collection::Notes, &id, 3, 1).is_none());
    // The version prefix is authenticated too.
    let mut tampered = sealed.clone();
    tampered[3] ^= 1;
    assert!(seal::open(&content, Collection::Notes, &id, 3, 1, &tampered).is_none());
    // Sealing the same record at the same revision is repeatable; another revision differs.
    assert_eq!(
        sealed,
        seal::seal(&content, Collection::Notes, &id, 3, b"secret text").unwrap()
    );
    assert_ne!(
        sealed,
        seal::seal(&content, Collection::Notes, &id, 4, b"secret text").unwrap()
    );
}

#[test]
fn blinded_ids_hide_the_natural_id_and_are_stable() {
    let content = Content::generate();
    let a = seal::blind_id(
        &content,
        Collection::Audits,
        "chats('19:abc')/messages('1')",
    );
    assert_eq!(a.len(), 64);
    assert_eq!(
        a,
        seal::blind_id(
            &content,
            Collection::Audits,
            "chats('19:abc')/messages('1')"
        )
    );
    assert_ne!(
        a,
        seal::blind_id(&content, Collection::Notes, "chats('19:abc')/messages('1')")
    );
    assert_ne!(
        a,
        seal::blind_id(
            &Content::generate(),
            Collection::Audits,
            "chats('19:abc')/messages('1')"
        )
    );
}

#[test]
fn the_content_key_is_created_once_and_restored_without_replacing() {
    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("keys");
    assert!(key::load(&keys).unwrap().is_none());
    let created = key::create(&keys).unwrap();
    assert!(key::create(&keys).is_err());
    let loaded = key::load(&keys).unwrap().unwrap();
    assert_eq!(loaded.backup(), created.backup());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(keys.join(key::FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let other = tempfile::tempdir().unwrap();
    let restored = other.path().join("keys");
    assert!(key::import(&restored, &created.backup()).unwrap());
    assert!(!key::import(&restored, &created.backup()).unwrap());
    assert!(key::import(&keys, &Content::generate().backup()).is_err());
    let mut broken = created.backup();
    broken.format = "other".into();
    assert!(key::import(&other.path().join("again"), &broken).is_err());
    let debug = format!("{created:?}");
    assert!(!debug.contains(&created.backup().keys[&1]));
}

// --- the engine against an in-memory Worker -------------------------------------------------

/// The local records of an installation, its ID in the account store and whether it is signed
/// in to a coordinator (so its configuration is the coordinator's).
#[derive(Default)]
struct Records(
    Mutex<BTreeMap<Collection, Vec<Exported>>>,
    Mutex<Option<String>>,
    Mutex<bool>,
);

impl Records {
    fn set(&self, collection: Collection, id: &str, value: Value) {
        let mut all = self.0.lock().unwrap();
        let list = all.entry(collection).or_default();
        list.retain(|e| e.id != id);
        list.push(Exported {
            id: id.into(),
            value,
        });
    }
    fn remove(&self, collection: Collection, id: &str) {
        self.0
            .lock()
            .unwrap()
            .entry(collection)
            .or_default()
            .retain(|e| e.id != id);
    }
}

impl Local for Records {
    fn installation(&self) -> Option<String> {
        self.1.lock().unwrap().clone()
    }

    fn configuration(&self) -> bool {
        !*self.2.lock().unwrap()
    }

    fn collect(&self, collection: Collection) -> anyhow::Result<Vec<Exported>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(&collection)
            .cloned()
            .unwrap_or_default())
    }
}

/// The Worker's write rules for one account (`cloud/src/account-store.ts`).
#[derive(Default)]
pub(crate) struct Worker {
    pub(crate) records: Mutex<HashMap<(Collection, String), Stored>>,
    generations: Mutex<HashMap<Collection, u64>>,
    /// A failure to answer with instead of storing, for the next write.
    pub(crate) refuse: Mutex<Option<Failure>>,
    /// Store the next write but lose its answer.
    lose_answer: Mutex<bool>,
    writes: Mutex<u64>,
    /// The installation the next requests come from (feature `origin`), and who wrote each record.
    pub(crate) installation: Mutex<u32>,
    writers: Mutex<HashMap<(Collection, String), u32>>,
}

impl Worker {
    /// A record as listed to the current installation: `own` says whether it wrote it.
    fn listed(&self, stored: &Stored) -> Stored {
        let writer = self
            .writers
            .lock()
            .unwrap()
            .get(&(stored.collection, stored.id.clone()))
            .copied();
        Stored {
            own: Some(writer.unwrap_or_default() == *self.installation.lock().unwrap()),
            ..stored.clone()
        }
    }

    /// `DELETE /v1/store`: every record goes, the generations stay. Returns how many went.
    pub(crate) fn erase(&self) -> u64 {
        let mut held = self.records.lock().unwrap();
        let erased = held.len() as u64;
        held.clear();
        self.writers.lock().unwrap().clear();
        erased
    }

    /// What writing `wire` would do, without writing it (`decide` in `account-store.ts`).
    fn decide(held: &HashMap<(Collection, String), Stored>, wire: &Wire) -> Outcome {
        let old = held.get(&(wire.collection, wire.id.clone()));
        if let Some(old) = old.filter(|o| o.revision == wire.revision) {
            return if old.digest == wire.digest && old.deleted == wire.deleted {
                Outcome::Unchanged
            } else {
                Outcome::Conflict
            };
        }
        if wire
            .base
            .is_some_and(|base| old.map_or(0, |o| o.revision) != base)
        {
            return Outcome::Conflict;
        }
        match old {
            Some(old) if old.revision > wire.revision => Outcome::Stale,
            Some(_) => Outcome::Updated,
            None => Outcome::Created,
        }
    }
}

impl Transport for Worker {
    async fn write(&self, records: &[Wire], atomic: bool) -> Result<Vec<Outcome>, Failure> {
        if let Some(failure) = self.refuse.lock().unwrap().take() {
            return Err(failure);
        }
        assert!((1..=100).contains(&records.len()));
        *self.writes.lock().unwrap() += 1;
        let mut held = self.records.lock().unwrap();
        let decided: Vec<Outcome> = records.iter().map(|w| Self::decide(&held, w)).collect();
        if atomic
            && decided
                .iter()
                .any(|o| matches!(o, Outcome::Stale | Outcome::Conflict))
        {
            return Ok(decided
                .into_iter()
                .map(|o| match o {
                    Outcome::Created | Outcome::Updated => Outcome::Aborted,
                    other => other,
                })
                .collect());
        }
        for (wire, outcome) in records.iter().zip(&decided) {
            if !matches!(outcome, Outcome::Created | Outcome::Updated) {
                continue;
            }
            if let Some(sealed) = &wire.sealed {
                assert_eq!(hex_sha256(sealed), wire.digest);
            }
            let mut generations = self.generations.lock().unwrap();
            let generation = generations.entry(wire.collection).or_default();
            *generation += 1;
            held.insert(
                (wire.collection, wire.id.clone()),
                Stored {
                    collection: wire.collection,
                    id: wire.id.clone(),
                    revision: wire.revision,
                    digest: wire.digest.clone(),
                    schema: wire.schema,
                    deleted: wire.deleted,
                    generation: *generation,
                    sealed: wire.sealed.clone(),
                    own: None,
                },
            );
            self.writers.lock().unwrap().insert(
                (wire.collection, wire.id.clone()),
                *self.installation.lock().unwrap(),
            );
        }
        if std::mem::take(&mut *self.lose_answer.lock().unwrap()) {
            return Err(Failure::Offline);
        }
        Ok(decided)
    }

    async fn page(
        &self,
        collection: Collection,
        since: u64,
        limit: usize,
    ) -> Result<Page, Failure> {
        let held = self.records.lock().unwrap();
        let mut found: Vec<Stored> = held
            .values()
            .filter(|s| s.collection == collection && s.generation > since)
            .map(|s| self.listed(s))
            .collect();
        found.sort_by_key(|s| s.generation);
        let more = found.len() > limit;
        found.truncate(limit);
        Ok(Page {
            records: found,
            more,
        })
    }

    async fn get(&self, collection: Collection, id: &str) -> Result<Option<Stored>, Failure> {
        Ok(self
            .records
            .lock()
            .unwrap()
            .get(&(collection, id.to_owned()))
            .map(|s| self.listed(s)))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    local: Records,
    worker: Worker,
    ledger: Ledger,
    content: Content,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("cloud").join("mirror.db")).unwrap();
        Self {
            _dir: dir,
            local: Records::default(),
            worker: Worker::default(),
            ledger,
            content: Content::generate(),
        }
    }
    fn engine(&self) -> Engine<'_, Records, Worker> {
        Engine {
            local: &self.local,
            transport: &self.worker,
            ledger: &self.ledger,
            content: &self.content,
            cas: false,
        }
    }
    async fn flush(&self, now: i64) -> super::engine::Flushed {
        self.engine().flush(now, Limits::default()).await.unwrap()
    }
    async fn verify(&self) -> super::engine::Report {
        self.engine().verify().await.unwrap().unwrap()
    }
    fn filled(&self) {
        self.local
            .set(Collection::Notes, "n1", json!({"title": "one"}));
        self.local
            .set(Collection::Notes, "n2", json!({"title": "two"}));
        self.local.set(
            Collection::Audits,
            "chats('x')/messages('1')",
            json!({"status": "sent", "audit": {"proposed": "hello"}}),
        );
        self.local.set(
            Collection::Settings,
            "config",
            json!({"llm": {"language": "es"}}),
        );
    }
}

fn collection<'a>(
    report: &'a super::engine::Report,
    name: &str,
) -> &'a super::engine::CollectionReport {
    report.collections.iter().find(|c| c.name == name).unwrap()
}

#[tokio::test]
async fn the_first_pass_uploads_everything_and_verifies() {
    let f = Fixture::new();
    f.filled();
    let flushed = f.flush(100).await;
    assert_eq!(
        (flushed.sent, flushed.refused, flushed.failure),
        (4, 0, None)
    );
    assert_eq!(f.ledger.outbox(None, 100).unwrap().pending, 0);
    assert_eq!(f.ledger.outbox(None, 100).unwrap().synced, 4);
    let report = f.verify().await;
    assert!(report.ok, "{report:?}");
    let notes = collection(&report, "notes");
    assert_eq!((notes.local, notes.remote, notes.pending), (2, 2, 0));
    assert_eq!(notes.local_digest, notes.remote_digest);
    // Nothing the Worker holds is readable without the key, and no natural ID reaches it.
    let held = f.worker.records.lock().unwrap();
    for stored in held.values() {
        assert_eq!(stored.id.len(), 64);
        let sealed = stored.sealed.as_ref().unwrap();
        assert!(!sealed.windows(5).any(|w| w == b"hello" || w == b"chats"));
    }
}

#[tokio::test]
async fn another_installations_history_is_its_own_and_the_configuration_is_shared() {
    let f = Fixture::new();
    f.filled();
    f.flush(100).await;
    // A second installation of the account, with the same key, uploads its own history.
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("mirror.db")).unwrap();
    let local = Records::default();
    local.set(
        Collection::Audits,
        "chats('y')/messages('2')",
        json!({"status": "sent", "audit": {"proposed": "hi"}}),
    );
    *f.worker.installation.lock().unwrap() = 2;
    let second = Engine {
        local: &local,
        transport: &f.worker,
        ledger: &ledger,
        content: &f.content,
        cas: false,
    };
    second.flush(200, Limits::default()).await.unwrap();
    let theirs = second.verify().await.unwrap().unwrap();
    let audits = collection(&theirs, "audits");
    assert_eq!(
        (audits.remote, audits.others, audits.unknown_remote),
        (1, 1, 0)
    );
    // The first installation's notes and settings are the account's, so the second must pull them.
    assert_eq!(collection(&theirs, "notes").unknown_remote, 2);
    assert!(!theirs.ok);
    *f.worker.installation.lock().unwrap() = 0;
    let mine = f.verify().await;
    assert!(mine.ok, "{mine:?}");
    assert_eq!(collection(&mine, "audits").others, 1);
}

#[tokio::test]
async fn an_unchanged_store_sends_nothing_and_a_change_sends_the_next_revision() {
    let f = Fixture::new();
    f.filled();
    f.flush(100).await;
    let writes = *f.worker.writes.lock().unwrap();
    let again = f.flush(200).await;
    assert_eq!((again.sent, again.batches), (0, 0));
    assert_eq!(*f.worker.writes.lock().unwrap(), writes);
    f.local
        .set(Collection::Notes, "n1", json!({"title": "one, edited"}));
    assert_eq!(f.engine().plan(300).unwrap(), 1);
    assert_eq!(f.ledger.outbox(None, 330).unwrap().oldest_seconds, Some(30));
    let sent = f.flush(300).await;
    assert_eq!(sent.sent, 1);
    let row = f.ledger.get(Collection::Notes, "n1").unwrap().unwrap();
    assert_eq!((row.acked_revision, row.revision), (2, 2));
    assert!(f.verify().await.ok);
}

#[tokio::test]
async fn a_removed_record_becomes_a_tombstone_and_a_never_sent_one_is_forgotten() {
    let f = Fixture::new();
    f.filled();
    f.flush(100).await;
    f.local.remove(Collection::Notes, "n2");
    f.local
        .set(Collection::Notes, "n3", json!({"title": "three"}));
    f.local.remove(Collection::Notes, "n3");
    f.flush(200).await;
    let report = f.verify().await;
    assert!(report.ok, "{report:?}");
    let notes = collection(&report, "notes");
    assert_eq!((notes.local, notes.remote, notes.tombstones), (1, 1, 1));
    assert!(f.ledger.get(Collection::Notes, "n3").unwrap().is_none());
    // The record comes back: it continues above the tombstone's revision.
    f.local
        .set(Collection::Notes, "n2", json!({"title": "two"}));
    f.flush(300).await;
    let row = f.ledger.get(Collection::Notes, "n2").unwrap().unwrap();
    assert_eq!(row.acked_revision, 3);
    assert!(f.verify().await.ok);
}

#[tokio::test]
async fn an_unreachable_worker_keeps_the_records_pending_and_nothing_is_lost() {
    let f = Fixture::new();
    f.filled();
    *f.worker.refuse.lock().unwrap() = Some(Failure::Offline);
    let flushed = f.flush(100).await;
    assert_eq!(
        (flushed.sent, flushed.failure.as_deref()),
        (0, Some("offline"))
    );
    assert_eq!(f.ledger.outbox(None, 100).unwrap().pending, 4);
    assert_eq!(f.ledger.outbox(None, 100).unwrap().retrying, 4);
    let flushed = f.flush(160).await;
    assert_eq!((flushed.sent, flushed.failure), (4, None));
    assert!(f.verify().await.ok);
    *f.worker.refuse.lock().unwrap() = Some(Failure::Quota);
    f.local
        .set(Collection::Notes, "n1", json!({"title": "later"}));
    let flushed = f.flush(200).await;
    assert_eq!(flushed.failure.as_deref(), Some("quota"));
    assert_eq!(f.ledger.outbox(None, 200).unwrap().pending, 1);
}

#[tokio::test]
async fn a_refused_batch_backs_off_and_is_retried() {
    let f = Fixture::new();
    f.filled();
    *f.worker.refuse.lock().unwrap() = Some(Failure::Refused("invalid_input".into()));
    let flushed = f.flush(100).await;
    assert_eq!((flushed.sent, flushed.refused), (0, 4));
    // Still inside the backoff: nothing is sent.
    assert_eq!(f.flush(110).await.batches, 0);
    assert_eq!(f.flush(100 + 61).await.sent, 4);
    assert!(f.verify().await.ok);
}

#[tokio::test]
async fn a_send_whose_answer_was_lost_is_repeated_with_the_same_bytes() {
    let f = Fixture::new();
    f.local
        .set(Collection::Notes, "n1", json!({"title": "one"}));
    *f.worker.lose_answer.lock().unwrap() = true;
    let flushed = f.flush(100).await;
    assert_eq!(flushed.failure.as_deref(), Some("offline"));
    let first = f
        .worker
        .records
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    let flushed = f.flush(200).await;
    assert_eq!(
        (flushed.sent, flushed.refused, flushed.failure),
        (1, 0, None)
    );
    let second = f
        .worker
        .records
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(second.revision, 1);
    // Content that changed in between goes out as the next revision.
    let g = Fixture::new();
    g.local
        .set(Collection::Notes, "n1", json!({"title": "one"}));
    *g.worker.lose_answer.lock().unwrap() = true;
    g.flush(100).await;
    g.local
        .set(Collection::Notes, "n1", json!({"title": "edited"}));
    g.flush(200).await;
    let row = g.ledger.get(Collection::Notes, "n1").unwrap().unwrap();
    assert_eq!(row.acked_revision, 2);
    assert!(g.verify().await.ok);
}

#[tokio::test]
async fn a_worker_ahead_of_the_ledger_is_followed() {
    let f = Fixture::new();
    f.local
        .set(Collection::Notes, "n1", json!({"title": "one"}));
    f.flush(100).await;
    f.local
        .set(Collection::Notes, "n1", json!({"title": "two"}));
    f.flush(200).await;
    // The ledger is lost (a restored profile): the Worker holds revision 2.
    f.ledger.forget(Collection::Notes, "n1").unwrap();
    let flushed = f.flush(300).await;
    assert_eq!(flushed.refused, 1);
    let flushed = f.flush(300).await;
    assert_eq!(flushed.sent, 1);
    let row = f.ledger.get(Collection::Notes, "n1").unwrap().unwrap();
    assert_eq!(row.acked_revision, 3);
    assert!(f.verify().await.ok);
}

#[tokio::test]
async fn a_record_too_large_for_the_worker_is_blocked_visibly() {
    let f = Fixture::new();
    f.filled();
    f.local.set(
        Collection::Notes,
        "big",
        json!({"text": "x".repeat(SEALED_MAX + 10)}),
    );
    let flushed = f.flush(100).await;
    assert_eq!(flushed.sent, 4);
    let outbox = f.ledger.outbox(None, 100).unwrap();
    assert_eq!((outbox.pending, outbox.blocked), (1, 1));
    assert_eq!(f.flush(100_000).await.batches, 0);
    // Shrinking it unblocks it.
    f.local
        .set(Collection::Notes, "big", json!({"text": "small"}));
    assert_eq!(f.flush(100_001).await.sent, 1);
    assert_eq!(f.ledger.outbox(None, 100_001).unwrap().blocked, 0);
}

#[tokio::test]
async fn batches_respect_the_record_and_byte_limits() {
    let f = Fixture::new();
    for n in 0..250 {
        f.local
            .set(Collection::Notes, &format!("n{n}"), json!({"n": n}));
    }
    let flushed = f
        .engine()
        .flush(
            100,
            Limits {
                records: 500,
                batch_records: 100,
                batch_bytes: 1_000_000,
            },
        )
        .await
        .unwrap();
    assert_eq!((flushed.sent, flushed.batches), (250, 3));
    for n in 0..4 {
        f.local.set(
            Collection::Evaluations,
            &format!("e{n}"),
            json!({"t": "y".repeat(400_000)}),
        );
    }
    let flushed = f
        .engine()
        .flush(
            200,
            Limits {
                records: 500,
                batch_records: 100,
                batch_bytes: 1_000_000,
            },
        )
        .await
        .unwrap();
    assert_eq!((flushed.sent, flushed.batches), (4, 2));
    // A pass sends at most `records` and the rest waits for the next one.
    for n in 0..30 {
        f.local
            .set(Collection::Examples, &format!("x{n}"), json!({"n": n}));
    }
    let flushed = f
        .engine()
        .flush(
            300,
            Limits {
                records: 20,
                ..Limits::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(flushed.sent, 20);
    assert_eq!(f.ledger.outbox(None, 300).unwrap().pending, 10);
}

#[tokio::test]
async fn verification_finds_every_kind_of_difference() {
    let f = Fixture::new();
    f.filled();
    f.flush(100).await;
    let blind = seal::blind_id(&f.content, Collection::Notes, "n1");
    let key = (Collection::Notes, blind.clone());
    // 1. Ciphertext that does not authenticate for its place.
    {
        let mut held = f.worker.records.lock().unwrap();
        let stored = held.get_mut(&key).unwrap();
        let mut sealed = stored.sealed.clone().unwrap();
        *sealed.last_mut().unwrap() ^= 1;
        stored.digest = hex_sha256(&sealed);
        stored.sealed = Some(sealed);
    }
    let report = f.verify().await;
    assert!(!report.ok);
    assert_eq!(collection(&report, "notes").differing, 1);
    // Repair the digest bookkeeping the way the ledger sees it: now it is only undecryptable.
    let mut row = f.ledger.get(Collection::Notes, "n1").unwrap().unwrap();
    row.acked_digest = f.worker.records.lock().unwrap()[&key].digest.clone();
    f.ledger.put(&row).unwrap();
    let report = f.verify().await;
    assert_eq!(collection(&report, "notes").undecryptable, 1);
    assert!(!report.ok);
    // 2. A record the Worker lost.
    f.worker.records.lock().unwrap().remove(&key);
    let report = f.verify().await;
    assert_eq!(collection(&report, "notes").missing_remote, 1);
    // 3. A record the ledger does not know.
    f.worker.records.lock().unwrap().insert(
        (Collection::Examples, "f".repeat(64)),
        Stored {
            collection: Collection::Examples,
            id: "f".repeat(64),
            revision: 1,
            digest: hex_sha256(b"x"),
            schema: 1,
            deleted: false,
            generation: 99,
            sealed: Some(b"x".to_vec()),
            own: None,
        },
    );
    let report = f.verify().await;
    assert_eq!(collection(&report, "examples").unknown_remote, 1);
    // 4. Content sealed for another record under a valid place: the payload names another ID.
    let other = seal::blind_id(&f.content, Collection::Settings, "config");
    let wrong = {
        let plain = super::plaintext("not-config", &json!({"llm": {}}));
        seal::seal(&f.content, Collection::Settings, &other, 1, &plain).unwrap()
    };
    {
        let mut held = f.worker.records.lock().unwrap();
        let stored = held
            .get_mut(&(Collection::Settings, other.clone()))
            .unwrap();
        stored.digest = hex_sha256(&wrong);
        stored.sealed = Some(wrong.clone());
    }
    let mut row = f
        .ledger
        .get(Collection::Settings, "config")
        .unwrap()
        .unwrap();
    row.acked_digest = hex_sha256(&wrong);
    f.ledger.put(&row).unwrap();
    let report = f.verify().await;
    assert_eq!(collection(&report, "settings").content_differs, 1);
}

#[tokio::test]
async fn local_changes_since_the_upload_are_pending_not_differences() {
    let f = Fixture::new();
    f.filled();
    f.flush(100).await;
    f.local
        .set(Collection::Notes, "n1", json!({"title": "newer"}));
    f.local.remove(Collection::Notes, "n2");
    f.local
        .set(Collection::Notes, "n9", json!({"title": "brand new"}));
    let report = f.verify().await;
    assert!(report.ok, "{report:?}");
    assert_eq!(collection(&report, "notes").pending, 3);
}

#[tokio::test]
async fn another_accounts_key_cannot_read_the_copy() {
    let f = Fixture::new();
    f.filled();
    f.flush(100).await;
    let stranger = Content::generate();
    let engine = Engine {
        local: &f.local,
        transport: &f.worker,
        ledger: &f.ledger,
        content: &stranger,
        cas: false,
    };
    let report = engine.verify().await.unwrap().unwrap();
    assert!(!report.ok);
    let held = f.worker.records.lock().unwrap();
    for stored in held.values() {
        assert!(
            seal::open(
                &stranger,
                stored.collection,
                &stored.id,
                stored.revision,
                stored.schema,
                stored.sealed.as_ref().unwrap()
            )
            .is_none()
        );
    }
}

// --- primary mode: the account store as the source of truth of the configuration -----------

impl super::primary::Apply for Records {
    fn apply(
        &self,
        collection: Collection,
        changes: &[(String, Option<Value>)],
    ) -> anyhow::Result<()> {
        for (id, value) in changes {
            match value {
                Some(value) => self.set(collection, id, value.clone()),
                None => self.remove(collection, id),
            }
        }
        Ok(())
    }
}

/// Another installation of the same account: its own stores and ledger, the same key and Worker.
struct Installation {
    _dir: tempfile::TempDir,
    local: Records,
    ledger: Ledger,
}

impl Installation {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("mirror.db")).unwrap();
        Self {
            _dir: dir,
            local: Records::default(),
            ledger,
        }
    }
    fn engine<'a>(&'a self, f: &'a Fixture) -> Engine<'a, Records, Worker> {
        Engine {
            local: &self.local,
            transport: &f.worker,
            ledger: &self.ledger,
            content: &f.content,
            cas: true,
        }
    }
}

impl Fixture {
    fn primary(&self) -> Engine<'_, Records, Worker> {
        Engine {
            cas: true,
            ..self.engine()
        }
    }
    async fn pull(&self, replace: bool) -> super::primary::Pulled {
        self.primary()
            .pull(&self.local, replace)
            .await
            .unwrap()
            .unwrap()
    }
    fn value(&self, collection: Collection, id: &str) -> Option<Value> {
        self.local
            .collect(collection)
            .unwrap()
            .into_iter()
            .find(|e| e.id == id)
            .map(|e| e.value)
    }
}

#[tokio::test]
async fn another_installations_own_settings_are_kept_for_it_alone() {
    let f = Fixture::new();
    *f.local.1.lock().unwrap() = Some("a".into());
    f.local
        .set(Collection::Settings, "config", json!({"language": "es"}));
    f.local.set(
        Collection::Settings,
        "installation/a",
        json!({"server": {"bind": "a"}}),
    );
    f.flush(100).await;
    let other = Installation::new();
    *other.local.1.lock().unwrap() = Some("b".into());
    *f.worker.installation.lock().unwrap() = 2;
    other
        .engine(&f)
        .pull(&other.local, false)
        .await
        .unwrap()
        .unwrap();
    let ids = |local: &Records| -> Vec<String> {
        local
            .collect(Collection::Settings)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect()
    };
    assert_eq!(
        ids(&other.local),
        ["config"],
        "never another's own settings"
    );
    other.local.set(
        Collection::Settings,
        "installation/b",
        json!({"server": {"bind": "b"}}),
    );
    other
        .engine(&f)
        .flush(200, Limits::default())
        .await
        .unwrap();
    let held = |id: &str| {
        f.worker
            .records
            .lock()
            .unwrap()
            .get(&(
                Collection::Settings,
                seal::blind_id(&f.content, Collection::Settings, id),
            ))
            .is_some_and(|s| !s.deleted)
    };
    assert!(held("installation/a") && held("installation/b"));
    let theirs = other.engine(&f).verify().await.unwrap().unwrap();
    assert!(theirs.ok, "{theirs:?}");
    assert_eq!(collection(&theirs, "settings").others, 1);
    // Back on the first installation: the second's settings are neither installed nor removed.
    *f.worker.installation.lock().unwrap() = 0;
    assert_eq!(f.pull(false).await.applied, 0);
    assert_eq!(ids(&f.local), ["config", "installation/a"]);
    f.flush(300).await;
    assert!(held("installation/b"));
    let mine = f.verify().await;
    assert!(mine.ok, "{mine:?}");
    assert_eq!(collection(&mine, "settings").others, 1);
}

#[tokio::test]
async fn a_computer_signed_in_to_a_coordinator_mirrors_only_its_history() {
    let f = Fixture::new();
    f.local
        .set(Collection::Settings, "config", json!({"language": "es"}));
    f.flush(100).await;
    // Another computer of the account, with the key it received at trust time.
    let other = Installation::new();
    *other.local.2.lock().unwrap() = true;
    other
        .local
        .set(Collection::Settings, "config", json!({"language": "pt"}));
    other
        .local
        .set(Collection::Notes, "mine", json!({"title": "its own"}));
    other.local.set(
        Collection::Audits,
        "chats('z')/messages('9')",
        json!({"status": "sent"}),
    );
    *f.worker.installation.lock().unwrap() = 3;
    let sent = other
        .engine(&f)
        .flush(200, Limits::default())
        .await
        .unwrap();
    assert_eq!(sent.sent, 1, "only the message audit");
    let theirs = other.engine(&f).verify().await.unwrap().unwrap();
    assert!(theirs.ok, "{theirs:?}");
    assert_eq!(collection(&theirs, "settings").others, 1);
    // The account's configuration in the store is untouched.
    *f.worker.installation.lock().unwrap() = 0;
    assert_eq!(f.pull(false).await.applied, 0);
    assert_eq!(
        f.value(Collection::Settings, "config").unwrap(),
        json!({"language": "es"})
    );
    assert!(f.verify().await.ok);
}

use super::primary::{Change, Refusal};

fn change(collection: Collection, id: &str, value: Option<Value>) -> Change {
    Change {
        collection,
        id: id.into(),
        value,
    }
}

#[tokio::test]
async fn a_configuration_write_reaches_the_store_first_and_settles_after_the_local_save() {
    let f = Fixture::new();
    f.local
        .set(Collection::Settings, "config", json!({"language": "es"}));
    f.flush(100).await;
    let next = json!({"language": "en"});
    let accepted = f
        .primary()
        .write(&[change(Collection::Settings, "config", Some(next.clone()))])
        .await
        .unwrap()
        .unwrap();
    // The store has it before the local stores do; the ledger agrees only once settled.
    assert_eq!(accepted.rows.len(), 1);
    f.local.set(Collection::Settings, "config", next);
    accepted.settle(&f.ledger).unwrap();
    assert_eq!(f.ledger.outbox(None, 200).unwrap().pending, 0);
    assert!(f.verify().await.ok);
    // Writing the same content again sends nothing.
    let again = f
        .primary()
        .write(&[change(
            Collection::Settings,
            "config",
            Some(json!({"language": "en"})),
        )])
        .await
        .unwrap()
        .unwrap();
    assert!(again.rows.is_empty());
}

#[tokio::test]
async fn a_write_based_on_an_old_revision_is_a_conflict_and_changes_nothing() {
    let f = Fixture::new();
    f.local
        .set(Collection::Settings, "config", json!({"language": "es"}));
    f.local.set(
        Collection::Settings,
        "knowledge_map",
        json!({"contexts": []}),
    );
    f.flush(100).await;
    // Another installation pulls the same state, then changes the configuration first.
    let other = Installation::new();
    other
        .engine(&f)
        .pull(&other.local, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        other.local.collect(Collection::Settings).unwrap().len(),
        2,
        "a fresh installation receives the account's configuration"
    );
    let theirs = other
        .engine(&f)
        .write(&[change(
            Collection::Settings,
            "config",
            Some(json!({"language": "pt"})),
        )])
        .await
        .unwrap()
        .unwrap();
    other
        .local
        .set(Collection::Settings, "config", json!({"language": "pt"}));
    theirs.settle(&other.ledger).unwrap();

    // This installation still holds the old revision: its write is refused as a whole, the map
    // included, and the store keeps the other installation's change.
    let before = f.worker.records.lock().unwrap().len();
    let refused = f
        .primary()
        .write(&[
            change(
                Collection::Settings,
                "config",
                Some(json!({"language": "en"})),
            ),
            change(
                Collection::Settings,
                "knowledge_map",
                Some(json!({"contexts": ["x"]})),
            ),
        ])
        .await
        .unwrap();
    assert_eq!(refused.unwrap_err(), Refusal::Conflict);
    assert_eq!(f.worker.records.lock().unwrap().len(), before);
    // Synchronizing brings the newer configuration; the change can then be repeated.
    let pulled = f.pull(false).await;
    assert_eq!(pulled.applied, 1);
    assert_eq!(
        f.value(Collection::Settings, "config"),
        Some(json!({"language": "pt"}))
    );
    let accepted = f
        .primary()
        .write(&[change(
            Collection::Settings,
            "knowledge_map",
            Some(json!({"contexts": ["x"]})),
        )])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(accepted.rows.len(), 1);
}

#[tokio::test]
async fn a_local_change_the_store_never_acknowledged_is_never_overwritten_by_a_pull() {
    let f = Fixture::new();
    f.local
        .set(Collection::Notes, "n1", json!({"title": "one"}));
    f.flush(100).await;
    let other = Installation::new();
    other
        .engine(&f)
        .pull(&other.local, false)
        .await
        .unwrap()
        .unwrap();
    let theirs = other
        .engine(&f)
        .write(&[change(
            Collection::Notes,
            "n1",
            Some(json!({"title": "theirs"})),
        )])
        .await
        .unwrap()
        .unwrap();
    theirs.settle(&other.ledger).unwrap();

    // Here the note was edited without reaching the store (a hand edit while offline).
    f.local
        .set(Collection::Notes, "n1", json!({"title": "mine"}));
    let pulled = f.pull(false).await;
    assert_eq!(pulled.conflicts, 1);
    assert_eq!(
        f.value(Collection::Notes, "n1"),
        Some(json!({"title": "mine"}))
    );
    assert_eq!(f.ledger.conflicts().unwrap(), 1);
    // The outbox does not push it over the store's version, and a write-through waits too.
    let flushed = f.primary().flush(200, Limits::default()).await.unwrap();
    assert_eq!(flushed.sent, 0);
    let refused = f
        .primary()
        .write(&[change(
            Collection::Notes,
            "n1",
            Some(json!({"title": "again"})),
        )])
        .await
        .unwrap();
    assert_eq!(refused.unwrap_err(), Refusal::Unresolved);

    // Replacing adopts the store's version and clears the conflict.
    let replaced = f.pull(true).await;
    assert_eq!(replaced.applied, 1);
    assert_eq!(
        f.value(Collection::Notes, "n1"),
        Some(json!({"title": "theirs"}))
    );
    assert_eq!(f.ledger.conflicts().unwrap(), 0);
    assert!(f.verify().await.ok);
}

#[tokio::test]
async fn the_outbox_sends_configuration_naming_its_base_and_stops_at_a_conflict() {
    let f = Fixture::new();
    f.local.set(
        Collection::Organization,
        "organization",
        json!({"name": "a"}),
    );
    f.flush(100).await;
    let other = Installation::new();
    other
        .engine(&f)
        .pull(&other.local, false)
        .await
        .unwrap()
        .unwrap();
    other
        .engine(&f)
        .write(&[change(
            Collection::Organization,
            "organization",
            Some(json!({"name": "b"})),
        )])
        .await
        .unwrap()
        .unwrap()
        .settle(&other.ledger)
        .unwrap();
    // A local change found by the sweep (not written through) meets the newer store copy.
    f.local.set(
        Collection::Organization,
        "organization",
        json!({"name": "c"}),
    );
    let flushed = f.primary().flush(200, Limits::default()).await.unwrap();
    assert_eq!((flushed.sent, flushed.conflicts), (0, 1));
    let revision = f
        .worker
        .records
        .lock()
        .unwrap()
        .values()
        .find(|s| s.collection == Collection::Organization)
        .unwrap()
        .revision;
    assert_eq!(
        revision, 2,
        "the store keeps the other installation's version"
    );
    // Further passes do not hammer the store.
    let again = f.primary().flush(300, Limits::default()).await.unwrap();
    assert_eq!(again.batches, 0);
    // Other collections are still mirrored as before.
    f.local
        .set(Collection::Audits, "m1", json!({"status": "sent"}));
    let mirrored = f.primary().flush(400, Limits::default()).await.unwrap();
    assert_eq!(mirrored.sent, 1);
}

#[tokio::test]
async fn a_write_whose_answer_was_lost_appears_here_after_the_next_pull() {
    let f = Fixture::new();
    f.local
        .set(Collection::Settings, "config", json!({"language": "es"}));
    f.flush(100).await;
    *f.worker.lose_answer.lock().unwrap() = true;
    let lost = f
        .primary()
        .write(&[change(
            Collection::Settings,
            "config",
            Some(json!({"language": "en"})),
        )])
        .await
        .unwrap();
    assert_eq!(lost.unwrap_err(), Refusal::Failure(Failure::Offline));
    // Nothing was saved here, but the store received it: it is the source of truth.
    assert_eq!(
        f.value(Collection::Settings, "config"),
        Some(json!({"language": "es"}))
    );
    assert_eq!(f.pull(false).await.applied, 1);
    assert_eq!(
        f.value(Collection::Settings, "config"),
        Some(json!({"language": "en"}))
    );
    assert!(f.verify().await.ok);
}

#[tokio::test]
async fn removing_a_configuration_record_is_a_compare_and_swap_tombstone() {
    let f = Fixture::new();
    f.local
        .set(Collection::Notes, "n1", json!({"title": "one"}));
    f.local
        .set(Collection::Notes, "n2", json!({"title": "two"}));
    f.flush(100).await;
    let other = Installation::new();
    other
        .engine(&f)
        .pull(&other.local, false)
        .await
        .unwrap()
        .unwrap();
    let accepted = f
        .primary()
        .write(&[change(Collection::Notes, "n1", None)])
        .await
        .unwrap()
        .unwrap();
    f.local.remove(Collection::Notes, "n1");
    accepted.settle(&f.ledger).unwrap();
    // The other installation removes it on its next pull; an old copy cannot bring it back.
    let pulled = other
        .engine(&f)
        .pull(&other.local, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pulled.applied, 1);
    assert_eq!(other.local.collect(Collection::Notes).unwrap().len(), 1);
    let resurrect = other
        .engine(&f)
        .write(&[change(
            Collection::Notes,
            "n1",
            Some(json!({"title": "one"})),
        )])
        .await
        .unwrap()
        .unwrap();
    // It is a new version the user wrote, not an old copy: the store accepts it above the tombstone.
    assert_eq!(resurrect.rows[0].acked_revision, 3);
    // Removing a note the store never had sends nothing.
    let nothing = f
        .primary()
        .write(&[change(Collection::Notes, "never", None)])
        .await
        .unwrap()
        .unwrap();
    assert!(nothing.rows.is_empty());
}

#[tokio::test]
async fn only_configuration_is_pulled_and_records_of_another_key_are_not_applied() {
    let f = Fixture::new();
    f.filled();
    f.flush(100).await;
    let other = Installation::new();
    let pulled = other
        .engine(&f)
        .pull(&other.local, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        pulled.applied, 3,
        "the notes and the settings, never the audits"
    );
    assert!(other.local.collect(Collection::Audits).unwrap().is_empty());
    let stranger = Content::generate();
    let foreign = Installation::new();
    let engine = Engine {
        local: &foreign.local,
        transport: &f.worker,
        ledger: &foreign.ledger,
        content: &stranger,
        cas: true,
    };
    let pulled = engine.pull(&foreign.local, false).await.unwrap().unwrap();
    assert_eq!((pulled.applied, pulled.undecryptable), (0, 3));
    assert!(foreign.local.collect(Collection::Notes).unwrap().is_empty());
}
