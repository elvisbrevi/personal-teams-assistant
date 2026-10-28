//! Host operations that let a consumer use an account variable by reference.
//!
//! Every operation re-checks the account's live authority and resolves the variable at the moment
//! of use: a deleted, rotated-since-chosen or foreign reference fails and never falls back to
//! another account, the environment or a stored credential. No operation returns a value.
//! Built-in consumers (DeepSeek) are available to the account's GUI, CLI and MCP. Launchers run a
//! local program and exist only on the trusted personal machine: the application window or the
//! local CLI of an installation owner, inside its allowed roots.
use super::{authority, control::Reply, implementation, variables as vars, *};
use crate::{
    consumers::{self as rules, BUILT_IN, Definition},
    variables::{self as vault, Secret, Store},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

mod connected;
pub(super) use connected::stage;

pub(super) struct State {
    launchers: rules::Launchers,
    /// What an installation signed in to a coordinator has bound; never a value.
    bindings: connected::Bindings,
    /// Values resolved online for the operation about to run (memory only, short-lived).
    staged: std::sync::Mutex<BTreeMap<String, connected::Staged>>,
    /// One launcher runs at a time.
    busy: tokio::sync::Mutex<()>,
    /// Serializes read-modify-write of the launcher list.
    edit: std::sync::Mutex<()>,
    /// Where a built-in consumer's check is sent; tests point it at a local double.
    #[cfg(test)]
    pub(super) origin: std::sync::Mutex<Option<String>>,
}

impl State {
    pub(super) fn new(profile: &Path) -> Self {
        Self {
            launchers: rules::Launchers::new(profile),
            bindings: connected::Bindings::new(profile),
            staged: std::sync::Mutex::new(BTreeMap::new()),
            busy: tokio::sync::Mutex::new(()),
            edit: std::sync::Mutex::new(()),
            #[cfg(test)]
            origin: std::sync::Mutex::new(None),
        }
    }
    fn origin(&self, consumer: &rules::BuiltIn) -> String {
        #[cfg(test)]
        if let Some(origin) = self.origin.lock().unwrap().clone() {
            return origin;
        }
        consumer.destination.to_owned()
    }
}

pub(super) fn handles(method: &str) -> bool {
    method.starts_with("variable_consumer_") || method.starts_with("variable_launcher_")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindArgs {
    consumer: String,
    variable: String,
    #[serde(default)]
    version: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsumerArgs {
    consumer: String,
    #[serde(default)]
    version: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DefineArgs {
    id: String,
    description: String,
    program: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    directory: PathBuf,
    env: BTreeMap<String, String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApproveArgs {
    pub id: String,
    /// The program digest the user was shown.
    pub digest: String,
}

pub(super) fn approve(
    launchers: &rules::Launchers,
    a: &ApproveArgs,
) -> std::result::Result<Value, vault::Error> {
    let launcher = launchers.approve(&a.id, &a.digest, chrono::Utc::now().timestamp())?;
    Ok(json!({"launcher":launcher.id,"approved_at":launcher.approved_at}))
}

/// What a person reviews before approving a launcher; the digest identifies that definition.
pub(super) fn launcher_view(launcher: &rules::Launcher, variables: Vec<Value>) -> Value {
    json!({
        "id":launcher.id,
        "description":launcher.description,
        "program":launcher.program,
        "digest":launcher.digest,
        "args":launcher.args,
        "directory":launcher.directory,
        "timeout_seconds":launcher.timeout_seconds,
        "approved":launcher.approved_at.is_some(),
        "variables":variables,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LauncherArgs {
    id: String,
    /// Environment name to the variable version the caller expects.
    #[serde(default)]
    versions: BTreeMap<String, u64>,
}

fn parse<T: DeserializeOwned>(args: &Value) -> std::result::Result<T, vault::Error> {
    serde_json::from_value(args.clone())
        .map_err(|_| vault::Error::Input("Invalid arguments for this variable operation.".into()))
}
fn input(message: impl Into<String>) -> vault::Error {
    vault::Error::Input(message.into())
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Reply {
    match run(host, &request.method, &request.args).await {
        Ok(mut data) => {
            if request.method == "variable_consumer_list" {
                let link = implementation::link(&host.state).ok().flatten();
                data["standing"] = host.state.standing.view(link.as_ref());
            }
            Reply::success(data)
        }
        Err(error) => vars::failure(error),
    }
}

fn consumer_of(id: &str) -> std::result::Result<&'static rules::BuiltIn, vault::Error> {
    rules::built_in(id).ok_or_else(|| {
        input("Unknown consumer. List the supported ones with `pta consumers list`.")
    })
}

fn audit_word(ok: bool) -> &'static str {
    if ok { "ok" } else { "failed" }
}
fn audit_result<T>(
    store: &Store,
    action: &str,
    entry: Option<&str>,
    result: &std::result::Result<T, vault::Error>,
) {
    let outcome = match result {
        Ok(_) => "ok",
        Err(vault::Error::Input(_)) => "rejected",
        Err(vault::Error::Conflict(message)) if message.contains("was deleted") => "deleted",
        Err(vault::Error::Conflict(_)) => "stale",
        Err(_) => "failed",
    };
    store.audit(action, entry, outcome);
}

async fn run(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> std::result::Result<Value, vault::Error> {
    let invocation = authority::invocation(&host.state)
        .map_err(|_| vault::Error::Conflict(authority::ACCOUNT_UNAVAILABLE.into()))?;
    let transport = invocation.transport;
    let local = matches!(
        transport,
        authority::Transport::Local | authority::Transport::Window
    );
    let state = &host.state.variables;
    match method {
        "variable_consumer_list"
        | "variable_consumer_bind"
        | "variable_consumer_unbind"
        | "variable_consumer_check" => {
            if transport == authority::Transport::Machine {
                return Err(vault::Error::Conflict(
                    "This variable operation is not available from this client.".into(),
                ));
            }
        }
        "variable_launcher_define" | "variable_launcher_remove" | "variable_launcher_run" => {
            // Generic process execution: only the owner of this installation, on this machine.
            if !invocation.scope.owner {
                return Err(vault::Error::Conflict(authority::EXECUTION_DENIED.into()));
            }
            if !local {
                if let Ok(store) = state.store(host) {
                    store.audit("launcher", None, "denied_client");
                }
                return Err(vault::Error::Conflict(
                    "Launchers run only from this machine's application window or local CLI."
                        .into(),
                ));
            }
        }
        "variable_launcher_approve" => {
            // A person at this machine's window approves what an agent may only propose.
            if !invocation.scope.owner {
                return Err(vault::Error::Conflict(authority::EXECUTION_DENIED.into()));
            }
            if transport != authority::Transport::Window {
                if let Ok(store) = state.store(host) {
                    store.audit("launcher_approve", None, "denied_client");
                }
                return Err(vault::Error::Conflict(
                    "Only the user approves a launcher, in this machine's application window."
                        .into(),
                ));
            }
        }
        _ => return Err(input("Unknown variable operation.")),
    }
    // Signed in to a coordinator: the account's variables are not on this computer. Every
    // choice and use goes through the coordinator and never falls back to a local store.
    if let Some(link) = implementation::link(&host.state)
        .map_err(|_| vault::Error::Storage("The coordinator connection is unreadable.".into()))?
    {
        return connected::run(host, &link, method, args, invocation.scope.owner && local).await;
    }
    let store = state.store(host)?;
    match method {
        "variable_consumer_list" => list(host, &store, invocation.scope.owner && local),
        "variable_consumer_bind" => {
            let a: BindArgs = parse(args)?;
            let result = bind(host, &store, &a).await;
            audit_result(&store, "bind", Some(&a.variable), &result);
            result
        }
        "variable_consumer_unbind" => {
            let a: ConsumerArgs = parse(args)?;
            let consumer = consumer_of(&a.consumer)?;
            let removed = store.unbind(consumer.id, consumer.slot)?;
            store.audit("unbind", None, audit_word(removed));
            Ok(
                json!({"consumer":consumer.id,"unbound":removed,"restart_required":restart_needed(host).await}),
            )
        }
        "variable_consumer_check" => {
            let a: ConsumerArgs = parse(args)?;
            let consumer = consumer_of(&a.consumer)?;
            let result = check(host, &store, consumer, a.version).await;
            audit_result(&store, "use", None, &result);
            result
        }
        "variable_launcher_define" => {
            let a: DefineArgs = parse(args)?;
            let result = define(host, &store, a).await;
            audit_result(&store, "launcher_define", None, &result);
            result
        }
        "variable_launcher_remove" => {
            let a: LauncherArgs = parse(args)?;
            let _edit = state
                .consumers
                .edit
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let removed = state.consumers.launchers.remove(&a.id)?;
            store.audit("launcher_remove", None, audit_word(removed));
            Ok(json!({"launcher":a.id,"removed":removed}))
        }
        "variable_launcher_approve" => {
            let a: ApproveArgs = parse(args)?;
            let _edit = state
                .consumers
                .edit
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let result = approve(&state.consumers.launchers, &a);
            audit_result(&store, "launcher_approve", None, &result);
            result
        }
        _ => {
            let a: LauncherArgs = parse(args)?;
            let result = launch(host, &store, a).await;
            audit_result(&store, "use", None, &result);
            result
        }
    }
}

/// The value of one entry of the current account, resolved now: from this installation's own
/// store, or from the coordinator when signed in to one. For an operation that verifies or uses
/// it internally; the value is never returned to a client.
pub(in crate::app) async fn resolve_entry(
    host: &Host,
    entry: &str,
    expected: Option<u64>,
) -> std::result::Result<(vault::Metadata, Secret), vault::Error> {
    if implementation::link(&host.state)
        .map_err(|_| vault::Error::Storage("The coordinator connection is unreadable.".into()))?
        .is_some()
    {
        return connected::resolve(&host.state, entry, expected).await;
    }
    host.state.variables.store(host)?.use_entry(entry, expected)
}

/// Whether a running assistant still holds the earlier credential (it reads it at start). A
/// start or stop in progress holds the lock: the answer is then yes, without waiting for it.
async fn restart_needed(host: &Host) -> bool {
    host.state
        .running
        .try_lock()
        .map_or(true, |running| running.is_some())
}

fn binding_view(binding: &vault::Binding) -> Value {
    json!({
        "state":binding.state,
        "id":binding.entry,
        "variable":binding.variable,
        "bound_at":binding.bound_at,
    })
}

fn list(host: &Host, store: &Store, launchers: bool) -> std::result::Result<Value, vault::Error> {
    let bindings = store.bindings()?;
    let consumers: Vec<Value> = BUILT_IN
        .iter()
        .map(|consumer| {
            let binding = bindings
                .iter()
                .find(|b| b.consumer == consumer.id && b.slot == consumer.slot);
            json!({
                "id":consumer.id,
                "title":consumer.title,
                "summary":consumer.summary,
                "destination":consumer.destination,
                "slot":consumer.slot,
                "credential":consumer.credential,
                "operations":consumer.operations,
                "binding":binding.map(binding_view),
            })
        })
        .collect();
    let mut result = json!({"consumers":consumers,"launchers_available":launchers});
    if launchers {
        let list = host.state.variables.consumers.launchers.list()?;
        let mut views = Vec::new();
        for launcher in list {
            let mut variables = Vec::new();
            for (env, entry) in &launcher.env {
                let (state, variable) = store.reference(entry)?;
                variables.push(json!({"env":env,"id":entry,"state":state,"variable":variable}));
            }
            views.push(launcher_view(&launcher, variables));
        }
        result["launchers"] = json!(views);
    }
    Ok(result)
}

async fn bind(
    host: &Host,
    store: &Store,
    a: &BindArgs,
) -> std::result::Result<Value, vault::Error> {
    let consumer = consumer_of(&a.consumer)?;
    // Choosing a variable is checked against the consumer now, so a wrong one is found early.
    let (metadata, value) = store.use_entry(&a.variable, a.version)?;
    rules::validate_value(consumer, value.expose())?;
    drop(value);
    let binding = store.bind(consumer.id, consumer.slot, &metadata.id)?;
    Ok(json!({
        "consumer":consumer.id,
        "binding":binding_view(&binding),
        "restart_required":restart_needed(host).await,
    }))
}

async fn check(
    host: &Host,
    store: &Store,
    consumer: &'static rules::BuiltIn,
    version: Option<u64>,
) -> std::result::Result<Value, vault::Error> {
    let Some(binding) = store.binding(consumer.id, consumer.slot)? else {
        return Err(input(
            "No variable is bound to this consumer. Choose one with `pta consumers bind`.",
        ));
    };
    let (metadata, value) = store.use_entry(&binding.entry, version)?;
    rules::validate_value(consumer, value.expose())?;
    let origin = host.state.variables.consumers.origin(consumer);
    let result = rules::check_at(&origin, &value).await;
    drop(value);
    Ok(json!({
        "consumer":consumer.id,
        "operation":"check",
        "destination":consumer.destination,
        "reference":{"id":metadata.id,"name":metadata.name,"version":metadata.version},
        "result":result,
    }))
}

async fn define(
    host: &Host,
    store: &Store,
    a: DefineArgs,
) -> std::result::Result<Value, vault::Error> {
    let directory = implementation::directories(host, std::slice::from_ref(&a.directory))
        .await
        .map_err(|error| input(error.to_string()))?
        .remove(0);
    for entry in a.env.values() {
        match store.reference(entry)?.0 {
            "ok" => {}
            "deleted" => {
                return Err(vault::Error::Conflict(
                    "A referenced variable was deleted. Refresh the list.".into(),
                ));
            }
            _ => return Err(input("A referenced variable is not in this account.")),
        }
    }
    let launcher = rules::define(
        Definition {
            id: a.id,
            description: a.description,
            program: a.program,
            args: a.args,
            directory,
            env: a.env,
            timeout_seconds: a.timeout_seconds,
        },
        chrono::Utc::now().timestamp(),
    )?;
    let consumers = &host.state.variables.consumers;
    let _edit = consumers.edit.lock().unwrap_or_else(|e| e.into_inner());
    consumers.launchers.add(launcher.clone())?;
    Ok(json!({"launcher":{
        "id":launcher.id,
        "program":launcher.program,
        "directory":launcher.directory,
        "timeout_seconds":launcher.timeout_seconds,
        "variables":launcher.env.keys().collect::<Vec<_>>(),
    }}))
}

async fn launch(
    host: &Host,
    store: &Store,
    a: LauncherArgs,
) -> std::result::Result<Value, vault::Error> {
    let consumers = &host.state.variables.consumers;
    let _busy = consumers
        .busy
        .try_lock()
        .map_err(|_| vault::Error::Conflict("A launcher is already running.".into()))?;
    let launcher = consumers.launchers.get(&a.id)?;
    rules::ensure_approved(&launcher)?;
    if let Some(unknown) = a
        .versions
        .keys()
        .find(|env| !launcher.env.contains_key(*env))
    {
        return Err(input(format!(
            "{unknown} is not one of this launcher's variables."
        )));
    }
    // The directory must still be inside the machine's allowed roots.
    let confined = implementation::directories(host, std::slice::from_ref(&launcher.directory))
        .await
        .map_err(|error| input(error.to_string()))?;
    if confined.first() != Some(&launcher.directory) {
        return Err(vault::Error::Conflict(
            "The approved working directory changed. Define the launcher again.".into(),
        ));
    }
    // Resolve every value first: any denied, deleted or stale reference stops before a start.
    let mut values: Vec<(String, Secret)> = Vec::new();
    let mut used = Vec::new();
    for (env, entry) in &launcher.env {
        let (metadata, value) = store.use_entry(entry, a.versions.get(env).copied())?;
        used.push(
            json!({"env":env,"id":metadata.id,"name":metadata.name,"version":metadata.version}),
        );
        values.push((env.clone(), value));
    }
    let result = rules::run(&launcher, values).await?;
    Ok(json!({
        "launcher":launcher.id,
        "operation":"run",
        "variables":used,
        "result":result,
        "output":"not captured",
    }))
}

/// Supplies application credentials from account variables while a consumer binding exists.
pub(super) struct Resolver {
    host: std::sync::Weak<Host>,
}

impl Resolver {
    pub(super) fn new(host: &Arc<Host>) -> Self {
        Self {
            host: Arc::downgrade(host),
        }
    }
    /// The live host's store, after the account authority check. `None` once the host is gone
    /// (process shutdown): nothing can be bound then, and nothing is read.
    fn live(&self) -> anyhow::Result<Option<Arc<Host>>> {
        let Some(host) = self.host.upgrade() else {
            return Ok(None);
        };
        authority::invocation(&host.state)?;
        Ok(Some(host))
    }
}

impl crate::security::AccountSecrets for Resolver {
    fn bound(&self, name: &str) -> anyhow::Result<bool> {
        let Some(host) = self.live()? else {
            return Ok(false);
        };
        let config = read_config(&host.state.config_path)?;
        Ok(vars::credentials::managed(&config, name))
    }
    fn present(&self, name: &str) -> anyhow::Result<bool> {
        let host = self.live()?.context("The host is no longer running.")?;
        if let Some(link) = implementation::link(&host.state)? {
            if let Some(consumer) = rules::built_in_credential(name)
                && host
                    .state
                    .variables
                    .consumers
                    .bindings
                    .has(&link, consumer.id, consumer.slot)
            {
                return Ok(true);
            }
            return Ok(vars::cached(&host, &link)
                .iter()
                .any(|v| v.name.eq_ignore_ascii_case(name)));
        }
        let store = host.state.variables.store(&host)?;
        vars::credentials::migrate(&host, &store)?;
        if let Some(consumer) = rules::built_in_credential(name)
            && let Some(binding) = store.binding(consumer.id, consumer.slot)?
        {
            return Ok(binding.state == "ok");
        }
        Ok(store
            .list(None)?
            .iter()
            .any(|v| v.name.eq_ignore_ascii_case(name)))
    }
    fn value(&self, name: &str) -> anyhow::Result<String> {
        let host = self.live()?.context("The host is no longer running.")?;
        if let Some(link) = implementation::link(&host.state)? {
            if let Some(consumer) = rules::built_in_credential(name)
                && host
                    .state
                    .variables
                    .consumers
                    .bindings
                    .has(&link, consumer.id, consumer.slot)
            {
                return host
                    .state
                    .variables
                    .consumers
                    .staged(name)
                    .map_err(anyhow::Error::msg);
            }
            let entry = vars::cached(&host, &link).into_iter()
                .find(|v| v.name.eq_ignore_ascii_case(name))
                .context("The application variable is missing. Synchronize the catalog and add it in Variables.")?;
            // The core's credential contract is synchronous, including reads by running tools.
            // A separate thread avoids blocking a Tokio worker on its own runtime and performs
            // a fresh authorization for every use; no account value is cached on this machine.
            return std::thread::spawn(move || -> anyhow::Result<String> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                if implementation::link(&host.state)?.as_ref() != Some(&link) {
                    anyhow::bail!("The signed-in account changed. Resolve the variable again.");
                }
                let (_, value) =
                    runtime.block_on(connected::resolve(&host.state, &entry.id, None))?;
                if implementation::link(&host.state)?.as_ref() != Some(&link) {
                    anyhow::bail!("The signed-in account changed. Resolve the variable again.");
                }
                Ok(value.expose().to_owned())
            })
            .join()
            .map_err(|_| anyhow::anyhow!("The account variable could not be resolved."))?;
        }
        let store = host.state.variables.store(&host)?;
        vars::credentials::migrate(&host, &store)?;
        let result = (|| -> std::result::Result<_, vault::Error> {
            if let Some(consumer) = rules::built_in_credential(name)
                && store.is_bound(consumer.id, consumer.slot)?
            {
                let (_, value) = store
                    .bound_value(consumer.id, consumer.slot)?
                    .ok_or_else(|| input("The binding was removed."))?;
                rules::validate_value(consumer, value.expose())?;
                return Ok(value);
            }
            store.resolve(name)
        })();
        audit_result(&store, "use", None, &result);
        Ok(result?.expose().to_owned())
    }
}

/// Make the host's credential lookups prefer a bound account variable. Called once per host
/// process; a credential with a binding never falls back to another source.
pub(super) fn install(host: &Arc<Host>) {
    crate::security::account_secrets(Some(Arc::new(Resolver::new(host))));
}

#[cfg(test)]
mod tests;
