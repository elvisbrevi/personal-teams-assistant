//! Account variables for the GUI, CLI and MCP: one host operation per behavior.
//!
//! A value enters only through protected entry (the GUI's masked field or CLI standard input) and
//! is returned by exactly one operation, `variable_reveal`, which only a person at the GUI (the
//! application window or an authenticated portal browser) can invoke: never the CLI, MCP, tools
//! or machines. An agent starts a protected-entry request (to store or to replace a value) by
//! name or ID and later reads only its state.
use super::{authority, consumers, control::Reply, *};
use crate::variables::{self as vault, KeyBackup, NewVariable, Secret, Store};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

mod sync;
pub(super) use sync::{auto, cached, call_account, machine_call};

/// A protected-entry request stays open this long (seconds).
const ENTRY_SECONDS: i64 = 15 * 60;
const MAX_PENDING: usize = 20;

pub(super) struct State {
    profile: PathBuf,
    store: std::sync::Mutex<Option<Arc<Store>>>,
    entries: std::sync::Mutex<BTreeMap<String, Entry>>,
    pub(super) consumers: consumers::State,
    catalog: sync::Mirror,
}

/// A pending protected entry: name, description and the entry (ID, seen version) it replaces.
type OpenEntry = (String, String, Option<(String, u64)>);

struct Entry {
    name: String,
    description: String,
    expires: i64,
    state: &'static str,
    version: Option<u64>,
    /// A replacement of an existing entry's value: its ID and the version the request saw.
    replaces: Option<(String, u64)>,
}

impl State {
    pub(super) fn new(profile: &Path) -> Self {
        Self {
            profile: profile.to_path_buf(),
            store: std::sync::Mutex::new(None),
            entries: std::sync::Mutex::new(BTreeMap::new()),
            consumers: consumers::State::new(profile),
            catalog: sync::Mirror::new(profile),
        }
    }
    /// Drop the synchronized catalog: the installation left or changed its account.
    pub(super) fn forget_catalog(&self) {
        self.catalog.forget();
    }
    /// Route the coordinator exchanges of an installation in tests.
    #[cfg(test)]
    pub(super) fn use_transport(
        &self,
        transport: impl Fn(
            String,
            Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send>,
        > + Send
        + Sync
        + 'static,
    ) {
        self.catalog.use_transport(Arc::new(transport));
    }
    /// Drop everything held in memory for the current account: requests waiting for a protected
    /// value and values staged for a running operation. Stored data is not touched.
    pub(super) fn withdraw(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.consumers.unstage();
    }
    pub(super) fn store(&self, host: &Host) -> std::result::Result<Arc<Store>, vault::Error> {
        let mut slot = self.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(store) = slot.as_ref() {
            return Ok(store.clone());
        }
        let account = host.state.authority.account.as_ref().map(|a| a.id.as_str());
        let store = Arc::new(Store::open_profile(&self.profile, account)?);
        *slot = Some(store.clone());
        Ok(store)
    }
    /// A protected-entry request that is still pending (or already completed, for a repeat):
    /// its name, description and the entry it replaces.
    fn open_entry(&self, request: &str, now: i64) -> std::result::Result<OpenEntry, vault::Error> {
        let entries = self.entries();
        let entry = entries.get(request).ok_or_else(|| {
            vault::Error::Input("That protected entry is unknown or has expired.".into())
        })?;
        if !(entry.state == "completed" || (entry.state == "pending" && entry.expires > now)) {
            return Err(vault::Error::Input(format!(
                "That protected entry is {}.",
                if entry.state == "pending" {
                    "expired"
                } else {
                    entry.state
                }
            )));
        }
        Ok((
            entry.name.clone(),
            entry.description.clone(),
            entry.replaces.clone(),
        ))
    }
    fn settle_entry(&self, request: &str, version: u64) {
        if let Some(entry) = self.entries().get_mut(request) {
            entry.state = "completed";
            entry.version = Some(version);
        }
    }
    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Entry>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = chrono::Utc::now().timestamp();
        // Settled requests are forgotten after one more window; expiry is reported meanwhile.
        entries.retain(|_, entry| entry.expires + ENTRY_SECONDS > now);
        entries
    }
}

