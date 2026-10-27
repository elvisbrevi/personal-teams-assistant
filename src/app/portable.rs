//! Synchronization of the account's portable configuration between its clients.
//!
//! The coordinator (the host that owns the account) is authoritative and serves a revisioned,
//! allowlisted snapshot ([`crate::portable`]). An installation signed in to it *restores* that
//! snapshot into its own profile, keeps the revision it loaded next to the latest one the
//! coordinator reported, and forwards its own edits of portable fields to the coordinator under
//! optimistic concurrency before saving them locally. Nothing else crosses: paths, repository
//! bindings, roots, tunnels, credentials, Microsoft and provider sessions stay local, restoring
//! never starts Teams, installs or signs in to a provider, or enables an execution root, and a
//! conflict is reported rather than resolved by whichever side wrote last.
use super::{
    authority,
    control::{self, Reply},
    implementation::{self, Link},
    *,
};
use crate::{
    index::{Index, Note},
    portable::{self as core, Document, Patch, Unavailable},
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Instant};
#[cfg(test)]
use std::{future::Future, pin::Pin};

const AUTO_SECONDS: u64 = 30;
const ATTEMPTS: usize = 3;
const MAX_MESSAGE: usize = 400;
/// A configuration document larger than this is refused instead of truncated.
const MAX_DOCUMENT: usize = 900_000;
const KEEP_BACKUPS: usize = 3;

#[cfg(test)]
type Call = Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>>;
#[cfg(test)]
type Transport = Arc<dyn Fn(String, Value) -> Call + Send + Sync>;

/// A revision of the account configuration: a counter that only grows when the content changes,
/// and the digest of that content (which is what a change is checked against).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Revision {
    generation: u64,
    digest: String,
}
impl Revision {
    fn show(&self) -> Value {
        json!({"generation":self.generation,"digest":&self.digest[..self.digest.len().min(12)]})
    }
}

/// What an installation remembers about the account state it loaded.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Replica {
    schema: u32,
    link: Link,
    /// Opaque fingerprint of the account source that produced the snapshot.
    source: String,
    /// The revision installed in this profile.
    loaded: Revision,
    /// The newest revision the coordinator reported.
    saved: Revision,
    loaded_at: i64,
    saved_at: i64,
    /// Digest of this installation's own projection right after it loaded or forwarded a
    /// change; a different projection later means local edits that were never forwarded.
    applied: String,
    unavailable: Unavailable,
}

#[derive(Default)]
struct Status {
    syncing: bool,
    attempted_at: Option<i64>,
    error: Option<(&'static str, &'static str)>,
    conflict: Option<String>,
    automatic_at: Option<Instant>,
    local_changes: bool,
}

pub(super) struct State {
    dir: PathBuf,
    status: std::sync::Mutex<Status>,
    gate: tokio::sync::Mutex<()>,
    /// Serializes the coordinator's revision bookkeeping.
    revisions: std::sync::Mutex<()>,
    #[cfg(test)]
    transport: std::sync::Mutex<Option<Transport>>,
}

