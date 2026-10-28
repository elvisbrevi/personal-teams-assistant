//! Synchronization of the account's variable catalog between its clients.
//!
//! The coordinator (the host that owns the account) is authoritative. An installation signed in
//! to it keeps a private, account-qualified *mirror* of the catalog: entry IDs, names,
//! descriptions, versions and timestamps, plus the coordinator's revision. A mirror never holds a
//! value, ciphertext or key, so there is no offline copy of a secret. Edits made on a connected
//! installation go to the coordinator under the same version checks as any other client, so a
//! stale or disconnected client can neither overwrite a newer change nor bring a deleted
//! variable back: the deleted ID stays a tombstone there and the mirror is only ever replaced
//! by a newer, validated snapshot.
use super::*;
use crate::app::{
    implementation::{self, Link},
    standing,
};
use std::{collections::BTreeSet, time::Instant};
#[cfg(test)]
use std::{future::Future, pin::Pin};

const SCHEMA: u32 = 1;
/// Entries per page: a page of the largest legal entries stays far below the reply limit.
const PAGE: usize = 100;
const MAX_PAGES: usize = 8;
/// Reads restarted because the coordinator's catalog changed between two pages.
const ATTEMPTS: usize = 3;
/// Quiet time between automatic attempts, whether or not the last one worked.
const AUTO_SECONDS: u64 = 30;
/// A message from the coordinator is shown only after it is cleaned and bounded.
const MAX_MESSAGE: usize = 400;

#[cfg(test)]
type Call = Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>>;
#[cfg(test)]
type Transport = Arc<dyn Fn(String, Value) -> Call + Send + Sync>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    schema: u32,
    link: Link,
    /// Opaque fingerprint of the account source that produced the snapshot.
    source: String,
    revision: u64,
    synced_at: i64,
    entries: Vec<vault::Metadata>,
}

#[derive(Default)]
struct Status {
    syncing: bool,
    attempted_at: Option<i64>,
    error: Option<(&'static str, &'static str)>,
    conflict: Option<String>,
    automatic_at: Option<Instant>,
}

/// The mirror of one installation: its cache file and what is known about the last attempts.
pub(super) struct Mirror {
    path: PathBuf,
    status: std::sync::Mutex<Status>,
    gate: tokio::sync::Mutex<()>,
    #[cfg(test)]
    transport: std::sync::Mutex<Option<Transport>>,
}

impl Mirror {
    pub(super) fn new(profile: &Path) -> Self {
        Self {
            path: profile.join("variables").join("catalog-mirror.json"),
            status: std::sync::Mutex::new(Status::default()),
            gate: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            transport: std::sync::Mutex::new(None),
        }
    }
    fn status(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(super) fn forget(&self) {
        let _ = fs::remove_file(&self.path);
        *self.status() = Status::default();
    }
    /// The cached catalog, only if it was produced for exactly this coordinator, account and
    /// machine; anything else is discarded rather than reused.
    fn load(&self, link: &Link) -> Option<Cache> {
        let bytes = fs::read(&self.path).ok()?;
        let cache = serde_json::from_slice::<Cache>(&bytes)
            .ok()
            .filter(|c| c.schema == SCHEMA && c.link == *link);
        if cache.is_none() {
            let _ = fs::remove_file(&self.path);
        }
        cache
    }
    fn save(&self, cache: &Cache) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            security::private_dir(parent)?;
        }
        write_private(&self.path, &serde_json::to_string(cache)?)
    }
    /// What a client may show about synchronization: never a value or a credential.
    fn describe(&self, link: &Link) -> Value {
        let cache = self.load(link);
        let status = self.status();
        let state = if status.syncing {
            "syncing"
        } else if status.conflict.is_some() {
            "conflict"
        } else if status.error.is_some() {
            "error"
        } else if cache.is_some() {
            "synced"
        } else {
            "never"
        };
        json!({
            "mode": "mirror",
            "state": state,
            "coordinator": link.origin,
            "revision": cache.as_ref().map(|c| c.revision),
            "synced_at": cache.as_ref().map(|c| c.synced_at),
            "entries": cache.as_ref().map(|c| c.entries.len()),
            "attempted_at": status.attempted_at,
            "error": status.error.map(|(code, message)| json!({"code":code,"message":message})),
            "conflict": status.conflict,
        })
    }
    #[cfg(test)]
    pub(super) fn use_transport(&self, transport: Transport) {
        *self.transport.lock().unwrap() = Some(transport);
    }
}