pub(super) fn handles(method: &str) -> bool {
    method.starts_with("variable_")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    id: String,
    name: String,
    description: String,
    value: Secret,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    id: String,
    version: u64,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    value: Option<Secret>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    id: String,
    version: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevealArgs {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditArgs {
    #[serde(default)]
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartArgs {
    name: String,
    description: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RotateArgs {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestArgs {
    request: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteArgs {
    request: String,
    value: Secret,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupArgs {
    output: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreArgs {
    backup: KeyBackup,
}

/// Arguments are parsed without echoing them: a rejected value must not reach an error text.
fn parse<T: DeserializeOwned>(args: &Value) -> std::result::Result<T, vault::Error> {
    serde_json::from_value(args.clone())
        .map_err(|_| vault::Error::Input("Invalid arguments for this variable operation.".into()))
}

pub(super) fn failure(error: vault::Error) -> Reply {
    let (code, exit) = match &error {
        vault::Error::Input(_) => ("invalid_input", 2),
        vault::Error::Conflict(_) => ("state_conflict", 6),
        vault::Error::Key(_) | vault::Error::Revoked(_) => ("not_ready", 3),
        vault::Error::Offline(_) => ("dependency_or_network", 5),
        vault::Error::Integrity(_) | vault::Error::Storage(_) => ("operation_failed", 1),
    };
    Reply::error(code, exit, &error.to_string())
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Reply {
    if super::migration::handles(&request.method) {
        return super::migration::operate(host, request).await;
    }
    if consumers::handles(&request.method) {
        return consumers::operate(host, request).await;
    }
    if let Some(reply) = sync::operate(host, request).await {
        return reply;
    }
    match run(host, &request.method, &request.args) {
        Ok(data) => Reply::success(data),
        Err(error) => failure(error),
    }
}

/// The reply of `variable_list`: metadata only, the requests waiting for the user and how the
/// catalog was obtained (`sync`).
fn listing(
    state: &State,
    variables: Vec<vault::Metadata>,
    total: usize,
    sync: Value,
    now: i64,
) -> Value {
    let pending: Vec<Value> = state
        .entries()
        .iter()
        .filter(|(_, e)| e.state == "pending" && e.expires > now)
        .map(|(request, e)| {
            json!({"request":request,"name":e.name,"description":e.description,"expires_at":e.expires,
                "action": if e.replaces.is_some() { "replace_value" } else { "create" },
                "id": e.replaces.as_ref().map(|(id, _)| id)})
        })
        .collect();
    json!({
        "variables": variables,
        "total": total,
        "pending": pending,
        "sync": sync,
        "limits": {"name":vault::MAX_NAME,"description":vault::MAX_DESCRIPTION,"value_bytes":vault::MAX_VALUE,"variables":vault::MAX_VARIABLES},
    })
}

/// The audit outcome of a result: a short fixed word, never any text from the request.
fn outcome<T>(result: &std::result::Result<T, vault::Error>, replayed: bool) -> &'static str {
    match result {
        Ok(_) if replayed => "replayed",
        Ok(_) => "ok",
        Err(vault::Error::Input(_)) => "rejected",
        Err(vault::Error::Conflict(message)) if message.contains("was deleted") => "deleted",
        Err(vault::Error::Conflict(_)) => "stale",
        Err(_) => "failed",
    }
}

/// The entry an audited request names, if it names a well-formed one.
fn entry_of(args: &Value) -> Option<String> {
    args.get("id")
        .and_then(Value::as_str)
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .map(|id| id.hyphenated().to_string())
}

/// Edit, delete and reveal one entry of the account.
fn mutate(store: &Store, method: &str, args: &Value) -> std::result::Result<Value, vault::Error> {
    match method {
        "variable_update" => {
            let update: UpdateArgs = parse(args)?;
            let updated = store.update(
                &update.id,
                update.version,
                update.description.as_deref(),
                update.value.as_ref(),
            )?;
            Ok(json!({"variable":updated.variable,"replayed":updated.replayed}))
        }
        "variable_delete" => {
            let delete: DeleteArgs = parse(args)?;
            let deleted = store.delete(&delete.id, delete.version)?;
            Ok(json!({"deleted":deleted,"replayed":deleted.replayed}))
        }
        _ => {
            let RevealArgs { id } = parse(args)?;
            let (variable, value) = store.reveal(&id)?;
            // The one place a value leaves the store: to the person who asked at the GUI.
            Ok(json!({"variable":variable,"value":value.expose()}))
        }
    }
}

fn run(host: &Host, method: &str, args: &Value) -> std::result::Result<Value, vault::Error> {
    let invocation = authority::invocation(&host.state)
        .map_err(|_| vault::Error::Conflict(authority::ACCOUNT_UNAVAILABLE.into()))?;
    let transport = invocation.transport;
    // Machine credentials never reach variables; the browser may only run the shared GUI's
    // own workflow. Operator key custody is local to the installation.
    let allowed = match method {
        "variable_list"
        | "variable_create"
        | "variable_update"
        | "variable_delete"
        | "variable_entry_status"
        | "variable_entry_complete"
        | "variable_entry_cancel" => transport != authority::Transport::Machine,
        // Only a person at the GUI reads a value back; the CLI, MCP and tools share the local
        // channel with agents, so that channel can never reveal one.
        "variable_reveal" => matches!(
            transport,
            authority::Transport::Window | authority::Transport::Browser
        ),
        "variable_entry_start"
        | "variable_rotation_start"
        | "variable_audit"
        | "variable_verify"
        | "variable_key_backup"
        | "variable_key_restore"
        | "variable_key_rotate" => transport == authority::Transport::Local,
        _ => return Err(vault::Error::Input("Unknown variable operation.".into())),
    };
    if !allowed {
        // A refused reveal is worth a trace; it never touches a value.
        if method == "variable_reveal"
            && let Ok(store) = host.state.variables.store(host)
        {
            store.audit("reveal", entry_of(args).as_deref(), "denied_client");
        }
        return Err(vault::Error::Conflict(
            "This variable operation is not available from this client.".into(),
        ));
    }
    let state = &host.state.variables;
    let store = state.store(host)?;
    let now = chrono::Utc::now().timestamp();
    // Reveal, edit and deletion leave an audit event naming action, entry and outcome.
    if let Some(action) = match method {
        "variable_reveal" => Some("reveal"),
        "variable_update" => Some("update"),
        "variable_delete" => Some("delete"),
        _ => None,
    } {
        let result = mutate(&store, method, args);
        let entry = entry_of(args);
        let replayed = result.as_ref().is_ok_and(|data| data["replayed"] == true);
        store.audit(action, entry.as_deref(), outcome(&result, replayed));
        return result;
    }
    match method {
        "variable_list" => {
            let list: ListArgs = parse(args)?;
            let mut variables = store.list(list.query.as_deref())?;
            let total = variables.len();
            variables.truncate(list.limit.unwrap_or(vault::MAX_VARIABLES).max(1));
            Ok(listing(
                state,
                variables,
                total,
                sync::authoritative(&store)?,
                now,
            ))
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
        "variable_entry_start" => {
            let start: StartArgs = parse(args)?;
            vault::validate_name(&start.name)?;
            vault::validate_description(&start.description)?;
            if sync::known(host, &store)?
                .iter()
                .any(|v| v.name.eq_ignore_ascii_case(&start.name))
            {
                return Err(vault::Error::Conflict(format!(
                    "A variable named {} already exists. Inspect it with variable_list before choosing another name.",
                    start.name
                )));
            }
            let mut entries = state.entries();
            let open = |e: &Entry| e.state == "pending" && e.expires > now;
            if entries
                .values()
                .any(|e| open(e) && e.name.eq_ignore_ascii_case(&start.name))
            {
                return Err(vault::Error::Conflict(
                    "A protected entry for that name is already waiting for the user.".into(),
                ));
            }
            if entries.values().filter(|e| open(e)).count() >= MAX_PENDING {
                return Err(vault::Error::Conflict(
                    "Too many protected entries are waiting. Ask the user to finish or cancel some.".into(),
                ));
            }
            let request = uuid::Uuid::new_v4().to_string();
            entries.insert(
                request.clone(),
                Entry {
                    name: start.name.clone(),
                    description: start.description.clone(),
                    expires: now + ENTRY_SECONDS,
                    state: "pending",
                    version: None,
                    replaces: None,
                },
            );
            Ok(json!({
                "request": request, "state": "pending", "name": start.name, "description": start.description,
                "expires_at": now + ENTRY_SECONDS,
                "next": "Ask the user to type the value in Settings > Credentials > Variables, or run pta variables entry complete REQUEST with the value on standard input. You never receive the value; check variable_entry_status afterwards.",
            }))
        }
        "variable_rotation_start" => {
            let RotateArgs { id } = parse(args)?;
            let id = uuid::Uuid::parse_str(&id)
                .map_err(|_| vault::Error::Input("The entry ID must be a UUID.".into()))?
                .hyphenated()
                .to_string();
            let target = sync::known(host, &store)?
                .into_iter()
                .find(|v| v.id == id)
                .ok_or_else(|| {
                    vault::Error::Input(
                        "There is no stored variable with that ID. Inspect it with variable_list."
                            .into(),
                    )
                })?;
            let mut entries = state.entries();
            let open = |e: &Entry| e.state == "pending" && e.expires > now;
            if entries
                .values()
                .any(|e| open(e) && e.name.eq_ignore_ascii_case(&target.name))
            {
                return Err(vault::Error::Conflict(
                    "A protected entry for that variable is already waiting for the user.".into(),
                ));
            }
            if entries.values().filter(|e| open(e)).count() >= MAX_PENDING {
                return Err(vault::Error::Conflict(
                    "Too many protected entries are waiting. Ask the user to finish or cancel some.".into(),
                ));
            }
            let request = uuid::Uuid::new_v4().to_string();
            entries.insert(
                request.clone(),
                Entry {
                    name: target.name.clone(),
                    description: target.description.clone(),
                    expires: now + ENTRY_SECONDS,
                    state: "pending",
                    version: None,
                    replaces: Some((target.id.clone(), target.version)),
                },
            );
            Ok(json!({
                "request": request, "state": "pending", "action": "replace_value", "id": target.id,
                "name": target.name, "description": target.description, "expires_at": now + ENTRY_SECONDS,
                "next": "Ask the user to type the new value in Settings > Credentials > Variables, or run pta variables entry complete REQUEST with the value on standard input. You never receive the value; check variable_entry_status afterwards. The request fails if the variable changes first.",
            }))
        }
        "variable_audit" => {
            let AuditArgs { limit } = parse(args)?;
            Ok(json!({"events":store.audit_events(limit.unwrap_or(50).clamp(1, 500))?}))
        }
        "variable_entry_status" | "variable_entry_cancel" => {
            let RequestArgs { request } = parse(args)?;
            let mut entries = state.entries();
            let Some(entry) = entries.get_mut(&request) else {
                return Ok(json!({"request":request,"state":"unknown"}));
            };
            if method == "variable_entry_cancel" && entry.state == "pending" {
                entry.state = "cancelled";
            }
            let shown = if entry.state == "pending" && entry.expires <= now {
                "expired"
            } else {
                entry.state
            };
            Ok(
                json!({"request":request,"state":shown,"name":entry.name,"description":entry.description,"version":entry.version,
                    "action": if entry.replaces.is_some() { "replace_value" } else { "create" }}),
            )
        }
        "variable_entry_complete" => {
            let complete: CompleteArgs = parse(args)?;
            let (name, description, replaces) = state.open_entry(&complete.request, now)?;
            let (variable, replayed) = if let Some((id, version)) = replaces {
                // A replacement rotates the value of the entry the request saw: if it changed
                // since, the typed value must not overwrite the newer state.
                let result = store.update(&id, version, None, Some(&complete.value));
                store.audit(
                    "rotate",
                    Some(&id),
                    outcome(&result, result.as_ref().is_ok_and(|u| u.replayed)),
                );
                let updated = result?;
                (updated.variable, updated.replayed)
            } else {
                let created = store.create(NewVariable {
                    id: complete.request.clone(),
                    name,
                    description,
                    value: complete.value,
                })?;
                (created.variable, created.replayed)
            };
            state.settle_entry(&complete.request, variable.version);
            Ok(json!({"variable":variable,"replayed":replayed}))
        }
        "variable_verify" => {
            let report = store.verify()?;
            Ok(
                json!({"checked":report.checked,"failed":report.failed,"ok":report.failed.is_empty()}),
            )
        }
        "variable_key_backup" => {
            let BackupArgs { output } = parse(args)?;
            if !output.is_absolute() {
                return Err(vault::Error::Input(
                    "Use an absolute private output path.".into(),
                ));
            }
            let backup = store.export_keys()?;
            let versions: Vec<u32> = backup.masters.keys().copied().collect();
            vault::write_new_private(
                &output,
                &serde_json::to_string(&backup).map_err(|_| {
                    vault::Error::Storage("The backup could not be written.".into())
                })?,
            )?;
            Ok(
                json!({"path":output,"master_versions":versions,"contains":"master keys only; keep the file apart from the profile and its backups"}),
            )
        }
        "variable_key_restore" => {
            let RestoreArgs { backup } = parse(args)?;
            let restored = store.import_keys(&backup)?;
            let report = store.verify()?;
            Ok(
                json!({"restored":restored,"checked":report.checked,"failed":report.failed,"ok":report.failed.is_empty()}),
            )
        }
        "variable_key_rotate" => {
            let rotation = store.rotate()?;
            let report = store.verify()?;
            Ok(json!({"rotation":rotation,"ok":report.failed.is_empty(),"failed":report.failed}))
        }
        _ => Err(vault::Error::Input("Unknown variable operation.".into())),
    }
}

#[cfg(test)]
pub(in crate::app) mod sync_tests;
#[cfg(test)]
pub(in crate::app) mod tests;