impl State {
    pub(super) fn new(profile: &Path) -> Self {
        Self {
            dir: profile.join("portable"),
            status: std::sync::Mutex::new(Status::default()),
            gate: tokio::sync::Mutex::new(()),
            revisions: std::sync::Mutex::new(()),
            #[cfg(test)]
            transport: std::sync::Mutex::new(None),
        }
    }
    fn status(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn replica_path(&self) -> PathBuf {
        self.dir.join("replica.json")
    }
    /// The installation left or changed its account: forget what was loaded from it. The
    /// restored configuration itself stays (it is the user's profile now).
    pub(super) fn forget(&self) {
        let _ = fs::remove_file(self.replica_path());
        *self.status() = Status::default();
    }
    /// The remembered state, only if it was produced for exactly this coordinator, account and
    /// machine; anything else is discarded rather than reused.
    fn load(&self, link: &Link) -> Option<Replica> {
        let bytes = fs::read(self.replica_path()).ok()?;
        let replica = serde_json::from_slice::<Replica>(&bytes)
            .ok()
            .filter(|r| r.schema == core::SCHEMA && r.link == *link);
        if replica.is_none() {
            let _ = fs::remove_file(self.replica_path());
            // Whatever was reported belonged to the account that was left.
            *self.status() = Status::default();
        }
        replica
    }
    fn save(&self, replica: &Replica) -> anyhow::Result<()> {
        security::private_dir(&self.dir)?;
        write_private(&self.replica_path(), &serde_json::to_string(replica)?)
    }
    #[cfg(test)]
    pub(super) fn use_transport(&self, transport: Transport) {
        *self.transport.lock().unwrap() = Some(transport);
    }
}

pub(super) fn handles(method: &str) -> bool {
    method.starts_with("portable_")
}

// ---------------------------------------------------------------------------------------------
// This installation's own state.

/// The profile's configuration, knowledge map and notes.
fn local(state: &DesktopState) -> anyhow::Result<(Config, KnowledgeMap, Vec<Note>)> {
    let config = read_config(&state.config_path)?;
    let map = KnowledgeMap::load(&state.map_path)?;
    let index = Index::open(&config.server.data_dir.join(crate::index::FILE))?;
    let notes = index.notes(None, core::MAX_NOTES + 1)?;
    Ok((config, map, notes))
}
fn local_digest(state: &DesktopState) -> anyhow::Result<String> {
    let (config, map, notes) = local(state)?;
    Ok(Document::project(&config, &map).digest(&notes))
}
/// The digest of a profile nobody has configured yet.
fn pristine_digest() -> String {
    let mut map = KnowledgeMap::parse("").expect("the empty map is valid");
    map.initialize_questions();
    let config = Config::desktop_template().expect("the template is valid");
    Document::project(&config, &map).digest(&[])
}
/// Whether a source from the account can be used with this computer's own bindings.
fn resolves(map: &KnowledgeMap, source: &crate::knowledge::Resource) -> bool {
    match core::alias(&source.access) {
        None => true,
        Some(alias) => {
            map.repositories.contains_key(alias)
                && match &source.access {
                    Access::File { repository, path } => {
                        crate::knowledge::read_repository_file(&map.repositories, repository, path)
                            .is_ok()
                    }
                    _ => true,
                }
        }
    }
}

/// What a note looked like when a client read it: a change since then is a conflict, however
/// quickly it followed (timestamps have a resolution of one second).
fn token(note: &Note) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(serde_json::to_vec(note).unwrap_or_default());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// An opaque fingerprint of the account this host serves, so a client notices when the
/// coordinator's configuration stops coming from the account it loaded.
fn source(host: &Host) -> String {
    use sha2::{Digest, Sha256};
    let account = host
        .state
        .authority
        .account
        .as_ref()
        .map_or("owner", |a| a.id.as_str());
    let digest = Sha256::digest(
        [
            b"personal-teams-assistant/portable-configuration-source/v1".as_slice(),
            account.as_bytes(),
        ]
        .concat(),
    );
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------------------------
// Coordinator side: what a trusted machine may ask about its account's configuration.

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Counter {
    generation: u64,
    digest: String,
}

/// The account's current revision. The generation moves forward whenever the content differs
/// from the last one observed, however that change was made.
fn current(state: &DesktopState, digest: &str) -> anyhow::Result<Revision> {
    let _guard = state
        .portable
        .revisions
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let path = state.portable.dir.join("revision.json");
    let last = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Counter>(&b).ok());
    let counter = match last {
        Some(c) if c.digest == digest => c,
        other => {
            let counter = Counter {
                generation: other.map_or(1, |c| c.generation + 1),
                digest: digest.to_owned(),
            };
            security::private_dir(&state.portable.dir)?;
            write_private(&path, &serde_json::to_string(&counter)?)?;
            counter
        }
    };
    Ok(Revision {
        generation: counter.generation,
        digest: counter.digest,
    })
}

fn account_view(
    state: &DesktopState,
) -> anyhow::Result<(Config, KnowledgeMap, Vec<Note>, Document, Revision)> {
    let (config, map, notes) = local(state)?;
    let document = Document::project(&config, &map);
    let revision = current(state, &document.digest(&notes))?;
    Ok((config, map, notes, document, revision))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotArgs {
    section: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyArgs {
    base: String,
    patch: Patch,
    #[serde(default)]
    unavailable: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteSaveArgs {
    note: Note,
    #[serde(default)]
    expected: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteRemoveArgs {
    id: String,
    expected: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordsArgs {
    kind: String,
    #[serde(default)]
    day: Option<String>,
    #[serde(default)]
    from: Option<String>,
}

fn invalid(message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("[invalid_input] {message}")
}
fn conflict(message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("[state_conflict] {message}")
}
fn parse<T: serde::de::DeserializeOwned>(args: &Value) -> anyhow::Result<T> {
    serde_json::from_value(args.clone())
        .map_err(|_| invalid("Invalid portable configuration request."))
}

/// What a trusted machine may ask the coordinator about its account's configuration: the
/// snapshot (allowlisted fields, in pages), versioned changes and the account's retained
/// records. Never a path, credential, key or provider session.
pub(in crate::app) async fn machine_call(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> anyhow::Result<Value> {
    let invocation = authority::invocation(&host.state)?;
    ensure!(
        invocation.transport == authority::Transport::Machine,
        "Unavailable coordinator operation."
    );
    let state = &host.state;
    match method {
        "portable_snapshot" => {
            let SnapshotArgs {
                section,
                offset,
                limit,
            } = parse(args)?;
            let (_, _, notes, document, revision) = account_view(state)?;
            document.check().map_err(|e| {
                invalid(format!(
                    "The account configuration cannot be synchronized: {e}."
                ))
            })?;
            ensure!(
                notes.len() <= core::MAX_NOTES,
                "[invalid_input] There are too many notes to synchronize ({} at most).",
                core::MAX_NOTES
            );
            let head = json!({
                "schema": core::SCHEMA,
                "source": source(host),
                "revision": revision,
            });
            let mut reply = head;
            match section.as_str() {
                "config" => {
                    ensure!(
                        serde_json::to_vec(&document)?.len() <= MAX_DOCUMENT,
                        "[invalid_input] The account configuration is too large to synchronize."
                    );
                    reply["document"] = serde_json::to_value(&document)?;
                    reply["notes_total"] = json!(notes.len());
                }
                "notes" => {
                    let mut notes = notes;
                    notes.sort_by(|a, b| a.id.cmp(&b.id));
                    let limit = limit.unwrap_or(core::NOTES_PAGE).clamp(1, core::NOTES_PAGE);
                    reply["total"] = json!(notes.len());
                    reply["offset"] = json!(offset);
                    reply["items"] = json!(
                        notes
                            .into_iter()
                            .skip(offset)
                            .take(limit)
                            .collect::<Vec<_>>()
                    );
                }
                _ => return Err(invalid("Unknown snapshot section.")),
            }
            Ok(reply)
        }
        "portable_apply" => {
            let ApplyArgs {
                base,
                patch,
                unavailable,
            } = parse(args)?;
            let _guard = state.operations.lock().await;
            let (config, map, notes, mut document, before) = account_view(state)?;
            if before.digest != base {
                return Err(conflict(format!(
                    "The account configuration changed (now revision {}). Synchronize, review it and repeat the change.",
                    before.generation
                )));
            }
            ensure!(
                !patch.is_empty(),
                "[invalid_input] The request changes nothing."
            );
            patch
                .apply_to(&mut document, &unavailable.into_iter().collect())
                .map_err(invalid)?;
            document
                .check()
                .map_err(|e| invalid(format!("The change is invalid: {e}.")))?;
            let (next_config, next_map, _) = document.install(&config, &map, |_| true);
            validate_local(&next_config, &next_map).map_err(|_| {
                invalid("The change is invalid for the account's own installation.")
            })?;
            let tunnel = fs::read_to_string(&state.tunnel_path).unwrap_or_default();
            commit_settings(state, &next_config, &next_map, &tunnel)?;
            forget_removed_sources(&next_config, &next_map);
            *state.activity_graph.lock().await = None;
            let (_, _, _, _, after) = account_view(state)?;
            let _ = notes;
            Ok(json!({
                "previous": before,
                "revision": after,
                "restart_required": state.running.lock().await.is_some(),
            }))
        }
        "portable_note_save" => {
            let NoteSaveArgs { note, expected } = parse(args)?;
            let _guard = state.operations.lock().await;
            let (config, _, _, _, before) = account_view(state)?;
            let index = Index::open(&config.server.data_dir.join(crate::index::FILE))?;
            let held = if note.id.is_empty() {
                None
            } else {
                Some(
                    index
                        .note(&note.id)?
                        .ok_or_else(|| invalid("Unknown note."))?,
                )
            };
            if held.as_ref().map(token) != expected {
                return Err(conflict(
                    "The note changed elsewhere. Synchronize, read it again and repeat the change.",
                ));
            }
            let saved = notes::save(state, &json!({"note": note}))
                .map_err(|m| invalid(m.split_once("] ").map_or(m.as_str(), |(_, text)| text)))?;
            let (_, _, _, _, after) = account_view(state)?;
            Ok(json!({"note": saved, "previous": before, "revision": after}))
        }
        "portable_note_remove" => {
            let NoteRemoveArgs { id, expected } = parse(args)?;
            let _guard = state.operations.lock().await;
            let (config, _, _, _, before) = account_view(state)?;
            let index = Index::open(&config.server.data_dir.join(crate::index::FILE))?;
            let held = index.note(&id)?.ok_or_else(|| invalid("Unknown note."))?;
            if token(&held) != expected {
                return Err(conflict(
                    "The note changed elsewhere. Synchronize, read it again and repeat the change.",
                ));
            }
            notes::remove(state, &json!({"id": id}))
                .map_err(|m| invalid(m.split_once("] ").map_or(m.as_str(), |(_, text)| text)))?;
            let (_, _, _, _, after) = account_view(state)?;
            Ok(json!({"removed": id, "previous": before, "revision": after}))
        }
        "portable_records" => {
            let RecordsArgs { kind, day, from } = parse(args)?;
            let value = match kind.as_str() {
                "activity_status" | "activity_history" => {
                    activity::inspect(state, day.as_deref(), from.as_deref(), false)
                        .await
                        .map_err(invalid)?
                }
                _ => return Err(invalid("Unknown record kind.")),
            };
            ensure!(
                serde_json::to_vec(&value)?.len() <= MAX_DOCUMENT,
                "[invalid_input] The record is too large; ask for one day."
            );
            Ok(value)
        }
        _ => anyhow::bail!("Unavailable coordinator operation."),
    }
}

// ---------------------------------------------------------------------------------------------
// Installation side.

struct Failure {
    code: &'static str,
    exit: i32,
    message: &'static str,
}
const REVOKED: Failure = Failure {
    code: "not_ready",
    exit: 3,
    message: standing::REVOKED,
};
const UNREACHABLE: Failure = Failure {
    code: "dependency_or_network",
    exit: 5,
    message: "The coordinator could not be reached, or this installation is no longer connected to it. This computer's configuration is unchanged.",
};
const UNSUPPORTED: Failure = Failure {
    code: "not_ready",
    exit: 3,
    message: "The coordinator does not support configuration synchronization yet. Update its host and CLI to the same release.",
};
const REFUSED: Failure = Failure {
    code: "operation_failed",
    exit: 1,
    message: "The coordinator refused the synchronization. Sign this installation in again with Connect this machine.",
};
const INVALID: Failure = Failure {
    code: "operation_failed",
    exit: 1,
    message: "The coordinator returned an invalid or oversized configuration. Nothing was changed.",
};
const STALE: Failure = Failure {
    code: "state_conflict",
    exit: 6,
    message: "The coordinator reports an older configuration than this installation already saw (a restored or replayed state). Nothing was changed; once you confirm the coordinator was restored, repeat with reset.",
};
const SOURCE: Failure = Failure {
    code: "state_conflict",
    exit: 6,
    message: "The coordinator now serves a different account than the one this configuration came from. Nothing was changed and no other account was selected; repeat with reset to start from the coordinator's configuration.",
};
const CHANGING: Failure = Failure {
    code: "state_conflict",
    exit: 6,
    message: "The configuration kept changing while it was read. Synchronize again in a moment.",
};
const LOCAL_CHANGES: Failure = Failure {
    code: "state_conflict",
    exit: 6,
    message: "This computer has configuration changes that were never saved to the account (or it was configured before it signed in). Nothing was replaced. Repeat with reset to replace them with the account's configuration; the previous portable settings are kept in a private backup first.",
};
const CANNOT_APPLY: Failure = Failure {
    code: "operation_failed",
    exit: 1,
    message: "The account configuration cannot be applied with this computer's repositories and settings. Nothing was changed.",
};
const NOT_LOADED: &str = "[conflict] This installation has not loaded the account configuration yet. Synchronize first (pta account sync); nothing was saved.";
const BEHIND: &str = "[conflict] The account configuration changed elsewhere. Synchronize, review it and repeat the change; nothing was saved.";
const DIVERGED: &str = "[conflict] This computer has configuration changes that were never saved to the account. Synchronize with reset to adopt the account's configuration; nothing was saved.";
const OFFLINE: &str = "[network] The coordinator could not be reached, so the account configuration was not changed. Local-only settings can still be saved.";

fn clean(message: &str) -> String {
    message
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_MESSAGE)
        .collect()
}

async fn call_coordinator(
    state: &DesktopState,
    method: &str,
    args: &Value,
) -> anyhow::Result<Option<Value>> {
    #[cfg(test)]
    {
        let transport = state.portable.transport.lock().unwrap().clone();
        if let Some(transport) = transport {
            return transport(method.into(), args.clone()).await.map(Some);
        }
    }
    implementation::fleet::account_reply(state, method, args).await
}

/// The failure of an exchange that did not return: a credential the coordinator no longer
/// accepts is told apart from an outage.
fn unreachable(error: &anyhow::Error) -> Failure {
    match standing::refusal(error) {
        Some(standing::Refusal::Rejected) => REVOKED,
        _ => UNREACHABLE,
    }
}

struct Fetched {
    source: String,
    revision: Revision,
    document: Document,
    notes: Vec<Note>,
}

async fn fetch_part(
    state: &DesktopState,
    section: &str,
    offset: usize,
) -> std::result::Result<Value, Failure> {
    let reply = call_coordinator(
        state,
        "portable_snapshot",
        &json!({"section":section,"offset":offset,"limit":core::NOTES_PAGE}),
    )
    .await
    .map_err(|error| unreachable(&error))?
    .ok_or(UNREACHABLE)?;
    if reply["ok"] != true {
        let unsupported = reply["message"]
            .as_str()
            .is_some_and(|m| m.contains("Unavailable coordinator operation"));
        return Err(if unsupported { UNSUPPORTED } else { REFUSED });
    }
    let data = reply["data"].clone();
    if data["schema"] != core::SCHEMA {
        return Err(UNSUPPORTED);
    }
    Ok(data)
}

async fn fetch(state: &DesktopState) -> std::result::Result<Fetched, Failure> {
    'attempt: for _ in 0..ATTEMPTS {
        let head = fetch_part(state, "config", 0).await?;
        let revision: Revision =
            serde_json::from_value(head["revision"].clone()).map_err(|_| INVALID)?;
        let source = head["source"].as_str().unwrap_or_default().to_owned();
        let document: Document =
            serde_json::from_value(head["document"].clone()).map_err(|_| INVALID)?;
        let total = head["notes_total"].as_u64().ok_or(INVALID)? as usize;
        if source.len() != 32
            || !source.bytes().all(|b| b.is_ascii_hexdigit())
            || total > core::MAX_NOTES
            || revision.digest.len() != 64
        {
            return Err(INVALID);
        }
        document.check().map_err(|_| INVALID)?;
        let mut notes: Vec<Note> = Vec::new();
        for _ in 0..core::MAX_NOTE_PAGES {
            if notes.len() >= total {
                break;
            }
            let page = fetch_part(state, "notes", notes.len()).await?;
            if page["source"] != source.as_str() {
                return Err(SOURCE);
            }
            let page_revision: Revision =
                serde_json::from_value(page["revision"].clone()).map_err(|_| INVALID)?;
            if page_revision != revision || page["total"].as_u64() != Some(total as u64) {
                continue 'attempt;
            }
            let items: Vec<Note> =
                serde_json::from_value(page["items"].clone()).map_err(|_| INVALID)?;
            if items.is_empty()
                || items.len() > core::NOTES_PAGE
                || page["offset"].as_u64() != Some(notes.len() as u64)
            {
                return Err(INVALID);
            }
            notes.extend(items);
        }
        if notes.len() != total {
            return Err(INVALID);
        }
        let contexts: BTreeSet<&str> = document.contexts.iter().map(|c| c.id.as_str()).collect();
        core::check_notes(&notes, &contexts).map_err(|_| INVALID)?;
        // The digest the coordinator reported must be the one of the content received.
        if document.digest(&notes) != revision.digest {
            return Err(INVALID);
        }
        return Ok(Fetched {
            source,
            revision,
            document,
            notes,
        });
    }
    Err(CHANGING)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The user, an agent or a completed change asked: waits for any attempt in flight.
    Explicit,
    /// The background refresh: skipped while another attempt is running.
    Automatic,
}

fn failure_reply(failure: &Failure) -> Reply {
    Reply::error(failure.code, failure.exit, failure.message)
}

/// Keep the portable parts being replaced in a private file, so `reset` never destroys the only
/// copy of something the user configured here.
fn back_up(state: &DesktopState, config: &Config, map: &KnowledgeMap, notes: &[Note]) {
    let dir = state.portable.dir.join("backups");
    let write = || -> anyhow::Result<()> {
        security::private_dir(&dir)?;
        let document = Document::project(config, map);
        let file = dir.join(format!("replaced-{}.json", now()));
        write_private(
            &file,
            &serde_json::to_string_pretty(&json!({"document":document,"notes":notes}))?,
        )?;
        let mut files: Vec<_> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        files.sort();
        while files.len() > KEEP_BACKUPS {
            let _ = fs::remove_file(files.remove(0));
        }
        Ok(())
    };
    if write().is_err() {
        tracing::warn!(event = "portable_backup_failed");
    }
}

/// Bring this installation to the coordinator's current configuration, or leave it exactly as
/// it was. Local changes that were never saved to the account are never overwritten without
/// `reset`.
async fn synchronize(host: &Arc<Host>, link: &Link, reset: bool, mode: Mode) -> Reply {
    let state = &host.state;
    let portable = &state.portable;
    let _gate = match mode {
        Mode::Explicit => portable.gate.lock().await,
        Mode::Automatic => match portable.gate.try_lock() {
            Ok(gate) => gate,
            Err(_) => return Reply::success(json!({"sync": describe(host, link, false).await})),
        },
    };
    {
        let mut status = portable.status();
        status.syncing = true;
        status.attempted_at = Some(now());
    }
    let result = async {
        let fetched = fetch(state).await?;
        // The connection may have changed while pages were in flight: never install a
        // configuration under a link it was not read for.
        if implementation::link(state).ok().flatten().as_ref() != Some(link) {
            return Err(UNREACHABLE);
        }
        let replica = if reset { None } else { portable.load(link) };
        if let Some(r) = &replica {
            if r.source != fetched.source {
                return Err(SOURCE);
            }
            if fetched.revision.generation < r.saved.generation {
                return Err(STALE);
            }
        }
        let _operations = state.operations.lock().await;
        let (config, map, notes) = local(state).map_err(|_| CANNOT_APPLY)?;
        let digest = Document::project(&config, &map).digest(&notes);
        let diverged = match &replica {
            Some(r) => digest != r.applied,
            None => digest != pristine_digest(),
        };
        let remember =
            |loaded: Revision, loaded_at: i64, applied: String, unavailable: Unavailable| Replica {
                schema: core::SCHEMA,
                link: link.clone(),
                source: fetched.source.clone(),
                saved: fetched.revision.clone(),
                saved_at: now(),
                loaded,
                loaded_at,
                applied,
                unavailable,
            };
        if let Some(r) = replica.as_ref().filter(|_| !diverged)
            && r.loaded == fetched.revision
        {
            let kept = remember(
                r.loaded.clone(),
                r.loaded_at,
                r.applied.clone(),
                r.unavailable.clone(),
            );
            portable.save(&kept).map_err(|_| CANNOT_APPLY)?;
            portable.status().local_changes = false;
            return Ok(json!({"changed":false}));
        }
        if diverged && !reset {
            if let Some(r) = &replica {
                let kept = remember(
                    r.loaded.clone(),
                    r.loaded_at,
                    r.applied.clone(),
                    r.unavailable.clone(),
                );
                let _ = portable.save(&kept);
            }
            portable.status().local_changes = true;
            return Err(LOCAL_CHANGES);
        }
        let (next_config, next_map, unavailable) =
            fetched
                .document
                .install(&config, &map, |source| resolves(&map, source));
        validate_local(&next_config, &next_map).map_err(|_| CANNOT_APPLY)?;
        let contexts: BTreeSet<&str> = next_map.contexts.iter().map(|c| c.id.as_str()).collect();
        core::check_notes(&fetched.notes, &contexts).map_err(|_| CANNOT_APPLY)?;
        if diverged {
            back_up(state, &config, &map, &notes);
        }
        let index = Index::open(&next_config.server.data_dir.join(crate::index::FILE))
            .map_err(|_| CANNOT_APPLY)?;
        let tunnel = fs::read_to_string(&state.tunnel_path).unwrap_or_default();
        commit_settings(state, &next_config, &next_map, &tunnel).map_err(|_| CANNOT_APPLY)?;
        index
            .replace_notes(&fetched.notes)
            .map_err(|_| CANNOT_APPLY)?;
        forget_removed_sources(&next_config, &next_map);
        let applied = local_digest(state).map_err(|_| CANNOT_APPLY)?;
        let loaded = fetched.revision.clone();
        portable
            .save(&remember(loaded, now(), applied, unavailable))
            .map_err(|_| CANNOT_APPLY)?;
        portable.status().local_changes = false;
        Ok(json!({"changed":true,"notes":fetched.notes.len()}))
    }
    .await;
    {
        let mut status = portable.status();
        status.syncing = false;
        match &result {
            Ok(changes) => {
                status.error = None;
                if mode == Mode::Explicit || changes["changed"] == true {
                    status.conflict = None;
                }
            }
            // Unsaved local changes are a state of their own (`diverged`), not a failure.
            Err(failure) if failure.message == LOCAL_CHANGES.message => status.error = None,
            Err(failure) => status.error = Some((failure.code, failure.message)),
        }
    }
    match result {
        Ok(changes) => {
            Reply::success(json!({"sync": describe(host, link, false).await, "changes": changes}))
        }
        Err(failure) => failure_reply(&failure),
    }
}

/// Background refresh, called by the connected installation's regular coordinator poll.
pub(in crate::app) fn auto(host: &Arc<Host>) {
    let Ok(Some(link)) = implementation::link(&host.state) else {
        return;
    };
    {
        let mut status = host.state.portable.status();
        if status
            .automatic_at
            .is_some_and(|at| at.elapsed().as_secs() < AUTO_SECONDS)
        {
            return;
        }
        status.automatic_at = Some(Instant::now());
    }
    let host = host.clone();
    tokio::spawn(async move {
        let _ = synchronize(&host, &link, false, Mode::Automatic).await;
    });
}

/// Is the provider of a model choice usable on this computer? Only looks; never installs or
/// signs in.
fn provider_ready(choice: &crate::config::LlmChoice) -> bool {
    match choice.provider.as_str() {
        "deepseek" => security::secret_source("DEEPSEEK_API_KEY")
            .ok()
            .flatten()
            .is_some(),
        name => {
            let binary = if name == "claude" { "claude" } else { name };
            std::env::var_os("PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|dir| {
                    ["", ".exe", ".cmd"]
                        .iter()
                        .any(|ext| dir.join(format!("{binary}{ext}")).is_file())
                })
            })
        }
    }
}

/// What the account configuration still needs from this computer, as opposed to what it
/// restored: repository bindings, providers and sign-ins stay local setup.
async fn local_setup(host: &Host, unavailable: &Unavailable) -> Vec<Value> {
    let mut items = Vec::new();
    let aliases: BTreeSet<&str> = unavailable
        .sources
        .iter()
        .map(|s| s.repository.as_str())
        .collect();
    for alias in aliases {
        items.push(json!({
            "id": format!("repository:{alias}"),
            "message": format!("Connect the repository «{alias}» on this computer; the sources that read it are unavailable until then."),
        }));
    }
    if let Ok(config) = read_config(&host.state.config_path) {
        // An ordinary portal account never learns what is installed on the operator's computer.
        let owner = authority::invocation(&host.state).is_ok_and(|i| i.scope.owner);
        for choice in config
            .llm
            .chain
            .iter()
            .filter(|c| c.enabled && (owner || c.provider == "deepseek"))
        {
            if !provider_ready(choice) {
                items.push(json!({
                    "id": format!("provider:{}", choice.provider),
                    "message": format!("The model provider «{}» is part of the account's configuration but is not installed or signed in on this computer; nothing else is used in its place.", choice.provider),
                }));
            }
        }
        if !crate::state::Store::has_token(&config.server.data_dir.join("assistant.db"))
            .unwrap_or(false)
        {
            items.push(json!({"id":"microsoft","message":"Sign in to Microsoft on this computer to use Teams; the account's configuration does not include a Microsoft session."}));
        }
    }
    if implementation::roots(host)
        .await
        .is_ok_and(|roots| roots.is_empty())
    {
        items.push(json!({"id":"roots","message":"No directory is selected for coding agents on this computer; registering it does not grant access."}));
    }
    items
}

/// What a client may show about synchronization: never a path, credential or provider session.
pub(in crate::app) async fn describe(host: &Host, link: &Link, fresh: bool) -> Value {
    let portable = &host.state.portable;
    let replica = portable.load(link);
    let changes = if fresh {
        match (&replica, local_digest(&host.state)) {
            (Some(r), Ok(digest)) => digest != r.applied,
            (None, Ok(digest)) => digest != pristine_digest(),
            _ => false,
        }
    } else {
        portable.status().local_changes
    };
    let unavailable = replica
        .as_ref()
        .map(|r| r.unavailable.clone())
        .unwrap_or_default();
    let setup = local_setup(host, &unavailable).await;
    let status = portable.status();
    let state = if status.syncing {
        "syncing"
    } else if status.conflict.is_some() {
        "conflict"
    } else if status.error.is_some() {
        "error"
    } else {
        match &replica {
            None => "never",
            Some(_) if changes => "diverged",
            Some(r) if r.loaded != r.saved => "stale",
            Some(_) => "synced",
        }
    };
    json!({
        "mode": "replica",
        "state": state,
        "coordinator": link.origin,
        "loaded": replica.as_ref().map(|r| r.loaded.show()),
        "saved": replica.as_ref().map(|r| r.saved.show()),
        "synced_at": replica.as_ref().map(|r| r.loaded_at),
        "checked_at": replica.as_ref().map(|r| r.saved_at),
        "attempted_at": status.attempted_at,
        "error": status.error.map(|(code, message)| json!({"code":code,"message":message})),
        "conflict": status.conflict,
        "local_changes": changes,
        "unavailable": unavailable,
        "local_setup": setup,
        "portable": {"settings": core::SETTINGS_FIELDS, "collections": core::COLLECTIONS},
    })
}

/// How a configuration that is its own authority describes itself.
async fn authoritative(host: &Host) -> anyhow::Result<Value> {
    let (_, _, notes, document, revision) = account_view(&host.state)?;
    let local = local_setup(host, &Unavailable::default()).await;
    Ok(json!({
        "mode": "authoritative",
        "state": "authoritative",
        "revision": revision.show(),
        "sources": document.sources.len(),
        "contexts": document.contexts.len(),
        "questions": document.questions.len(),
        "notes": notes.len(),
        "local_setup": local,
        "portable": {"settings": core::SETTINGS_FIELDS, "collections": core::COLLECTIONS},
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncArgs {
    #[serde(default)]
    reset: bool,
}

fn refused(message: &str) -> Reply {
    Reply::error("operation_failed", 1, message)
}

/// `portable_sync`, `portable_status` and `portable_records` for any client of this host.
pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Reply {
    let method = request.method.as_str();
    let invocation = match authority::invocation(&host.state) {
        Ok(invocation) => invocation,
        Err(_) => return Reply::error("state_conflict", 6, authority::ACCOUNT_UNAVAILABLE),
    };
    if invocation.transport == authority::Transport::Machine {
        return refused("This operation is not available from this client.");
    }
    let link = match implementation::link(&host.state) {
        Ok(link) => link,
        Err(_) => return refused("The coordinator connection is unreadable."),
    };
    match method {
        "portable_sync" | "portable_status" => {
            let args: SyncArgs = match parse(&request.args) {
                Ok(args) => args,
                Err(_) => return Reply::error("invalid_input", 2, "Invalid arguments."),
            };
            if method == "portable_status" && request.args != json!({}) {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            }
            let Some(link) = link else {
                // Coordinator or standalone installation: its own configuration is the authority.
                return match authoritative(host).await {
                    Ok(sync) => Reply::success(
                        json!({"sync": sync, "standing": host.state.standing.view(None)}),
                    ),
                    Err(_) => refused("The configuration could not be read."),
                };
            };
            if method == "portable_status" {
                return Reply::success(json!({
                    "sync": describe(host, &link, true).await,
                    "standing": host.state.standing.view(Some(&link)),
                }));
            }
            synchronize(host, &link, args.reset, Mode::Explicit).await
        }
        "portable_records" => {
            let Some(_) = link else {
                return Reply::error(
                    "not_ready",
                    3,
                    "This installation is the account's coordinator or is not signed in; read its records directly.",
                );
            };
            let args: RecordsArgs = match parse(&request.args) {
                Ok(args) => args,
                Err(_) => return Reply::error("invalid_input", 2, "Invalid arguments."),
            };
            let outcome = match call_coordinator(
                &host.state,
                "portable_records",
                &json!({"kind":args.kind,"day":args.day,"from":args.from}),
            )
            .await
            {
                Ok(Some(outcome)) => outcome,
                Err(error) => return failure_reply(&unreachable(&error)),
                _ => return failure_reply(&UNREACHABLE),
            };
            if outcome["ok"] != true {
                let message = clean(outcome["message"].as_str().unwrap_or_default());
                return if outcome["code"] == "invalid_input" {
                    Reply::error("invalid_input", 2, &message)
                } else {
                    refused("The coordinator could not read that record.")
                };
            }
            Reply::success(
                json!({"kind":args.kind,"source":"coordinator","record":outcome["data"]}),
            )
        }
        _ => Reply::error("invalid_input", 2, "Unknown operation."),
    }
}

// ---------------------------------------------------------------------------------------------
// Edits made on a connected installation.

/// A change the coordinator accepted, waiting for the local save that follows it.
pub(super) struct Forwarded {
    previous: Revision,
    revision: Revision,
}

fn rejected(state: &DesktopState, outcome: &Value) -> String {
    let message = clean(outcome["message"].as_str().unwrap_or_default());
    match outcome["code"].as_str() {
        Some("state_conflict") => {
            state.portable.status().conflict = Some(message.clone());
            format!("[conflict] {message}")
        }
        Some("invalid_input") => format!("[invalid_input] {message}"),
        _ => "The coordinator could not apply the change; nothing was saved.".into(),
    }
}

/// A usable memory of the account state this installation loaded, for an edit to build on.
fn ready(
    state: &DesktopState,
    link: &Link,
    notes_edit: bool,
) -> std::result::Result<Replica, String> {
    let replica = state.portable.load(link).ok_or(NOT_LOADED)?;
    if !notes_edit && replica.loaded != replica.saved {
        return Err(BEHIND.into());
    }
    if local_digest(state).is_ok_and(|digest| digest != replica.applied) {
        state.portable.status().local_changes = true;
        return Err(DIVERGED.into());
    }
    Ok(replica)
}

/// Settings, sources, contexts and questions go to the coordinator before they are saved here.
/// `Ok(None)`: nothing portable changed (or this installation has no coordinator).
pub(super) async fn before_save(
    state: &DesktopState,
    previous: (&Config, &KnowledgeMap),
    next: (&Config, &KnowledgeMap),
) -> std::result::Result<Option<Forwarded>, String> {
    let Some(link) = implementation::link(state).map_err(fail)? else {
        return Ok(None);
    };
    let patch = Patch::between(
        &Document::project(previous.0, previous.1),
        &Document::project(next.0, next.1),
    );
    if patch.is_empty() {
        return Ok(None);
    }
    let replica = ready(state, &link, false)?;
    let outcome = match call_coordinator(
        state,
        "portable_apply",
        &json!({
            "base": replica.loaded.digest,
            "patch": patch,
            "unavailable": replica.unavailable.ids(),
        }),
    )
    .await
    {
        Ok(Some(outcome)) => outcome,
        _ => return Err(OFFLINE.into()),
    };
    if outcome["ok"] != true {
        return Err(rejected(state, &outcome));
    }
    let data = &outcome["data"];
    match (
        serde_json::from_value(data["previous"].clone()),
        serde_json::from_value(data["revision"].clone()),
    ) {
        (Ok(previous), Ok(revision)) => Ok(Some(Forwarded { previous, revision })),
        _ => Err("The coordinator returned an unexpected reply; check the account configuration before repeating.".into()),
    }
}

/// Record that the coordinator's new revision is now the one this installation has loaded.
pub(super) fn after_save(state: &DesktopState, forwarded: Option<Forwarded>) {
    if let Some(forwarded) = forwarded {
        settle(state, &forwarded);
    }
}
fn settle(state: &DesktopState, forwarded: &Forwarded) {
    let Ok(Some(link)) = implementation::link(state) else {
        return;
    };
    let Some(mut replica) = state.portable.load(&link) else {
        return;
    };
    // Only when nothing else happened at the coordinator in between; otherwise the next
    // synchronization finds out.
    if replica.loaded.digest != forwarded.previous.digest {
        return;
    }
    if let Ok(applied) = local_digest(state) {
        replica.loaded = forwarded.revision.clone();
        replica.saved = forwarded.revision.clone();
        replica.loaded_at = now();
        replica.saved_at = now();
        replica.applied = applied;
        let _ = state.portable.save(&replica);
        let mut status = state.portable.status();
        status.local_changes = false;
        status.conflict = None;
    }
}

/// A note saved on a connected installation is decided by the coordinator, then kept here.
pub(super) async fn note_save(
    state: &DesktopState,
    args: &Value,
) -> std::result::Result<Value, String> {
    let Some(link) = implementation::link(state).map_err(fail)? else {
        return notes::save(state, args);
    };
    let note: Note = serde_json::from_value(args.get("note").cloned().unwrap_or(Value::Null))
        .map_err(|_| "[invalid_input] A note needs title and text, and optionally contexts, keywords, origin and reference.".to_owned())?;
    ready(state, &link, true)?;
    let config = read_config(&state.config_path).map_err(fail)?;
    let index = Index::open(&config.server.data_dir.join(crate::index::FILE)).map_err(fail)?;
    let expected = if note.id.is_empty() {
        None
    } else {
        Some(token(
            &index
                .note(&note.id)
                .map_err(fail)?
                .ok_or("[invalid_input] Unknown note.")?,
        ))
    };
    let outcome = match call_coordinator(
        state,
        "portable_note_save",
        &json!({"note":note,"expected":expected}),
    )
    .await
    {
        Ok(Some(outcome)) => outcome,
        _ => return Err(OFFLINE.into()),
    };
    if outcome["ok"] != true {
        return Err(rejected(state, &outcome));
    }
    let saved: Note = serde_json::from_value(outcome["data"]["note"].clone())
        .map_err(|_| "The coordinator returned an unexpected reply.".to_owned())?;
    index.put_note(&saved).map_err(fail)?;
    if let (Ok(previous), Ok(revision)) = (
        serde_json::from_value(outcome["data"]["previous"].clone()),
        serde_json::from_value(outcome["data"]["revision"].clone()),
    ) {
        settle(state, &Forwarded { previous, revision });
    }
    Ok(json!(saved))
}

pub(super) async fn note_remove(
    state: &DesktopState,
    args: &Value,
) -> std::result::Result<Value, String> {
    let Some(link) = implementation::link(state).map_err(fail)? else {
        return notes::remove(state, args);
    };
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or("[invalid_input] A note ID is required.")?;
    ready(state, &link, true)?;
    let config = read_config(&state.config_path).map_err(fail)?;
    let index = Index::open(&config.server.data_dir.join(crate::index::FILE)).map_err(fail)?;
    let held = index
        .note(id)
        .map_err(fail)?
        .ok_or("[invalid_input] Unknown note.")?;
    let outcome = match call_coordinator(
        state,
        "portable_note_remove",
        &json!({"id":id,"expected":token(&held)}),
    )
    .await
    {
        Ok(Some(outcome)) => outcome,
        _ => return Err(OFFLINE.into()),
    };
    if outcome["ok"] != true {
        return Err(rejected(state, &outcome));
    }
    index.remove_note(id).map_err(fail)?;
    if let (Ok(previous), Ok(revision)) = (
        serde_json::from_value(outcome["data"]["previous"].clone()),
        serde_json::from_value(outcome["data"]["revision"].clone()),
    ) {
        settle(state, &Forwarded { previous, revision });
    }
    Ok(json!({"removed": id}))
}

#[cfg(test)]
mod tests;