/// How a catalog that is its own authority describes itself.
pub(super) fn authoritative(store: &Store) -> std::result::Result<Value, vault::Error> {
    Ok(json!({
        "mode": "authoritative",
        "state": "authoritative",
        "revision": store.catalog_page(0, 0)?.revision,
    }))
}

/// The variables a request can see: the synchronized catalog on an installation signed in to a
/// coordinator, otherwise this installation's own store.
pub(super) fn known(
    host: &Host,
    store: &Store,
) -> std::result::Result<Vec<vault::Metadata>, vault::Error> {
    match implementation::link(&host.state)
        .map_err(|_| vault::Error::Storage("The coordinator connection is unreadable.".into()))?
    {
        Some(link) => Ok(host
            .state
            .variables
            .catalog
            .load(&link)
            .map(|c| c.entries)
            .unwrap_or_default()),
        None => store.list(None),
    }
}

struct Failure {
    code: &'static str,
    exit: i32,
    message: &'static str,
}
const UNREACHABLE: Failure = Failure {
    code: "dependency_or_network",
    exit: 5,
    message: "The coordinator could not be reached, or this installation is no longer connected to it. The last synchronized catalog is still shown; nothing was changed.",
};
const REVOKED: Failure = Failure {
    code: "not_ready",
    exit: 3,
    message: standing::REVOKED,
};
const UNSUPPORTED: Failure = Failure {
    code: "not_ready",
    exit: 3,
    message: "The coordinator does not support catalog synchronization yet. Update its host and CLI to the same release.",
};
const REFUSED: Failure = Failure {
    code: "operation_failed",
    exit: 1,
    message: "The coordinator refused the synchronization. Sign this installation in again with Connect this machine.",
};
const INVALID: Failure = Failure {
    code: "operation_failed",
    exit: 1,
    message: "The coordinator returned an invalid or oversized catalog. Nothing was replaced.",
};
const STALE: Failure = Failure {
    code: "state_conflict",
    exit: 6,
    message: "The coordinator reports an older catalog than this installation already holds (a restored or replayed state). Nothing was replaced; once you confirm the coordinator was restored, repeat with reset.",
};
const SOURCE: Failure = Failure {
    code: "state_conflict",
    exit: 6,
    message: "The coordinator now serves a different account source than the cached catalog. Nothing was replaced and no other account was selected; repeat with reset to start from the coordinator's catalog.",
};
const CHANGING: Failure = Failure {
    code: "state_conflict",
    exit: 6,
    message: "The catalog kept changing while it was read. Synchronize again in a moment.",
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    schema: u32,
    source: String,
    revision: u64,
    total: usize,
    offset: usize,
    items: Vec<vault::Metadata>,
}

struct Snapshot {
    source: String,
    revision: u64,
    entries: Vec<vault::Metadata>,
}

pub(in crate::app) async fn call_coordinator(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> anyhow::Result<Option<Value>> {
    call_account(&host.state, method, args).await
}

/// One account call to the coordinator, recorded in the installation's standing. `None` when
/// the installation is not signed in to one.
pub(in crate::app) async fn call_account(
    state: &DesktopState,
    method: &str,
    args: &Value,
) -> anyhow::Result<Option<Value>> {
    #[cfg(test)]
    {
        let transport = state.variables.catalog.transport.lock().unwrap().clone();
        if let Some(transport) = transport {
            let result = transport(method.into(), args.clone()).await;
            standing::note(state, &result);
            return result.map(Some);
        }
    }
    implementation::fleet::account_reply(state, method, args).await
}

/// The synchronized catalog of this installation's account, for choosing a variable offline.
pub(in crate::app) fn cached(host: &Host, link: &Link) -> Vec<vault::Metadata> {
    host.state
        .variables
        .catalog
        .load(link)
        .map(|c| c.entries)
        .unwrap_or_default()
}

/// A message from the coordinator, without control characters and bounded in length.
fn clean(message: &str) -> String {
    message
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_MESSAGE)
        .collect()
}

