//! Account variables for the GUI, CLI and MCP: one host operation per behavior.
//!
//! A value enters only through protected entry (the GUI's masked field or CLI standard input) and
//! is never returned by any operation. An agent starts a protected-entry request by name and
//! description and later reads only its state.
use super::{authority, control::Reply, *};
use crate::variables::{self as vault, KeyBackup, NewVariable, Secret, Store};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// A protected-entry request stays open this long (seconds).
const ENTRY_SECONDS: i64 = 15 * 60;
const MAX_PENDING: usize = 20;

pub(super) struct State {
    profile: PathBuf,
    store: std::sync::Mutex<Option<Arc<Store>>>,
    entries: std::sync::Mutex<BTreeMap<String, Entry>>,
}

struct Entry {
    name: String,
    description: String,
    expires: i64,
    state: &'static str,
    version: Option<u64>,
}

impl State {
    pub(super) fn new(profile: &Path) -> Self {
        Self {
            profile: profile.to_path_buf(),
            store: std::sync::Mutex::new(None),
            entries: std::sync::Mutex::new(BTreeMap::new()),
        }
    }
    fn store(&self, host: &Host) -> std::result::Result<Arc<Store>, vault::Error> {
        let mut slot = self.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(store) = slot.as_ref() {
            return Ok(store.clone());
        }
        let account = host.state.authority.account.as_ref().map(|a| a.id.as_str());
        let store = Arc::new(Store::open_profile(&self.profile, account)?);
        *slot = Some(store.clone());
        Ok(store)
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
struct StartArgs {
    name: String,
    description: String,
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

fn failure(error: vault::Error) -> Reply {
    let (code, exit) = match &error {
        vault::Error::Input(_) => ("invalid_input", 2),
        vault::Error::Conflict(_) => ("state_conflict", 6),
        vault::Error::Key(_) => ("not_ready", 3),
        vault::Error::Integrity(_) | vault::Error::Storage(_) => ("operation_failed", 1),
    };
    Reply::error(code, exit, &error.to_string())
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Reply {
    match run(host, &request.method, &request.args) {
        Ok(data) => Reply::success(data),
        Err(error) => failure(error),
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
        | "variable_entry_status"
        | "variable_entry_complete"
        | "variable_entry_cancel" => transport != authority::Transport::Machine,
        "variable_entry_start"
        | "variable_verify"
        | "variable_key_backup"
        | "variable_key_restore"
        | "variable_key_rotate" => transport == authority::Transport::Local,
        _ => return Err(vault::Error::Input("Unknown variable operation.".into())),
    };
    if !allowed {
        return Err(vault::Error::Conflict(
            "This variable operation is not available from this client.".into(),
        ));
    }
    let state = &host.state.variables;
    let store = state.store(host)?;
    let now = chrono::Utc::now().timestamp();
    match method {
        "variable_list" => {
            let list: ListArgs = parse(args)?;
            let mut variables = store.list(list.query.as_deref())?;
            let total = variables.len();
            variables.truncate(list.limit.unwrap_or(vault::MAX_VARIABLES).max(1));
            let pending: Vec<Value> = state
                .entries()
                .iter()
                .filter(|(_, e)| e.state == "pending" && e.expires > now)
                .map(|(request, e)| {
                    json!({"request":request,"name":e.name,"description":e.description,"expires_at":e.expires})
                })
                .collect();
            Ok(json!({
                "variables": variables,
                "total": total,
                "pending": pending,
                "limits": {"name":vault::MAX_NAME,"description":vault::MAX_DESCRIPTION,"value_bytes":vault::MAX_VALUE,"variables":vault::MAX_VARIABLES},
            }))
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
            if store
                .list(None)?
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
                },
            );
            Ok(json!({
                "request": request, "state": "pending", "name": start.name, "description": start.description,
                "expires_at": now + ENTRY_SECONDS,
                "next": "Ask the user to type the value in Settings > Credentials > Variables, or run pta variables entry complete REQUEST with the value on standard input. You never receive the value; check variable_entry_status afterwards.",
            }))
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
                json!({"request":request,"state":shown,"name":entry.name,"description":entry.description,"version":entry.version}),
            )
        }
        "variable_entry_complete" => {
            let complete: CompleteArgs = parse(args)?;
            let (name, description) = {
                let entries = state.entries();
                let entry = entries.get(&complete.request).ok_or_else(|| {
                    vault::Error::Input("That protected entry is unknown or has expired.".into())
                })?;
                if !(entry.state == "completed"
                    || (entry.state == "pending" && entry.expires > now))
                {
                    return Err(vault::Error::Input(format!(
                        "That protected entry is {}.",
                        if entry.state == "pending" {
                            "expired"
                        } else {
                            entry.state
                        }
                    )));
                }
                (entry.name.clone(), entry.description.clone())
            };
            let created = store.create(NewVariable {
                id: complete.request.clone(),
                name,
                description,
                value: complete.value,
            })?;
            if let Some(entry) = state.entries().get_mut(&complete.request) {
                entry.state = "completed";
                entry.version = Some(created.variable.version);
            }
            Ok(json!({"variable":created.variable,"replayed":created.replayed}))
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
mod tests;