async fn fetch_page(host: &Arc<Host>, offset: usize) -> std::result::Result<Page, Failure> {
    let reply = call_coordinator(
        host,
        "variable_catalog",
        &json!({"offset":offset,"limit":PAGE}),
    )
    .await
    .map_err(|error| match standing::refusal(&error) {
        Some(standing::Refusal::Rejected) => REVOKED,
        _ => UNREACHABLE,
    })?
    .ok_or(UNREACHABLE)?;
    if reply["ok"] != true {
        let unsupported = reply["message"]
            .as_str()
            .is_some_and(|m| m.contains("Unavailable coordinator operation"));
        return Err(if unsupported { UNSUPPORTED } else { REFUSED });
    }
    let page: Page = serde_json::from_value(reply["data"].clone()).map_err(|_| INVALID)?;
    if page.schema != SCHEMA {
        return Err(UNSUPPORTED);
    }
    if page.offset != offset
        || page.total > vault::MAX_VARIABLES
        || page.items.len() > PAGE
        || page.source.len() != 32
        || !page.source.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(INVALID);
    }
    Ok(page)
}

/// Every entry must be one the store itself could have produced.
fn validate(entries: &[vault::Metadata], revision: u64) -> std::result::Result<(), Failure> {
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut versions = 0u64;
    for entry in entries {
        let canonical = uuid::Uuid::parse_str(&entry.id)
            .map(|id| id.hyphenated().to_string())
            .map_err(|_| INVALID)?;
        if canonical != entry.id
            || vault::validate_name(&entry.name).is_err()
            || vault::validate_description(&entry.description).is_err()
            || entry.version == 0
            || entry.created_at < 0
            || entry.updated_at < entry.created_at
            || !ids.insert(entry.id.clone())
            || !names.insert(entry.name.to_ascii_uppercase())
        {
            return Err(INVALID);
        }
        versions = versions.saturating_add(entry.version);
    }
    // The revision counts every entry version and every deletion, so it can only be larger.
    if revision < versions {
        return Err(INVALID);
    }
    Ok(())
}

async fn fetch_snapshot(host: &Arc<Host>) -> std::result::Result<Snapshot, Failure> {
    'attempt: for _ in 0..ATTEMPTS {
        let mut entries: Vec<vault::Metadata> = Vec::new();
        let mut first: Option<(String, u64, usize)> = None;
        for _ in 0..MAX_PAGES {
            let page = fetch_page(host, entries.len()).await?;
            match &first {
                None => first = Some((page.source.clone(), page.revision, page.total)),
                Some((source, revision, total)) => {
                    if *source != page.source {
                        return Err(SOURCE);
                    }
                    if *revision != page.revision || *total != page.total {
                        continue 'attempt;
                    }
                }
            }
            let total = first.as_ref().map_or(0, |f| f.2);
            if page.items.is_empty() && entries.len() < total {
                return Err(INVALID);
            }
            entries.extend(page.items);
            if entries.len() >= total {
                break;
            }
        }
        let (source, revision, total) = first.ok_or(INVALID)?;
        if entries.len() != total {
            return Err(INVALID);
        }
        validate(&entries, revision)?;
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        return Ok(Snapshot {
            source,
            revision,
            entries,
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

fn diff(before: Option<&Cache>, after: &[vault::Metadata]) -> Value {
    let old: BTreeMap<&str, u64> = before
        .map(|c| {
            c.entries
                .iter()
                .map(|e| (e.id.as_str(), e.version))
                .collect()
        })
        .unwrap_or_default();
    let mut added = 0;
    let mut updated = 0;
    for entry in after {
        match old.get(entry.id.as_str()) {
            None => added += 1,
            Some(version) if *version != entry.version => updated += 1,
            Some(_) => {}
        }
    }
    let kept: BTreeSet<&str> = after.iter().map(|e| e.id.as_str()).collect();
    let removed = old.keys().filter(|id| !kept.contains(*id)).count();
    json!({"added":added,"updated":updated,"removed":removed})
}

fn failure_reply(failure: &Failure) -> Reply {
    Reply::error(failure.code, failure.exit, failure.message)
}

/// Replace the mirror with the coordinator's current catalog, or leave it exactly as it was.
async fn synchronize(host: &Arc<Host>, link: &Link, reset: bool, mode: Mode) -> Reply {
    let mirror = &host.state.variables.catalog;
    let _gate = match mode {
        Mode::Explicit => mirror.gate.lock().await,
        Mode::Automatic => match mirror.gate.try_lock() {
            Ok(gate) => gate,
            Err(_) => return Reply::success(json!({"sync": mirror.describe(link)})),
        },
    };
    {
        let mut status = mirror.status();
        status.syncing = true;
        status.attempted_at = Some(chrono::Utc::now().timestamp());
    }
    if reset {
        let _ = fs::remove_file(&mirror.path);
    }
    let result = async {
        let snapshot = fetch_snapshot(host).await?;
        // The connection may have changed while pages were in flight: never store a catalog
        // under a link it was not read for.
        if implementation::link(&host.state).ok().flatten().as_ref() != Some(link) {
            return Err(UNREACHABLE);
        }
        let before = mirror.load(link);
        if let Some(cache) = &before {
            if cache.source != snapshot.source {
                return Err(SOURCE);
            }
            if snapshot.revision < cache.revision
                || (snapshot.revision == cache.revision && cache.entries != snapshot.entries)
            {
                return Err(STALE);
            }
        }
        let changes = diff(before.as_ref(), &snapshot.entries);
        mirror
            .save(&Cache {
                schema: SCHEMA,
                link: link.clone(),
                source: snapshot.source,
                revision: snapshot.revision,
                synced_at: chrono::Utc::now().timestamp(),
                entries: snapshot.entries,
            })
            .map_err(|_| Failure {
                code: "operation_failed",
                exit: 1,
                message: "The synchronized catalog could not be stored on this computer.",
            })?;
        Ok(changes)
    }
    .await;
    let mut status = mirror.status();
    status.syncing = false;
    match result {
        Ok(changes) => {
            status.error = None;
            if mode == Mode::Explicit {
                status.conflict = None;
            }
            drop(status);
            Reply::success(json!({
                "sync": mirror.describe(link),
                "changes": changes,
                "standing": host.state.standing.view(Some(link)),
            }))
        }
        Err(failure) => {
            status.error = Some((failure.code, failure.message));
            drop(status);
            failure_reply(&failure)
        }
    }
}

/// Background refresh, called by the connected installation's regular coordinator poll.
pub(in crate::app) fn auto(host: &Arc<Host>) {
    let Ok(Some(link)) = implementation::link(&host.state) else {
        return;
    };
    {
        let mut status = host.state.variables.catalog.status();
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncArgs {
    #[serde(default)]
    reset: bool,
}

/// The coordinator's reply to a change, restricted to the shape the store produces.
fn shape(method: &str, data: &Value) -> Option<Value> {
    if method == "variable_delete" {
        let deleted = data.get("deleted")?;
        return Some(json!({
            "deleted": {
                "id": deleted.get("id")?.as_str()?,
                "name": deleted.get("name")?.as_str()?,
                "version": deleted.get("version")?.as_u64()?,
                "replayed": deleted.get("replayed")?.as_bool()?,
            },
            "replayed": data.get("replayed")?.as_bool()?,
        }));
    }
    let variable: vault::Metadata = serde_json::from_value(data.get("variable")?.clone()).ok()?;
    Some(json!({"variable": variable, "replayed": data.get("replayed")?.as_bool()?}))
}

/// Send one change to the coordinator, which alone decides it, and then refresh the mirror.
async fn forward(host: &Arc<Host>, link: &Link, method: &str, args: Value) -> Reply {
    let mirror = &host.state.variables.catalog;
    let outcome = match call_coordinator(host, method, &args).await {
        Ok(Some(reply)) => reply,
        _ => {
            return Reply::error(
                "dependency_or_network",
                5,
                "The coordinator could not be reached, or this installation is no longer connected to it. The change may not have been applied: synchronize, inspect the variable and repeat only if it is unchanged.",
            );
        }
    };
    if outcome["ok"] != true {
        let message = clean(outcome["message"].as_str().unwrap_or_default());
        let conflict = outcome["code"] == "state_conflict";
        if conflict {
            mirror.status().conflict = Some(message.clone());
        }
        // Fresh state first: the caller must inspect what changed before deciding again.
        let _ = synchronize(host, link, false, Mode::Automatic).await;
        return match outcome["code"].as_str() {
            Some("state_conflict") => Reply::error("state_conflict", 6, &message),
            Some("invalid_input") => Reply::error("invalid_input", 2, &message),
            _ => Reply::error(
                "operation_failed",
                1,
                "The coordinator could not apply the change.",
            ),
        };
    }
    let Some(mut data) = shape(method, &outcome["data"]) else {
        return Reply::error(
            "operation_failed",
            1,
            "The coordinator returned an unexpected reply.",
        );
    };
    mirror.status().conflict = None;
    let refreshed = synchronize(host, link, false, Mode::Explicit).await;
    data["sync"] = if refreshed.ok {
        refreshed.data["sync"].clone()
    } else {
        mirror.describe(link)
    };
    Reply::success(data)
}

/// Operations whose behavior depends on whether this installation is signed in to a coordinator.
/// `None` leaves the request to the ordinary local handling.
pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Option<Reply> {
    let method = request.method.as_str();
    if !matches!(
        method,
        "variable_sync"
            | "variable_sync_status"
            | "variable_list"
            | "variable_create"
            | "variable_update"
            | "variable_delete"
            | "variable_reveal"
            | "variable_entry_complete"
    ) {
        return None;
    }
    let link = match implementation::link(&host.state) {
        Ok(link) => link,
        Err(_) => {
            return Some(failure(vault::Error::Storage(
                "The coordinator connection is unreadable.".into(),
            )));
        }
    };
    let synchronization = matches!(method, "variable_sync" | "variable_sync_status");
    if link.is_none() && !synchronization {
        return None;
    }
    let invocation = match authority::invocation(&host.state) {
        Ok(invocation) => invocation,
        Err(_) => {
            return Some(failure(vault::Error::Conflict(
                authority::ACCOUNT_UNAVAILABLE.into(),
            )));
        }
    };
    if invocation.transport == authority::Transport::Machine {
        return Some(failure(vault::Error::Conflict(
            "This variable operation is not available from this client.".into(),
        )));
    }
    let state = &host.state.variables;
    let Some(link) = link else {
        // Coordinator or standalone installation: its own catalog is the authority.
        if method == "variable_sync"
            && let Err(error) = parse::<SyncArgs>(&request.args)
        {
            return Some(failure(error));
        }
        let store = match state.store(host) {
            Ok(store) => store,
            Err(error) => return Some(failure(error)),
        };
        return Some(match authoritative(&store) {
            Ok(sync) => Reply::success(json!({"sync": sync})),
            Err(error) => failure(error),
        });
    };
    let mirror = &state.catalog;
    let now = chrono::Utc::now().timestamp();
    Some(match method {
        "variable_sync_status" => Reply::success(json!({
            "sync": mirror.describe(&link),
            "standing": host.state.standing.view(Some(&link)),
        })),
        "variable_sync" => match parse::<SyncArgs>(&request.args) {
            Ok(args) => synchronize(host, &link, args.reset, Mode::Explicit).await,
            Err(error) => failure(error),
        },
        "variable_list" => {
            let list: ListArgs = match parse(&request.args) {
                Ok(list) => list,
                Err(error) => return Some(failure(error)),
            };
            // A first look right after signing in should not have to wait for the next refresh.
            let quiet = mirror.status().attempted_at.is_none_or(|at| now - at > 5);
            if quiet && mirror.load(&link).is_none() {
                let _ = synchronize(host, &link, false, Mode::Explicit).await;
            }
            let needle = list
                .query
                .as_deref()
                .map(str::to_lowercase)
                .filter(|q| !q.trim().is_empty());
            let mut variables: Vec<vault::Metadata> = mirror
                .load(&link)
                .map(|c| c.entries)
                .unwrap_or_default()
                .into_iter()
                .filter(|v| {
                    needle.as_ref().is_none_or(|q| {
                        v.name.to_lowercase().contains(q.trim())
                            || v.description.to_lowercase().contains(q.trim())
                    })
                })
                .collect();
            variables.sort_by_key(|v| v.name.to_ascii_uppercase());
            let total = variables.len();
            variables.truncate(list.limit.unwrap_or(vault::MAX_VARIABLES).max(1));
            let mut listed = listing(state, variables, total, mirror.describe(&link), now);
            listed["standing"] = host.state.standing.view(Some(&link));
            Reply::success(listed)
        }
        "variable_reveal" => failure(vault::Error::Input(
            "This installation keeps no copy of account values. Reveal or copy the value from the portal or the coordinator.".into(),
        )),
        "variable_entry_complete" => {
            let complete: CompleteArgs = match parse(&request.args) {
                Ok(complete) => complete,
                Err(error) => return Some(failure(error)),
            };
            let (name, description, replaces) = match state.open_entry(&complete.request, now) {
                Ok(entry) => entry,
                Err(error) => return Some(failure(error)),
            };
            let (method, args) = match replaces {
                // A replacement rotates the version the request saw; the coordinator refuses it
                // if the entry changed since.
                Some((id, version)) => (
                    "variable_update",
                    json!({"id":id,"version":version,"value":complete.value.expose()}),
                ),
                None => (
                    "variable_create",
                    json!({"id":complete.request,"name":name,"description":description,"value":complete.value.expose()}),
                ),
            };
            let reply = forward(host, &link, method, args).await;
            if reply.ok
                && let Some(version) = reply.data["variable"]["version"].as_u64()
            {
                state.settle_entry(&complete.request, version);
            }
            reply
        }
        _ => {
            // Create, update and delete: validate the shape here, let the coordinator decide.
            let checked = match method {
                "variable_create" => parse::<CreateArgs>(&request.args).map(|_| ()),
                "variable_update" => parse::<UpdateArgs>(&request.args).map(|_| ()),
                _ => parse::<DeleteArgs>(&request.args).map(|_| ()),
            };
            match checked {
                Ok(()) => forward(host, &link, method, request.args.clone()).await,
                Err(error) => failure(error),
            }
        }
    })
}

/// What a trusted machine may ask the coordinator about its account's variables: the catalog
/// (metadata, in pages) and the same versioned changes any client can make. Never a value read,
/// consumers, audit or keys.
pub(in crate::app) fn machine_call(
    host: &Host,
    method: &str,
    args: &Value,
) -> anyhow::Result<Value> {
    let invocation = authority::invocation(&host.state)?;
    ensure!(
        invocation.transport == authority::Transport::Machine,
        "Unavailable coordinator operation."
    );
    let store = host
        .state
        .variables
        .store(host)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    serve(host, &store, method, args).map_err(|error| match error {
        vault::Error::Conflict(message) => anyhow::anyhow!("[state_conflict] {message}"),
        other => anyhow::anyhow!(other.to_string()),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveArgs {
    id: String,
    #[serde(default)]
    version: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogArgs {
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}

fn serve(
    host: &Host,
    store: &Store,
    method: &str,
    args: &Value,
) -> std::result::Result<Value, vault::Error> {
    match method {
        "variable_catalog" => {
            let CatalogArgs { offset, limit } = parse(args)?;
            let page = store.catalog_page(offset, limit.unwrap_or(PAGE).clamp(1, PAGE))?;
            Ok(json!({
                "schema": SCHEMA,
                "source": source(host),
                "revision": page.revision,
                "total": page.total,
                "offset": offset,
                "items": page.items,
            }))
        }
        "variable_resolve" => {
            // The one value a machine receives: for a use on that machine, never a reveal.
            let ResolveArgs { id, version } = parse(args)?;
            let result = store.use_entry(&id, version);
            store.audit("use", entry_of(args).as_deref(), outcome(&result, false));
            let (variable, value) = result?;
            Ok(json!({"variable":variable,"value":value.expose()}))
        }
        "variable_create" => {
            let create: CreateArgs = parse(args)?;
            let created = store.create(NewVariable {
                id: create.id,
                name: create.name,
                description: create.description,
                value: create.value,
            })?;
            Ok(json!({"variable":created.variable,"replayed":created.replayed}))
        }
        _ => {
            let action = if method == "variable_update" {
                "update"
            } else {
                "delete"
            };
            let result = mutate(store, method, args);
            let replayed = result.as_ref().is_ok_and(|data| data["replayed"] == true);
            store.audit(
                action,
                entry_of(args).as_deref(),
                outcome(&result, replayed),
            );
            result
        }
    }
}

/// An opaque fingerprint of the account this host serves, so a client notices when the
/// coordinator's catalog stops coming from the account it cached.
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
            b"personal-teams-assistant/variable-catalog-source/v1".as_slice(),
            account.as_bytes(),
        ]
        .concat(),
    );
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}
