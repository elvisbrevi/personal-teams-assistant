//! Work-item implementation, with durable runs and account-bound execution machines.
use super::*;
use crate::store::{
    implementation::{ImplementationStore, LocalImplementation},
    organization::{LocalOrganization, OrganizationStore},
    settings::SettingsStore,
};
use crate::tools::todo::{Detail, DetailRequest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub(super) mod checkout;
pub(super) mod director;
pub(super) mod fleet;
pub(super) mod join;
pub(super) mod login;
mod management;
pub(super) mod paths;
mod requests;
mod store;
pub(super) use fleet::register;
pub(crate) use store::Database;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    name: String,
    roots: Vec<PathBuf>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            name: std::env::var("HOSTNAME")
                .or_else(|_| std::env::var("COMPUTERNAME"))
                .unwrap_or_else(|_| std::env::consts::OS.into()),
            roots: Vec::new(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Machine {
    id: String,
    name: String,
    os: String,
    seen: i64,
    secret_hash: String,
    #[serde(default)]
    account_version: String,
    #[serde(default)]
    assistant_phase: Option<AssistantPhase>,
    /// Hash of the enrolled installation's identity key; empty for older pairings.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    installation: String,
    #[serde(default)]
    revoked: bool,
    #[serde(default)]
    roots: Vec<PathBuf>,
    #[serde(default)]
    management_support: bool,
    /// Its installation in the account store, revoked there when the machine is revoked.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    store_installation: String,
    /// The public key the person approved, with the account store, when trusting the machine: a
    /// replaced service token or a rotated content key is sealed to it later. Empty when the
    /// machine was trusted without the account store or before 0.10.38.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    store_key: String,
    /// What the machine last said it holds of the account store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    store_held: Option<StoreHeld>,
    /// The version the machine's host runs, as its last poll reported it; none from older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host_version: Option<String>,
    /// Its update state as its last poll reported it; none from older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    update: Option<UpdateReport>,
    /// Whether its last poll said it answers `checkout_inspect`; older hosts do not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    checkout_inspect: bool,
    /// Whether its last poll said it runs the workflow director (`workflow_direct`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    workflow_direct: bool,
    /// Whether its last poll said another device may ask it to update itself (`machine_upgrade`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    upgrade: bool,
}
impl Machine {
    /// Trusted and seen in its last polls (a worker polls every two seconds).
    fn online(&self) -> bool {
        !self.revoked && now() - self.seen < 35
    }
}
/// A machine's update state as it reports it: shown in the portal, never acted on there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateReport {
    /// `current`, `available`, `staging`, `staged`, `deferred`, `applying` or `unsupported`.
    state: String,
    /// The newer release waiting to be installed.
    #[serde(default)]
    pending: Option<String>,
    /// A newer release whose installation failed and was rolled back.
    #[serde(default)]
    failed: Option<String>,
    #[serde(default)]
    unattended: bool,
}
impl UpdateReport {
    const STATES: [&str; 7] = [
        "current",
        "available",
        "staging",
        "staged",
        "deferred",
        "applying",
        "unsupported",
    ];
    /// What a poll carries, or nothing when it is not exactly a report: a version is a release
    /// number and the state one of the fixed words, so no text of the machine reaches the portal.
    fn parse(value: &Value) -> Option<Self> {
        let report: Self = serde_json::from_value(value.clone()).ok()?;
        let version = |v: &Option<String>| {
            v.as_deref()
                .is_none_or(|v| crate::maintenance::Version::parse(v).is_some())
        };
        (Self::STATES.contains(&report.state.as_str())
            && version(&report.pending)
            && version(&report.failed))
        .then_some(report)
    }
}
/// A machine's account store credentials as it last reported them: its service token's
/// fingerprint and its content key's active version, never the values.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreHeld {
    token: String,
    key: u32,
    at: i64,
}
/// The coordinator and account this installation signed in to. Anything synchronized from that
/// account is qualified by it, so a different coordinator, account or machine never reuses it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Link {
    pub origin: String,
    pub account: u64,
    pub machine: String,
}
/// The coordinator connection of this installation, if it has one.
/// The coordinator's account store mode (`off`, `mirror` or `primary`), asked over the machine
/// protocol: a signed-in computer joins the store as its source only when the coordinator has.
pub(super) async fn coordinator_store_mode(host: &Arc<Host>) -> Result<String> {
    fleet::coordinator_store_mode(host).await
}

/// A machine of the account trusted with the account store, as the coordinator knows it.
pub(super) struct StoreMachine {
    pub id: String,
    pub name: String,
    pub store_installation: String,
    /// Whether the coordinator recorded a key to seal new credentials to.
    pub receives: bool,
    /// What it last reported: token fingerprint, content key version, when.
    pub held: Option<(String, u32, i64)>,
}

/// The machines (not revoked) that were given the account store or joined it.
pub(super) async fn store_machines(host: &Arc<Host>) -> Vec<StoreMachine> {
    let data = host.state.implementation.registry.lock().await;
    data.machines
        .iter()
        .filter(|m| !m.revoked && (!m.store_key.is_empty() || !m.store_installation.is_empty()))
        .map(|m| StoreMachine {
            id: m.id.clone(),
            name: m.name.clone(),
            store_installation: m.store_installation.clone(),
            receives: !m.store_key.is_empty(),
            held: m
                .store_held
                .as_ref()
                .map(|h| (h.token.clone(), h.key, h.at)),
        })
        .collect()
}

/// A machine of this coordinator, as the lost computer path needs it.
pub(super) struct KnownMachine {
    pub id: String,
    pub name: String,
    pub store_installation: String,
    pub revoked: bool,
}

/// The machine with this ID, or the one that joined the account store as this installation; the
/// coordinator's own ID is `Err`.
pub(super) async fn known_machine(
    host: &Arc<Host>,
    machine: Option<&str>,
    installation: Option<&str>,
) -> std::result::Result<Option<KnownMachine>, ()> {
    let data = host.state.implementation.registry.lock().await;
    if machine.is_some_and(|id| id == data.id) {
        return Err(());
    }
    Ok(data
        .machines
        .iter()
        .find(|m| {
            machine.is_some_and(|id| m.id == id)
                || installation.is_some_and(|id| !id.is_empty() && m.store_installation == id)
        })
        .map(|m| KnownMachine {
            id: m.id.clone(),
            name: m.name.clone(),
            store_installation: m.store_installation.clone(),
            revoked: m.revoked,
        }))
}

/// Revoke a machine on this coordinator (`machine_revoke` without its checks of a trusted,
/// listed machine); returns its installation in the account store.
pub(super) async fn revoke_machine(host: &Arc<Host>, id: &str) -> Result<String> {
    management::revoke_now(host, id).await
}

pub(super) fn link(state: &DesktopState) -> Result<Option<Link>> {
    Ok(state.implementation.connection()?.map(|c| Link {
        origin: c.url,
        account: c.account,
        machine: c.id,
    }))
}
#[derive(Clone, Serialize, Deserialize)]
struct Connection {
    url: String,
    account: u64,
    id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    provider: String,
    id: u64,
    project: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    task: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    machine: String,
    item: Selection,
    context: String,
    directories: Vec<PathBuf>,
    provider: String,
    model: String,
    effort: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Launch {
    pub(crate) id: String,
    request: Start,
    pub(crate) detail: Detail,
    language: crate::config::Language,
    planning: bool,
    /// The user's instructions for this kind of run, resolved where it was started; a launch
    /// from an older host has none and uses the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    /// The work item guides that apply: the item's own type, or every type in its project when
    /// planning children.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    guides: Vec<crate::work_items::Guide>,
    /// The organization's norms, architecture and conventions for the item's project, within
    /// the run's budget; untrusted input, like the guides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    organization: Vec<crate::organization::Entry>,
    /// What the workflow that asked for this coding run lifts from its fixed rules
    /// (`Workflow::allow`). Only the workflow engine sets it, and only on a coding run;
    /// `implementation_start` never does, and a worker that predates the field ignores it and
    /// keeps every prohibition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) allow: Vec<crate::workflows::Allow>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Run {
    pub(crate) launch: Launch,
    pub(crate) state: String,
    created_at: String,
    pub(crate) finished_at: Option<String>,
    pub(crate) output: String,
    pub(crate) plan: Option<Value>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    remote: Option<crate::llm::implementation::mobile::Access>,
    /// The permission mode the execution machine launched the CLI with
    /// (`llm::implementation::permission_mode`); none in a record from an older host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) permission_mode: Option<String>,
    /// The permission prompts the CLI still raised and nobody answered (bounded, redacted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) prompts: Vec<String>,
}
/// The final state of a coding run that stopped on a permission prompt, recorded with its text
/// so its cause can be removed; distinct from `needs_attention`.
pub(crate) const PERMISSION_PROMPT: &str = "permission_prompt";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionSummary {
    id: String,
    machine: String,
    item: crate::tools::todo::Item,
    provider: String,
    model: String,
    #[serde(default)]
    effort: String,
    state: String,
    session_id: Option<String>,
    #[serde(default)]
    remote: Option<crate::llm::implementation::mobile::Access>,
    created_at: String,
    started_at: Option<String>,
    finished_at: Option<String>,
}
impl From<Run> for SessionSummary {
    fn from(run: Run) -> Self {
        let mut item = run.launch.detail.item;
        item.dates.clear();
        Self {
            id: run.launch.id,
            machine: run.launch.request.machine,
            item,
            provider: run.launch.request.provider,
            model: run.launch.request.model,
            effort: run.launch.request.effort,
            state: run.state,
            session_id: run.session_id,
            remote: run.remote,
            created_at: run.created_at,
            started_at: run.started_at,
            finished_at: run.finished_at,
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Registry {
    id: String,
    settings: Settings,
    teams_machine: String,
    #[serde(default)]
    teams_lease: Option<(String, i64)>,
    machines: Vec<Machine>,
    #[serde(default)]
    creations: BTreeMap<String, Value>,
}
struct Call {
    id: String,
    machine: String,
    method: String,
    args: Value,
    claimed: bool,
    result: Option<Value>,
}
pub(super) struct State {
    root: PathBuf,
    registry: Mutex<Registry>,
    tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    calls: Mutex<Vec<Call>>,
    login: Mutex<Option<login::Pending>>,
    pub(in crate::app) db: store::Database,
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn uuid(text: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(text).is_ok(),
        "Invalid machine or run ID."
    );
    Ok(())
}
impl State {
    /// Another machine takes the Teams selection (tests of what this one may then do).
    #[cfg(test)]
    pub(in crate::app) async fn select_teams_machine(&self, id: &str) {
        self.registry.lock().await.teams_machine = id.into();
    }
    pub(super) fn new(profile: &Path) -> Result<Self> {
        let root = profile.join("implementation");
        security::private_dir(&root)?;
        let path = root.join("registry.json");
        let mut registry: Registry = if path.exists() {
            serde_json::from_slice(&fs::read(path)?)?
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            Registry {
                id: id.clone(),
                settings: Settings::default(),
                teams_machine: id,
                teams_lease: None,
                machines: Vec::new(),
                creations: BTreeMap::new(),
            }
        };
        // Roots saved with the Windows verbatim prefix read back in their plain spelling.
        registry.settings.roots = paths::plain_all(&registry.settings.roots);
        for machine in &mut registry.machines {
            machine.roots = paths::plain_all(&machine.roots);
        }
        for record in registry.creations.values_mut() {
            if record["state"] == "sending" {
                record["state"] = json!("uncertain");
            }
        }
        write_private(
            &root.join("registry.json"),
            &serde_json::to_string(&registry)?,
        )?;
        let state = Self {
            db: store::Database::open(&root.join("runs.db"))?,
            root,
            registry: Mutex::new(registry),
            tasks: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            login: Mutex::new(None),
        };
        Ok(state)
    }
    fn save(&self, data: &Registry) -> Result<()> {
        write_private(
            &self.root.join("registry.json"),
            &serde_json::to_string(data)?,
        )
    }
    fn reset_roots(&self, data: &mut Registry) -> Result<()> {
        let previous = std::mem::take(&mut data.settings.roots);
        if let Err(error) = self.save(data) {
            data.settings.roots = previous;
            return Err(error);
        }
        Ok(())
    }
    fn connection(&self) -> Result<Option<Connection>> {
        let path = self.root.join("connection.json");
        if path.exists() {
            Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
        } else {
            Ok(None)
        }
    }
}

pub(super) async fn roots(host: &Host) -> Result<Vec<PathBuf>> {
    let settings = host
        .state
        .implementation
        .registry
        .lock()
        .await
        .settings
        .clone();
    let config = host.state.settings.config()?;
    if !config.server.webhook_prefix.is_empty() {
        return Ok(vec![config.server.data_dir.join("repositories")]);
    }
    Ok(settings.roots)
}
fn confined(path: &Path, roots: &[PathBuf]) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "Choose an absolute directory path.");
    let canonical = path.canonicalize()?;
    ensure!(
        canonical.is_dir()
            && roots
                .iter()
                .filter_map(|r| r.canonicalize().ok())
                .any(|r| canonical.starts_with(r)),
        "Directory is outside this machine's allowed roots."
    );
    Ok(canonical)
}
pub(super) async fn directories(host: &Host, selected: &[PathBuf]) -> Result<Vec<PathBuf>> {
    ensure!(
        (1..=16).contains(&selected.len()),
        "Select between one and sixteen directories."
    );
    let allowed = roots(host).await?;
    let profile = host.state.settings.profile().canonicalize()?;
    let mut result = Vec::new();
    for path in selected {
        let path = confined(path, &allowed)?;
        let repositories = host
            .state
            .settings
            .config()?
            .server
            .data_dir
            .join("repositories")
            .canonicalize()
            .ok();
        ensure!(
            !profile.starts_with(&path)
                && (!path.starts_with(&profile)
                    || repositories
                        .as_ref()
                        .is_some_and(|root| path.starts_with(root))),
            "The assistant's profile cannot be a working directory."
        );
        if !result.contains(&path) {
            result.push(path);
        }
    }
    Ok(result)
}
async fn detail(host: &Host, selection: &Selection) -> Result<Detail> {
    ensure!(
        ["azure", "github"].contains(&selection.provider.as_str()),
        "Invalid work item provider."
    );
    let request = DetailRequest {
        id: selection.id,
        project: Some(selection.project.clone()),
        source: selection.source.clone(),
    };
    let method = if selection.provider == "github" {
        "get_github_issue"
    } else if selection.task {
        "get_azure_task"
    } else {
        "get_azure_hu"
    };
    let value = tools::todo::read(
        &host.state,
        method,
        &json!({"id":request.id,"project":request.project,"source":request.source}),
    )
    .await
    .map_err(anyhow::Error::msg)?;
    Ok(serde_json::from_value(value)?)
}
fn planning(selection: &Selection, detail: &Detail) -> bool {
    if selection.provider == "github" {
        detail.item.kind.eq_ignore_ascii_case("Epic")
    } else {
        !selection.task
    }
}

pub(super) fn handles(method: &str) -> bool {
    method.starts_with("implementation_") || method.starts_with("machine_")
}

/// An execution machine of the account as a workflow names it.
pub(super) struct MachineFacts {
    /// The coordinator (this installation) itself.
    pub own: bool,
    pub online: bool,
    /// The operating system it reported: how it spells directories.
    pub os: String,
    /// Its allowed roots, as it last reported them (this installation's own, read now).
    pub roots: Vec<PathBuf>,
    /// It answers `checkout_inspect`.
    pub inspect: bool,
    /// It runs the workflow director (`workflow_direct`).
    pub direct: bool,
}
/// The machine `id` of this account, or none when it is unknown, revoked or another account's.
pub(super) async fn machine_facts(host: &Arc<Host>, id: &str) -> Result<Option<MachineFacts>> {
    let version = management::version(host)?;
    let own_roots = paths::plain_all(&roots(host).await?);
    let data = host.state.implementation.registry.lock().await;
    if data.id == id {
        return Ok(Some(MachineFacts {
            own: true,
            online: true,
            os: std::env::consts::OS.into(),
            roots: own_roots,
            inspect: true,
            direct: true,
        }));
    }
    Ok(data
        .machines
        .iter()
        .find(|m| m.id == id && !m.revoked && m.account_version == version)
        .map(|m| MachineFacts {
            own: false,
            online: m.online(),
            os: m.os.clone(),
            roots: m.roots.clone(),
            inspect: m.checkout_inspect,
            direct: m.workflow_direct,
        }))
}
/// The one-shot calls a workflow save makes on its machine: its installed CLIs and a checkout.
pub(super) async fn machine_call(
    host: &Arc<Host>,
    machine: &str,
    method: &str,
    args: Value,
) -> Result<Value> {
    ensure!(
        matches!(method, "implementation_catalog" | "checkout_inspect"),
        "Unavailable execution operation."
    );
    fleet::call(host, machine, method, args).await
}
/// This installation's machine ID, and its allowed roots set as a test needs them.
#[cfg(test)]
pub(super) async fn test_machine(host: &Host, roots: Option<Vec<PathBuf>>) -> String {
    let mut data = host.state.implementation.registry.lock().await;
    if let Some(roots) = roots {
        data.settings.roots = roots;
    }
    data.id.clone()
}
pub(super) async fn operate(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> std::result::Result<Value, String> {
    // The Implement form's workflow choice: a workflow ID or the director starts or classifies
    // through the workflow engine; `none` is the plain run below.
    if method == "implementation_start" && workflows::implement::choice(args).is_some() {
        let mut request = control::Request::new(method);
        request.args = args.clone();
        authority::authorize(&host.state, &request).map_err(|e| format!("[invalid_input] {e}"))?;
        return workflows::implement::start(host, args, &workflows::engine::Azure::Live).await;
    }
    let plain = if method == "implementation_start" {
        workflows::implement::plain(args)
    } else {
        args.clone()
    };
    let args = &plain;
    operation(host, method, args).await.map_err(|error| {
        if error.downcast_ref::<reqwest::Error>().is_some() { return "[network] The execution machine or provider could not be reached. Inspect its runs before trying again.".into(); }
        let redactor=security::Redactor::new(&[],Vec::new()).expect("built-in redaction patterns");
        let text = redactor.redact(&error.to_string());
        // A stale or conflicting account change keeps its class so the caller can refresh.
        if text.starts_with("[state_conflict] ") { return text; }
        format!("[invalid_input] {text}")
    })
}
async fn operation(host: &Arc<Host>, method: &str, args: &Value) -> Result<Value> {
    let mut request = control::Request::new(method);
    request.args = args.clone();
    authority::authorize(&host.state, &request)?;
    match method {
        "implementation_request_catalog"
        | "implementation_request_start"
        | "implementation_request_status"
        | "implementation_history" => requests::operate(host, method, args).await,
        "implementation_sessions" => fleet::session_index(host, args).await,
        "implementation_mobile_session" => {
            ensure!(
                args["confirmed"] == true,
                "Confirm connecting this agent session to your mobile app."
            );
            let machine = args["machine"].as_str().context("Choose a machine.")?;
            uuid(machine)?;
            fleet::call(host, machine, method, args.clone()).await
        }
        "implementation_open_session" => {
            let id = args["id"].as_str().context("Choose a run.")?;
            uuid(id)?;
            let own = host.state.implementation.registry.lock().await.id.clone();
            ensure!(
                args["machine"] == own,
                "This session belongs to another machine. Open the assistant on that machine."
            );
            let run = host.state.implementation.db.get(id)?;
            let Some(run) = run else {
                return requests::open_session(host, id).await;
            };
            let session = run
                .session_id
                .context("The agent has not reported a session ID yet.")?;
            ensure!(
                host.shell.browser(),
                "This host has no desktop. Open the session from a desktop device with access to this machine."
            );
            match run.launch.request.provider.as_str() {
                "codex" => {
                    uuid(&session)?;
                    let url = format!("codex://threads/{session}");
                    host.shell.open_url(&url).map_err(|_|anyhow::anyhow!("The system could not open the Codex app. Check that it is installed and can access this local session."))?;
                }
                "claude" => {
                    uuid(&session)?;
                    ensure!(
                        !["running", "planning"].contains(&run.state.as_str()),
                        "Claude Desktop can open this local session after the CLI finishes. This session is still running."
                    );
                    crate::llm::implementation::open_claude_session(
                        &session,
                        &run.launch.request.directories[0],
                    )
                    .await?;
                }
                _ => anyhow::bail!(
                    "This provider does not publish a verified desktop link for this local CLI session."
                ),
            }
            Ok(json!({"opened":true}))
        }
        "machine_browse" => browse_machine(host, args),
        "machine_register" => register(host, args).await,
        "machine_join_command" => join::local(host).await,
        "machine_login" => login::begin(host, args).await,
        "machine_login_status" => login::status(host).await,
        "machine_login_finish" => login::finish(host).await,
        "machine_login_cancel" => login::cancel(host).await,
        "machine_list" | "machine_settings" | "machine_connect" | "machine_disconnect"
        | "machine_teams" | "machine_save" | "machine_protocol" => {
            fleet::operate(host, method, args).await
        }
        "machine_detail" | "machine_update" | "machine_revoke" | "machine_directories" => {
            management::operate(host, method, args).await
        }
        "implementation_prepare" => {
            let selection: Selection = serde_json::from_value(args["item"].clone())?;
            let detail = detail(host, &selection).await?;
            let data = host.state.implementation.registry.lock().await;
            let creations: BTreeMap<_, _> = data
                .creations
                .iter()
                .filter(|(_, value)| value["parent"] == detail.item.url)
                .map(|(id, record)| (id, creation_summary(record)))
                .collect();
            Ok(
                json!({"planning":planning(&selection,&detail),"detail":detail,"creations":creations}),
            )
        }
        "machine_assistant"
        | "implementation_catalog"
        | "implementation_browse"
        | "implementation_runs"
        | "implementation_cancel" => {
            let machine = args["machine"].as_str().context("Choose a machine.")?;
            fleet::call(host, machine, method, args.clone()).await
        }
        "machine_upgrade" => {
            let machine = args["machine"].as_str().context("Choose a machine.")?;
            ensure!(
                args.as_object().is_some_and(|a| a.len() == 1),
                "Updating a machine takes only the machine: the version is always the newest release."
            );
            fleet::call(host, machine, method, json!({})).await
        }
        "implementation_start" => {
            let request: Start = serde_json::from_value(args.clone())?;
            uuid(&request.machine)?;
            ensure!(
                request.context.len() <= 32_000,
                "Implementation context is too long."
            );
            let detail = detail(host, &request.item).await?;
            let planning = planning(&request.item, &detail);
            let launch = build_launch(
                host,
                uuid::Uuid::new_v4().to_string(),
                request,
                detail,
                planning,
            )?;
            fleet::call(
                host,
                &launch.request.machine,
                "implementation_launch",
                serde_json::to_value(&launch)?,
            )
            .await
        }
        "implementation_create" | "implementation_creation" => {
            if let Some(result) = fleet::coordinator(host, method, args).await? {
                return Ok(result);
            }
            if method == "implementation_create" {
                create(host, args).await
            } else {
                let id = args["id"].as_str().context("Choose a plan.")?;
                uuid(id)?;
                let data = host.state.implementation.registry.lock().await;
                Ok(data
                    .creations
                    .get(id)
                    .map(creation_summary)
                    .unwrap_or(Value::Null))
            }
        }
        // Only local IPC and the authenticated worker protocol can reach these internal methods.
        "implementation_local" => {
            local(host, args["method"].as_str().unwrap_or(""), &args["args"]).await
        }
        _ => anyhow::bail!("Unknown implementation operation."),
    }
}

/// The launch of an implementation run: the user's instructions for its kind, the work item
/// guides that apply (the item's own type, or every type of its project when planning) and the
/// organization's entries for its project.
fn build_launch(
    host: &Arc<Host>,
    id: String,
    request: Start,
    detail: Detail,
    planning: bool,
) -> Result<Launch> {
    let map = host.state.settings.map()?;
    let organization = LocalOrganization::new(&host.state.settings).organization()?;
    let (provider, project) = (&request.item.provider, &detail.item.project);
    let guides = if planning {
        crate::work_items::for_project(&organization.guides, provider, project)
    } else {
        crate::work_items::for_item(&organization.guides, provider, &detail.item.kind, project)
            .into_iter()
            .collect()
    };
    Ok(Launch {
        id,
        planning,
        instructions: Some(crate::agent_prompts::resolve(
            &map.agent_prompts,
            if planning {
                crate::agent_prompts::PLAN
            } else {
                crate::agent_prompts::CODE
            },
        )),
        guides: guides.into_iter().cloned().collect(),
        organization: crate::organization::for_agents(&organization.organization, project)
            .into_iter()
            .cloned()
            .collect(),
        request,
        detail,
        language: host.state.settings.config()?.llm.language,
        allow: Vec::new(),
    })
}

/// An implementation run the workflow engine asks for: an ID it chose and recorded before
/// asking, an Azure item it read, its machine, agent and directories, and its context.
pub(super) struct Planned {
    pub id: String,
    pub machine: String,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub directories: Vec<PathBuf>,
    pub context: String,
    pub source: Option<String>,
    pub detail: Detail,
    pub planning: bool,
    /// What the workflow lifts from the coding rules; ignored for a planning run.
    pub allow: Vec<crate::workflows::Allow>,
}
/// Launch a run the engine planned, through the same `implementation_launch` call as
/// `implementation_start`. The call is idempotent by run ID: asking again for a run the machine
/// already holds returns it (`existing`) and starts nothing.
pub(super) async fn launch_planned(host: &Arc<Host>, planned: Planned) -> Result<Value> {
    uuid(&planned.id)?;
    uuid(&planned.machine)?;
    ensure!(
        planned.context.len() <= 32_000,
        "Implementation context is too long."
    );
    let request = Start {
        machine: planned.machine,
        item: Selection {
            provider: "azure".into(),
            id: planned.detail.item.id,
            project: planned.detail.item.project.clone(),
            source: planned.source,
            task: !planned.planning,
        },
        context: planned.context,
        directories: planned.directories,
        provider: planned.provider,
        model: planned.model,
        effort: planned.effort,
    };
    let mut launch = build_launch(host, planned.id, request, planned.detail, planned.planning)?;
    if !planned.planning {
        launch.allow = planned.allow;
    }
    fleet::call(
        host,
        &launch.request.machine,
        "implementation_launch",
        serde_json::to_value(&launch)?,
    )
    .await
}
/// One implementation run as its machine records it (`implementation_runs`), or none when the
/// machine holds no run with that ID.
pub(super) async fn observe(host: &Arc<Host>, machine: &str, id: &str) -> Result<Option<Value>> {
    uuid(id)?;
    let runs = fleet::call(
        host,
        machine,
        "implementation_runs",
        json!({"machine":machine,"id":id}),
    )
    .await?;
    Ok(runs["runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|run| run["id"] == id))
        .cloned())
}
/// Cancel an implementation run on its machine (`implementation_cancel`).
pub(super) async fn cancel_run(host: &Arc<Host>, machine: &str, id: &str) -> Result<()> {
    uuid(id)?;
    fleet::call(
        host,
        machine,
        "implementation_cancel",
        json!({"machine":machine,"id":id}),
    )
    .await
    .map(|_| ())
}
/// The creation ledger's record of a plan (`implementation_create`), as it is now: its state,
/// parent, reviewed children and the items created.
pub(super) async fn creation_record(host: &Arc<Host>, plan: &str) -> Option<Value> {
    host.state
        .implementation
        .registry
        .lock()
        .await
        .creations
        .get(plan)
        .cloned()
}
/// A creation recorded as `implementation_create` records it (tests of what observes it).
#[cfg(all(test, unix))]
pub(super) async fn test_creation(host: &Host, plan: &str, record: Value) {
    let state = &host.state.implementation;
    let mut data = state.registry.lock().await;
    data.creations.insert(plan.into(), record);
    state.save(&data).unwrap();
}
/// End an in-flight run in `state` as its machine would record it (tests).
#[cfg(all(test, unix))]
pub(super) fn test_finish(host: &Host, id: &str, state: &str, output: &str) {
    LocalImplementation::new(&host.state.implementation.db)
        .finish_run(id, state, output, None)
        .unwrap();
}

fn browse_machine(host: &Host, args: &Value) -> Result<Value> {
    let config = host.state.settings.config()?;
    let allowed = if !config.server.webhook_prefix.is_empty() {
        vec![config.server.data_dir.join("repositories")]
    } else {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .context("Home directory unavailable.")?;
        let roots = paths::filesystem_roots(&home);
        ensure!(!roots.is_empty(), "Directory root unavailable.");
        roots
    };
    browse_directories(args["path"].as_str(), &allowed)
}

fn browse_directories(path: Option<&str>, allowed: &[PathBuf]) -> Result<Value> {
    if path.is_none_or(str::is_empty) {
        return Ok(
            json!({"path":null,"parent":null,"directories":paths::plain_all(allowed),"roots":true}),
        );
    }
    let path = confined(Path::new(path.unwrap()), allowed)?;
    let mut entries: Vec<PathBuf> = fs::read_dir(&path)?
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| !n.to_string_lossy().starts_with('.'))
                && confined(p, allowed).is_ok()
        })
        .take(1000)
        .map(|p| paths::plain(&p))
        .collect();
    entries.sort();
    let parent = path
        .parent()
        .and_then(|p| confined(p, allowed).ok())
        .map(|p| paths::plain(&p));
    Ok(json!({"path":paths::plain(&path),"parent":parent,"directories":entries,"roots":false}))
}

async fn local(host: &Arc<Host>, method: &str, args: &Value) -> Result<Value> {
    if !matches!(
        method,
        "machine_assistant"
            | "machine_upgrade"
            | "implementation_runs"
            | "machine_roots"
            | "machine_browse"
            | "checkout_inspect"
    ) {
        authority::require_execution(&host.state)?;
    } else {
        ensure!(
            authority::invocation(&host.state)?.scope.admitted,
            "Account binding changed."
        );
    }
    authority::reject_authority_arguments(args)?;
    let state = &host.state.implementation;
    match method {
        "machine_browse" => browse_machine(host, args),
        "checkout_inspect" => {
            let allowed = roots(host).await?;
            checkout::inspect_all(&args["paths"], &allowed)
        }
        // Boxed: the session is a large future, kept off the stack of every machine call.
        "workflow_direct" => Box::pin(director::answer(host, args)).await,
        "machine_roots" => management::save_roots(host, args).await,
        "machine_upgrade" => Box::pin(crate::app::maintenance::remote_latest(host)).await,
        "implementation_mobile_session" => mobile_session(host, args).await,
        "machine_assistant" => {
            let method = match args["action"].as_str() {
                Some("start") => "start_assistant",
                Some("stop") => "stop_assistant",
                _ => anyhow::bail!("Choose start or stop for the assistant."),
            };
            let reply = Box::pin(control::dispatch(host, control::Request::new(method))).await;
            ensure!(
                reply.ok,
                "{}",
                reply
                    .message
                    .as_deref()
                    .unwrap_or("Assistant operation failed.")
            );
            Ok(json!({"assistant_phase":*host.state.phase.borrow()}))
        }

        "implementation_catalog" => {
            let mut catalog = crate::llm::catalog().await;
            catalog
                .providers
                .retain(|p| p.transport == "cli" && p.available);
            Ok(serde_json::to_value(catalog)?)
        }
        "implementation_browse" => {
            let allowed = roots(host).await?;
            browse_directories(args["path"].as_str(), &allowed)
        }
        "implementation_runs" => {
            let mut runs = Vec::new();
            let mut size = 0;
            let mut partial = false;
            for run in state
                .db
                .list(args["work_item_url"].as_str(), args["id"].as_str(), false)?
            {
                let value = json!({"id":run.launch.id,"item":run.launch.detail.item,"planning":run.launch.planning,"state":run.state,"created_at":run.created_at,"started_at":run.started_at,"finished_at":run.finished_at,"session_id":run.session_id,"remote":run.remote,"provider":run.launch.request.provider,"model":run.launch.request.model,"effort":run.launch.request.effort,"machine":run.launch.request.machine,"output":run.output,"plan":run.plan,"permission_mode":run.permission_mode,"prompts":run.prompts});
                size += serde_json::to_vec(&value)?.len();
                if size > 800_000 && !runs.is_empty() {
                    partial = true;
                    break;
                }
                runs.push(value);
            }
            Ok(json!({"runs":runs,"partial":partial}))
        }
        "implementation_cancel" => {
            let id = args["id"].as_str().context("Choose a run.")?;
            uuid(id)?;
            let run = state.db.get(id)?;
            let task = state.tasks.lock().await.remove(id);
            if let Some(task) = task {
                task.abort();
                let _ = task.await;
            }
            if let Some(run) = run.filter(|run| {
                ["running", "planning"].contains(&run.state.as_str())
                    && run.launch.request.provider == "codex"
                    && run.remote.as_ref().is_some_and(|a| a.status == "connected")
            }) && let Some(session) = run.session_id
            {
                // Keep the run active, and its directories reserved, until the daemon confirms stop.
                crate::llm::implementation::mobile::cancel_codex(&session).await?;
            }
            LocalImplementation::new(&state.db).finish_run(id, "cancelled", "", None)?;
            Ok(json!({"cancelled":true}))
        }
        "implementation_launch" => {
            let mut launch: Launch = serde_json::from_value(args.clone())?;
            uuid(&launch.id)?;
            let own = state.registry.lock().await.id.clone();
            ensure!(
                launch.request.machine == own,
                "Run targets another machine."
            );
            if state.db.get(&launch.id)?.is_some() {
                return Ok(json!({"id":launch.id,"existing":true}));
            }
            launch.request.directories = directories(host, &launch.request.directories).await?;
            crate::llm::implementation::validate(
                &launch.request.provider,
                &launch.request.model,
                &launch.request.effort,
            )
            .await?;
            let data = state.registry.lock().await;
            if state.db.get(&launch.id)?.is_some() {
                return Ok(json!({"id":launch.id,"existing":true}));
            }
            ensure!(
                !state
                    .db
                    .list(None, None, true)?
                    .iter()
                    .any(|r| ["running", "planning"].contains(&r.state.as_str())
                        && r.launch.request.directories.iter().any(|p| launch
                            .request
                            .directories
                            .iter()
                            .any(|q| p.starts_with(q) || q.starts_with(p)))),
                "Another run is using these directories."
            );
            let id = launch.id.clone();
            state.db.insert(&Run {
                state: if launch.planning {
                    "planning"
                } else {
                    "running"
                }
                .into(),
                launch: launch.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                finished_at: None,
                output: String::new(),
                plan: None,
                session_id: None,
                started_at: None,
                remote: None,
                permission_mode: Some(
                    crate::llm::implementation::permission_mode(
                        &launch.request.provider,
                        launch.planning,
                    )
                    .into(),
                ),
                prompts: Vec::new(),
            })?;
            let host = host.clone();
            let task = tokio::spawn(async move {
                run(&host, launch).await;
            });
            state.tasks.lock().await.insert(id.clone(), task);
            drop(data);
            Ok(json!({"id":id,"started":true}))
        }
        _ => anyhow::bail!("Unavailable machine operation."),
    }
}
fn remote_observer(host: &Arc<Host>, id: &str) -> crate::llm::implementation::mobile::Observer {
    let host = host.clone();
    let id = id.to_owned();
    Arc::new(move |access| {
        authority::require_execution(&host.state)?;
        host.state.implementation.db.remote(&id, access)
    })
}

async fn mobile_session(host: &Arc<Host>, args: &Value) -> Result<Value> {
    use crate::llm::implementation::mobile;
    ensure!(
        args["confirmed"] == true,
        "Confirm connecting this agent session to your mobile app."
    );
    let id = args["id"].as_str().context("Choose a recorded run.")?;
    uuid(id)?;
    let db = &host.state.implementation.db;
    let (provider, model, effort, session, working, planning, active, reported) =
        if let Some(run) = db.get(id)? {
            let request = run.launch.request;
            ensure!(
                request.machine == host.state.implementation.registry.lock().await.id,
                "This session belongs to another execution machine."
            );
            (
                request.provider,
                request.model,
                request.effort,
                run.session_id,
                directories(host, &request.directories).await?,
                run.launch.planning,
                ["running", "planning"].contains(&run.state.as_str()),
                run.remote,
            )
        } else {
            let record = db
                .requests(Some(id))?
                .pop()
                .context("Session record unavailable.")?;
            (
                record["provider"].as_str().unwrap_or_default().to_owned(),
                record["model"].as_str().unwrap_or_default().to_owned(),
                record["effort"].as_str().unwrap_or_default().to_owned(),
                record["session_id"].as_str().map(str::to_owned),
                vec![host.state.implementation.root.join("requests").join(id)],
                true,
                record["state"] == "generating",
                serde_json::from_value::<mobile::Access>(record["remote"].clone()).ok(),
            )
        };
    let session = session.context("The agent has not reported a session ID yet.")?;
    uuid(&session)?;
    let observer = remote_observer(host, id);
    match args["action"].as_str().unwrap_or("connect") {
        "close" => {
            ensure!(
                !active,
                "Let the current run finish before disconnecting its mobile session."
            );
            ensure!(
                provider == "claude",
                "Codex Remote Control is shared by this machine. Manage it in Codex."
            );
            mobile::close(&session).await;
            let access = mobile::Access {
                status: "offline".into(),
                ..mobile::Access::default()
            };
            observer(access.clone())?;
            Ok(json!({"remote":access}))
        }
        action @ ("connect" | "pair") => {
            if active
                && reported.as_ref().is_some_and(|r| {
                    r.status == "connected"
                        && (provider == "codex" || r.expires_at.is_some_and(|t| t > now()))
                })
            {
                let pairing = if action == "pair" {
                    ensure!(
                        provider == "codex",
                        "Pairing codes are available only for Codex."
                    );
                    Some(mobile::pair_codex().await?)
                } else {
                    None
                };
                return Ok(json!({"remote":reported,"pairing":pairing}));
            }
            ensure!(
                !active,
                "Remote Control did not connect when this run started. Let the current run finish before reconnecting it."
            );
            let access = mobile::reconnect(
                (&provider, &model, &effort),
                &session,
                reported.as_ref().and_then(|r| r.session_id.as_deref()),
                &working,
                planning,
                observer.clone(),
            )
            .await?;
            observer(access.clone())?;
            let pairing = if action == "pair" {
                ensure!(
                    provider == "codex",
                    "Pairing codes are available only for Codex."
                );
                Some(mobile::pair_codex().await?)
            } else {
                None
            };
            Ok(json!({"remote":access,"pairing":pairing}))
        }
        _ => anyhow::bail!("Choose connect, pair or close for this mobile session."),
    }
}

async fn run(host: &Arc<Host>, launch: Launch) {
    if authority::require_execution(&host.state).is_err() {
        return;
    }
    let request = &launch.request;
    let mut input = json!({"verified_item":launch.detail,"user_context":request.context,"working_directories":request.directories,"language":launch.language,"today":chrono::Local::now().date_naive()});
    if !launch.guides.is_empty() {
        input["work_item_guides"] = json!(launch.guides);
    }
    if !launch.organization.is_empty() {
        input["organization"] = crate::organization::agent_input(&launch.organization);
    }
    let input = input.to_string();
    let instructions = launch.instructions.clone().unwrap_or_else(|| {
        crate::agent_prompts::resolve(
            &[],
            if launch.planning {
                crate::agent_prompts::PLAN
            } else {
                crate::agent_prompts::CODE
            },
        )
    });
    let observer = |session: Option<&str>| {
        authority::require_execution(&host.state)?;
        host.state.implementation.db.session(&launch.id, session)
    };
    let remote = remote_observer(host, &launch.id);
    let result = if launch.planning {
        async {
            let plan = crate::llm::implementation::plan(
                (&request.provider, &request.model, &request.effort),
                &request.directories,
                &instructions,
                &input,
                &observer,
                remote.clone(),
            )
            .await?;
            let children = plan["children"].as_array().context("Invalid child plan.")?;
            ensure!(
                (1..=20).contains(&children.len()),
                "Invalid child plan size."
            );
            for child in children {
                validate_child(child)?;
            }
            Ok((true, String::new(), Some(plan), Vec::new()))
        }
        .await
    } else {
        let prompt =
            crate::llm::implementation::coding_prompt(&instructions, &input, &launch.allow);
        crate::llm::implementation::execute(
            &request.provider,
            &request.model,
            &request.effort,
            &request.directories,
            &prompt,
            &launch.allow,
            &observer,
            remote,
        )
        .await
        .map(|outcome| (outcome.ok, outcome.text, None, outcome.prompts))
    };
    let (status, output, plan, prompts) = match result {
        Ok((ok, output, plan, prompts)) => (final_state(ok, plan.is_some(), &prompts), output, plan, prompts),
        Err(_) => ("failed", "The selected CLI did not complete. Check its login, permissions and model availability on the execution machine.".into(), None, Vec::new()),
    };
    if authority::invocation(&host.state).is_ok_and(|i| i.scope.owner) {
        let db = &host.state.implementation.db;
        if !prompts.is_empty() {
            let _ = db.prompts(&launch.id, &prompts);
        }
        let _ = LocalImplementation::new(db).finish_run(&launch.id, status, &output, plan);
    }
}
/// How a finished run ends. A run is fully autonomous, so a permission prompt the CLI still
/// raised is its own state with the prompt recorded, never `needs_attention`.
fn final_state(ok: bool, plan: bool, prompts: &[String]) -> &'static str {
    if plan {
        "plan_ready"
    } else if !prompts.is_empty() {
        PERMISSION_PROMPT
    } else if ok {
        "done"
    } else {
        "needs_attention"
    }
}

async fn create(host: &Arc<Host>, args: &Value) -> Result<Value> {
    let machine = args["machine"].as_str().context("Choose a machine.")?;
    let id = args["id"].as_str().context("Choose a plan.")?;
    uuid(id)?;
    ensure!(
        args["confirmed"] == true,
        "Review and confirm the child tasks before creating them."
    );
    if let Some(record) = host
        .state
        .implementation
        .registry
        .lock()
        .await
        .creations
        .get(id)
    {
        return Ok(creation_summary(record));
    }
    let runs = fleet::call(host, machine, "implementation_runs", json!({"id":id})).await?;
    let run = runs["runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|run| run["id"] == id))
        .context("Plan unavailable.")?;
    ensure!(
        run["planning"] == true && run["state"] == "plan_ready",
        "This run is not an approved child-task plan."
    );
    let selection: Selection = serde_json::from_value(args["item"].clone())?;
    let parent = detail(host, &selection).await?;
    ensure!(
        planning(&selection, &parent) && parent.item.url == run["item"]["url"],
        "Plan belongs to another parent."
    );
    let children = args["children"].as_array().context("Choose child tasks.")?;
    ensure!(
        (1..=20).contains(&children.len()),
        "Choose between one and twenty child tasks."
    );
    for child in children {
        validate_child(child)?;
    }
    if selection.provider == "azure" {
        super::azure_cli::require().await?;
    }
    // Every type and field value is checked against the live definition before the first write.
    for child in children {
        tools::todo::check_child(
            &host.state,
            &selection.provider,
            selection.source.as_deref(),
            &parent,
            child,
        )
        .await?;
    }
    let state = &host.state.implementation;
    {
        let mut data = state.registry.lock().await;
        ensure!(
            !data.creations.contains_key(id),
            "This plan was already submitted. Review its recorded outcome before doing anything else."
        );
        data.creations.insert(
            id.into(),
            json!({"state":"sending","parent":parent.item.url,"children":children,"created":[]}),
        );
        state.save(&data)?;
    }
    let result = {
        let data = state.registry.lock().await;
        creation_summary(&data.creations[id])
    };
    let task_host = host.clone();
    let task_id = id.to_owned();
    let children = children.clone();
    let task = tokio::spawn(async move {
        if send_children(&task_host, &task_id, &selection, &parent, children)
            .await
            .is_err()
        {
            if !authority::invocation(&task_host.state).is_ok_and(|i| i.scope.owner) {
                return;
            }
            let state = &task_host.state.implementation;
            let mut data = state.registry.lock().await;
            if let Some(record) = data.creations.get_mut(&task_id) {
                record["state"] = json!("uncertain");
                let _ = state.save(&data);
            }
        }
    });
    state
        .tasks
        .lock()
        .await
        .insert(format!("create:{id}"), task);
    Ok(result)
}
fn creation_summary(record: &Value) -> Value {
    json!({"state":record["state"],"parent":record["parent"],"created":record["created"]})
}
async fn send_children(
    host: &Arc<Host>,
    id: &str,
    selection: &Selection,
    parent: &Detail,
    children: Vec<Value>,
) -> Result<()> {
    let state = &host.state.implementation;
    let mut created = Vec::new();
    for child in &children {
        ensure!(
            authority::invocation(&host.state)?.scope.owner,
            "Account binding changed."
        );
        let result = tools::todo::create_child(
            &host.state,
            &selection.provider,
            selection.source.as_deref(),
            parent,
            child,
        )
        .await;
        ensure!(
            authority::invocation(&host.state)?.scope.owner,
            "Account binding changed."
        );
        let mut data = state.registry.lock().await;
        let record = data.creations.get_mut(id).unwrap();
        match result {
            Ok(item) => {
                let linked = item["state"] == "created";
                created.push(item);
                record["created"] = json!(created);
                if !linked {
                    record["state"] = json!("needs_attention");
                    state.save(&data)?;
                    return Ok(());
                }
            }
            Err(_) => {
                record["state"] = json!("uncertain");
                state.save(&data)?;
                return Ok(());
            }
        }
        state.save(&data)?;
    }
    let mut data = state.registry.lock().await;
    data.creations.get_mut(id).unwrap()["state"] = json!("created");
    state.save(&data)?;
    Ok(())
}
fn validate_child(child: &Value) -> Result<()> {
    let title = child["title"].as_str().unwrap_or_default();
    let description = child["description"].as_str().unwrap_or_default();
    ensure!(
        !title.trim().is_empty()
            && title.len() <= 200
            && !title.chars().any(char::is_control)
            && !description.trim().is_empty()
            && description.len() <= 16_000,
        "Each child needs a title and description within the allowed length."
    );
    let kind = &child["type"];
    ensure!(
        kind.is_null()
            || kind
                .as_str()
                .is_some_and(|k| k.len() <= 120 && !k.chars().any(char::is_control)),
        "A child's type is the name of a work item type."
    );
    let fields = &child["fields"];
    ensure!(
        fields.is_null()
            || fields.as_array().is_some_and(|fields| {
                fields.len() <= 40
                    && fields.iter().all(|f| {
                        f["reference"]
                            .as_str()
                            .is_some_and(crate::work_items::valid_reference)
                            && f["value"].as_str().is_some_and(|v| v.len() <= 16_000)
                    })
            }),
        "A child's fields are at most 40 reference and value pairs."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_permission_prompt_never_ends_a_run_in_needs_attention() {
        let prompt = vec!["Bash: git push".to_owned()];
        assert_eq!(final_state(false, false, &prompt), PERMISSION_PROMPT);
        assert_eq!(final_state(true, false, &prompt), PERMISSION_PROMPT);
        assert_eq!(final_state(true, false, &[]), "done");
        assert_eq!(final_state(false, false, &[]), "needs_attention");
        assert_eq!(final_state(true, true, &[]), "plan_ready");
    }
    #[test]
    fn paths_are_canonical_and_cannot_escape_allowed_roots() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        assert!(confined(allowed.path(), &[allowed.path().into()]).is_ok());
        assert!(confined(outside.path(), &[allowed.path().into()]).is_err());
        assert!(confined(Path::new("relative"), &[allowed.path().into()]).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), allowed.path().join("escape")).unwrap();
            assert!(confined(&allowed.path().join("escape"), &[allowed.path().into()]).is_err());
        }
    }
    #[test]
    fn child_validation_requires_a_bounded_title_and_description() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path()).unwrap();
        let registry = state.registry.try_lock().unwrap();
        assert_eq!(registry.id, registry.teams_machine);
        assert!(
            validate_child(&json!({"title":"Fix","description":"Verify and fix the parser."}))
                .is_ok()
        );
        assert!(validate_child(&json!({"title":"","description":"missing title"})).is_err());
        assert!(!handles("start_assistant"));
    }
    struct TestShell;
    impl Shell for TestShell {
        fn open_url(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn exit(&self) {}
    }
    pub(super) fn host(root: &Path) -> Arc<Host> {
        Arc::new(Host {
            state: init_state(&root.join("profile"), &root.join("data")).unwrap(),
            shell: Box::new(TestShell),
        })
    }
    #[tokio::test]
    async fn assistant_machine_controls_share_local_guards_and_authenticated_one_shot_delivery() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let own = host.state.implementation.registry.lock().await.id.clone();
        let stopped = operation(
            &host,
            "machine_assistant",
            &json!({"machine":own,"action":"stop"}),
        )
        .await
        .unwrap();
        assert_eq!(stopped["assistant_phase"], "stopped");
        assert!(
            local(&host, "machine_assistant", &json!({"action":"restart"}))
                .await
                .is_err()
        );

        let worker = uuid::Uuid::new_v4().to_string();
        let registration =
            fleet::register(&host, &json!({"id":worker,"name":"Worker","os":"linux"}))
                .await
                .unwrap();
        let token = registration["token"].as_str().unwrap();
        host.state
            .implementation
            .registry
            .lock()
            .await
            .teams_machine = worker.clone();
        // The ordinary start guard rejects another Teams owner before credentials or Graph are read.
        assert!(
            operation(
                &host,
                "machine_assistant",
                &json!({"machine":own,"action":"start"})
            )
            .await
            .is_err()
        );
        assert_eq!(*host.state.phase.borrow(), AssistantPhase::Stopped);

        fleet::protocol(
            &host,
            &json!({"id":worker,"token":token,"action":"poll","assistant_phase":"running"}),
        )
        .await
        .unwrap();
        let caller = host.clone();
        let target = worker.clone();
        let call = tokio::spawn(async move {
            operation(
                &caller,
                "machine_assistant",
                &json!({"machine":target,"action":"stop"}),
            )
            .await
        });
        let poll = json!({"id":worker,"token":token,"action":"poll","assistant_phase":"running"});
        let command = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let reply = fleet::protocol(&host, &poll).await.unwrap();
                if !reply["command"].is_null() {
                    break reply["command"].clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(command["method"], "machine_assistant");
        assert_eq!(command["args"]["action"], "stop");
        assert!(fleet::protocol(&host, &poll).await.unwrap()["command"].is_null());
        let list = operation(&host, "machine_list", &json!({})).await.unwrap();
        assert_eq!(list["machines"][1]["assistant_phase"], "running");
        assert!(
            fleet::protocol(
                &host,
                &json!({"id":worker,"token":"invalid","action":"poll","assistant_phase":"stopped"})
            )
            .await
            .is_err()
        );
        fleet::protocol(&host, &json!({"id":worker,"token":token,"action":"poll","assistant_phase":"stopped","results":[{"id":command["id"],"result":{"ok":true,"data":{"assistant_phase":"stopped"}}}]})).await.unwrap();
        assert_eq!(call.await.unwrap().unwrap()["assistant_phase"], "stopped");
        let list = operation(&host, "machine_list", &json!({})).await.unwrap();
        assert_eq!(list["machines"][1]["assistant_phase"], "stopped");
        // A compatible report is required again after a worker downgrade/restart.
        fleet::protocol(&host, &json!({"id":worker,"token":token,"action":"poll"}))
            .await
            .unwrap();
        let unknown = operation(&host, "machine_detail", &json!({"machine":worker}))
            .await
            .unwrap();
        assert_eq!(unknown["presence"], "unknown");
        assert_eq!(unknown["capabilities"]["management"], false);
        assert!(
            operation(
                &host,
                "machine_assistant",
                &json!({"machine":worker,"action":"start"})
            )
            .await
            .is_err()
        );
    }
    #[tokio::test]
    async fn worker_polls_report_version_and_update_state_and_older_workers_read_as_unknown() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let worker = uuid::Uuid::new_v4().to_string();
        let registration =
            fleet::register(&host, &json!({"id":worker,"name":"Worker","os":"macos"}))
                .await
                .unwrap();
        let token = registration["token"].as_str().unwrap();
        let detail = |host: Arc<Host>, worker: String| async move {
            operation(&host, "machine_detail", &json!({"machine":worker}))
                .await
                .unwrap()
        };
        // An older worker sends neither field.
        fleet::protocol(&host, &json!({"id":worker,"token":token,"action":"poll"}))
            .await
            .unwrap();
        let older = detail(host.clone(), worker.clone()).await;
        assert_eq!(older["version"], Value::Null);
        assert_eq!(older["update"], Value::Null);
        // A current worker reports its running version and update state.
        let report = json!({"state":"staged","pending":"0.11.6","failed":null,"unattended":true});
        fleet::protocol(
            &host,
            &json!({"id":worker,"token":token,"action":"poll","host_version":"0.11.5","update":report}),
        )
        .await
        .unwrap();
        let current = detail(host.clone(), worker.clone()).await;
        assert_eq!(current["version"], "0.11.5");
        assert_eq!(current["update"]["state"], "staged");
        assert_eq!(current["update"]["pending"], "0.11.6");
        assert_eq!(current["update"]["unattended"], true);
        let list = operation(&host, "machine_list", &json!({})).await.unwrap();
        let listed = list["machines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == worker.as_str())
            .unwrap();
        assert_eq!(listed["version"], "0.11.5");
        assert_eq!(listed["update"]["state"], "staged");
        // This computer's own row carries its version too.
        assert_eq!(list["machines"][0]["version"], env!("CARGO_PKG_VERSION"));
        // Anything that is not exactly a report (free text, unknown fields, a non-version) is
        // dropped rather than shown.
        for update in [
            json!({"state":"rm -rf /","pending":null}),
            json!({"state":"staged","pending":"latest"}),
            json!({"state":"staged","note":"text from the machine"}),
            json!("staged"),
        ] {
            fleet::protocol(
                &host,
                &json!({"id":worker,"token":token,"action":"poll","host_version":"v-next","update":update}),
            )
            .await
            .unwrap();
            let shown = detail(host.clone(), worker.clone()).await;
            assert_eq!(shown["version"], Value::Null, "{update}");
            assert_eq!(shown["update"], Value::Null, "{update}");
        }
        // Reporting is all a poll does: no update operation is reachable through the protocol.
        for method in [
            "crate_update_apply",
            "crate_update_stage",
            "crate_update_unattended",
        ] {
            assert!(
                fleet::protocol(
                    &host,
                    &json!({"id":worker,"token":token,"action":"call","method":method,"args":{}}),
                )
                .await
                .is_err(),
                "{method}"
            );
        }
    }
    #[tokio::test]
    async fn another_device_asks_a_capable_worker_to_update_and_relays_its_reason() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let worker = uuid::Uuid::new_v4().to_string();
        let registration =
            fleet::register(&host, &json!({"id":worker,"name":"Worker","os":"windows"}))
                .await
                .unwrap();
        let token = registration["token"].as_str().unwrap();
        // An older worker, or one whose installation cannot update itself, is not asked.
        let poll = json!({"id":worker,"token":token,"action":"poll","assistant_phase":"stopped"});
        fleet::protocol(&host, &poll).await.unwrap();
        let listed = operation(&host, "machine_detail", &json!({"machine":worker}))
            .await
            .unwrap();
        assert_eq!(listed["capabilities"]["upgrade"], false);
        let refused = operation(&host, "machine_upgrade", &json!({"machine":worker}))
            .await
            .unwrap_err();
        assert!(refused.to_string().contains("pta update"), "{refused}");
        // The version is never an argument.
        assert!(
            operation(
                &host,
                "machine_upgrade",
                &json!({"machine":worker,"version":"9.9.9"})
            )
            .await
            .is_err()
        );
        // A worker never reaches it as a coordinator operation of its own.
        assert!(
            fleet::protocol(
                &host,
                &json!({"id":worker,"token":token,"action":"call","method":"machine_upgrade","args":{}}),
            )
            .await
            .is_err()
        );

        let poll = json!({"id":worker,"token":token,"action":"poll","assistant_phase":"stopped","upgrade_support":true});
        fleet::protocol(&host, &poll).await.unwrap();
        let listed = operation(&host, "machine_detail", &json!({"machine":worker}))
            .await
            .unwrap();
        assert_eq!(listed["capabilities"]["upgrade"], true);
        let ask = |host: Arc<Host>, worker: String| {
            tokio::spawn(async move {
                operation(&host, "machine_upgrade", &json!({"machine":worker})).await
            })
        };
        let claim = || async {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let reply = fleet::protocol(&host, &poll).await.unwrap();
                    if !reply["command"].is_null() {
                        break reply["command"].clone();
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap()
        };
        let call = ask(host.clone(), worker.clone());
        let command = claim().await;
        assert_eq!(command["method"], "machine_upgrade");
        assert_eq!(command["args"], json!({}));
        fleet::protocol(&host, &json!({"id":worker,"token":token,"action":"poll","upgrade_support":true,"results":[{"id":command["id"],"result":{"ok":true,"data":{"outcome":"staging","target":"0.99.0"}}}]})).await.unwrap();
        assert_eq!(call.await.unwrap().unwrap()["outcome"], "staging");
        // A refusal on the machine comes back with its own reason.
        let call = ask(host.clone(), worker.clone());
        let command = claim().await;
        fleet::protocol(&host, &json!({"id":worker,"token":token,"action":"poll","upgrade_support":true,"results":[{"id":command["id"],"result":{"ok":false,"error":"Keychain: update it at the computer."}}]})).await.unwrap();
        let error = call.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("Keychain"), "{error}");
        // This computer's own row is updated here, and only by its local owner.
        let own = host.state.implementation.registry.lock().await.id.clone();
        let list = operation(&host, "machine_list", &json!({})).await.unwrap();
        assert_eq!(list["machines"][0]["id"], own.as_str());
        assert!(list["machines"][0]["capabilities"]["upgrade"].is_boolean());
    }
    #[tokio::test]
    async fn a_worker_answers_checkout_inspect_only_once_its_poll_says_it_can() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let worker = uuid::Uuid::new_v4().to_string();
        let registration =
            fleet::register(&host, &json!({"id":worker,"name":"Worker","os":"linux"}))
                .await
                .unwrap();
        let token = registration["token"].as_str().unwrap();
        let inspect = |host: Arc<Host>, worker: String| async move {
            machine_facts(&host, &worker)
                .await
                .unwrap()
                .unwrap()
                .inspect
        };
        // An older worker sends no flag and reads as unable to answer.
        fleet::protocol(&host, &json!({"id":worker,"token":token,"action":"poll"}))
            .await
            .unwrap();
        assert!(!inspect(host.clone(), worker.clone()).await);
        fleet::protocol(
            &host,
            &json!({"id":worker,"token":token,"action":"poll","checkout_inspect_support":true}),
        )
        .await
        .unwrap();
        assert!(inspect(host.clone(), worker.clone()).await);
        // The call is not reachable from the worker protocol's coordinator side.
        assert!(
            fleet::protocol(
                &host,
                &json!({"id":worker,"token":token,"action":"call","method":"checkout_inspect","args":{"path":"/"}}),
            )
            .await
            .is_err()
        );
        // Only the two workflow calls go through `machine_call`.
        let own = host.state.implementation.registry.lock().await.id.clone();
        assert!(
            machine_call(&host, &own, "implementation_launch", json!({}))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn the_director_runs_on_a_worker_only_once_it_says_it_can_and_the_coordinator_records_it()
    {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let worker = uuid::Uuid::new_v4().to_string();
        let registration =
            fleet::register(&host, &json!({"id":worker,"name":"Worker","os":"linux"}))
                .await
                .unwrap();
        let token = registration["token"].as_str().unwrap().to_owned();
        let director = crate::workflows::director::Director {
            machine: worker.clone(),
            provider: "claude".into(),
            model: "claude-sonnet-5-5".into(),
            effort: "low".into(),
        };
        let item: crate::tools::todo::Item = serde_json::from_value(json!({"id":41,"title":"Receipt downloads",
            "url":"https://dev.azure.com/example/Payments/_workitems/edit/41","project":"Payments","state":"Ready",
            "kind":"User Story","created_at":null,"dates":[]}))
        .unwrap();
        let ask = |host: Arc<Host>,
                   director: crate::workflows::director::Director,
                   item: crate::tools::todo::Item| async move {
            director::ask(
                &host,
                director::Ask {
                    machine: &director.machine,
                    director: &director,
                    task: crate::llm::implementation::DirectTask::Classify,
                    instructions: "Choose.",
                    input: "{}".into(),
                    item: &item,
                },
            )
            .await
        };
        // An older worker does not say it runs the director, and is never asked.
        fleet::protocol(&host, &json!({"id":worker,"token":token,"action":"poll"}))
            .await
            .unwrap();
        assert!(!machine_facts(&host, &worker).await.unwrap().unwrap().direct);
        let refused = ask(host.clone(), director.clone(), item.clone())
            .await
            .err()
            .unwrap();
        assert!(
            refused
                .to_string()
                .contains("Update the workflow director's machine")
        );
        // Its record says the director did not run; nothing else was recorded.
        let records = host.state.implementation.db.requests(None).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["state"], "failed");
        let poll =
            json!({"id":worker,"token":token,"action":"poll","workflow_direct_support":true});
        fleet::protocol(&host, &poll).await.unwrap();
        assert!(machine_facts(&host, &worker).await.unwrap().unwrap().direct);
        // The call is not reachable from the worker protocol's coordinator side.
        assert!(
            fleet::protocol(
                &host,
                &json!({"id":worker,"token":token,"action":"call","method":"workflow_direct","args":{}}),
            )
            .await
            .is_err()
        );
        let asking = tokio::spawn(ask(host.clone(), director.clone(), item.clone()));
        let command = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let reply = fleet::protocol(&host, &poll).await.unwrap();
                if !reply["command"].is_null() {
                    break reply["command"].clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(command["method"], "workflow_direct");
        assert_eq!(command["args"]["provider"], "claude");
        assert_eq!(command["args"]["instructions"], "Choose.");
        // In flight, the session is in Agent history as generating, on the worker.
        let generating = host.state.implementation.db.requests(None).unwrap();
        assert!(
            generating
                .iter()
                .any(|r| r["state"] == "generating" && r["machine"] == worker.as_str())
        );
        // The worker answers once; the coordinator keeps the record.
        let result = json!({"ok":true,"data":{"answer":{"workflow":"payments","reason":"Pagos."},"problem":null,
            "session_id":"44444444-4444-4444-8444-444444444444","started_at":"2026-11-07T10:00:00+00:00","finished_at":"2026-11-07T10:00:09+00:00"}});
        let mut answered = poll.clone();
        answered["results"] = json!([{"id":command["id"],"result":result}]);
        fleet::protocol(&host, &answered).await.unwrap();
        let answer = asking.await.unwrap().unwrap();
        assert_eq!(
            answer.direction.answer,
            Some(json!({"workflow":"payments","reason":"Pagos."}))
        );
        assert_eq!(
            answer.session_id.as_deref(),
            Some("44444444-4444-4444-8444-444444444444")
        );
        let history = host.state.implementation.db.history().unwrap();
        let recorded = history
            .iter()
            .find(|r| r["id"] == answer.run.as_str())
            .unwrap();
        assert_eq!(recorded["state"], "done");
        assert_eq!(recorded["purpose"], "classification");
        assert_eq!(recorded["machine"], worker.as_str());
        assert_eq!(recorded["started_at"], "2026-11-07T10:00:00+00:00");
        assert_eq!(
            recorded["item"]["url"],
            "https://dev.azure.com/example/Payments/_workitems/edit/41"
        );
        // A session ID that is not one is dropped, and the answer is kept.
        let asking = tokio::spawn(ask(host.clone(), director.clone(), item.clone()));
        let command = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let reply = fleet::protocol(&host, &poll).await.unwrap();
                if !reply["command"].is_null() {
                    break reply["command"].clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let result = json!({"ok":true,"data":{"answer":{"workflow":"payments","reason":"Pagos."},"problem":"Free text the worker made up",
            "session_id":"../../etc/passwd","started_at":"yesterday","finished_at":null}});
        let mut answered = poll.clone();
        answered["results"] = json!([{"id":command["id"],"result":result}]);
        fleet::protocol(&host, &answered).await.unwrap();
        let kept = asking.await.unwrap().unwrap();
        assert_eq!(kept.session_id, None);
        assert_eq!(
            kept.direction.answer,
            Some(json!({"workflow":"payments","reason":"Pagos."}))
        );
        assert_eq!(kept.direction.problem, None);
        let record = host
            .state
            .implementation
            .db
            .requests(Some(&kept.run))
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(record["state"], "done");
        assert!(record["session_id"].is_null() && record["started_at"].is_null());
        assert_eq!(record["output"], "");
        // Nothing was launched on any machine.
        assert!(
            host.state
                .implementation
                .db
                .list(None, None, true)
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn directory_configuration_browse_preserves_execution_and_profile_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let allowed = root.path().join("allowed");
        let other = root.path().join("other");
        fs::create_dir(&allowed).unwrap();
        fs::create_dir(&other).unwrap();
        host.state
            .implementation
            .registry
            .lock()
            .await
            .settings
            .roots = vec![allowed];
        let args = json!({"path":other});
        assert!(local(&host, "implementation_browse", &args).await.is_err());
        assert!(operation(&host, "machine_browse", &args).await.is_ok());

        let mut config = host.state.settings.config().unwrap();
        config.server.webhook_prefix = "/webhooks/synthetic".into();
        let repositories = config.server.data_dir.join("repositories");
        fs::create_dir_all(&repositories).unwrap();
        host.state.settings.save_config(&config).unwrap();
        assert!(operation(&host, "machine_browse", &args).await.is_err());
        let listing = operation(&host, "machine_browse", &json!({"path":repositories}))
            .await
            .unwrap();
        assert!(listing["parent"].is_null());
        assert!(local(&host, "implementation_browse", &args).await.is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&other, repositories.join("escape")).unwrap();
            assert!(
                operation(
                    &host,
                    "machine_browse",
                    &json!({"path":repositories.join("escape")})
                )
                .await
                .is_err()
            );
            assert_eq!(
                operation(&host, "machine_browse", &json!({"path":repositories}))
                    .await
                    .unwrap()["directories"],
                json!([])
            );
        }
    }
    #[tokio::test]
    async fn a_lost_machine_is_revoked_and_repeating_continues_without_redoing_it() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let own = host.state.implementation.registry.lock().await.id.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let key = crate::mirror::handoff::Receiver::generate().unwrap();
        let registration = fleet::register(
            &host,
            &json!({"id":id,"name":"Laptop","os":"linux","enrollment":"login",
                "installation":"a".repeat(64),"store_key":key.public(),"handoff":"b".repeat(64)}),
        )
        .await
        .unwrap();
        let lost = |args: Value| {
            let host = host.clone();
            async move {
                let mut request = control::Request::new("cloud_lost");
                request.args = args;
                crate::app::cloud::operate(&host, &request).await
            }
        };
        // The coordinator itself, both targets or none are refused.
        assert_eq!(lost(json!({"machine": own})).await.code, "invalid_input");
        assert_eq!(lost(json!({})).await.code, "invalid_input");
        assert_eq!(
            lost(json!({"machine": id, "installation": id})).await.code,
            "invalid_input"
        );
        // Without an account store only the machine is revoked; the rest is not needed.
        let reply = lost(json!({"machine": id})).await;
        assert!(reply.ok, "{:?}", reply.message);
        assert_eq!(reply.data["complete"], true);
        let states: Vec<_> = reply.data["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["step"].as_str().unwrap(), s["state"].as_str().unwrap()))
            .collect();
        assert_eq!(
            states,
            [
                ("machine", "done"),
                ("store", "not_needed"),
                ("token", "not_needed"),
                ("key", "not_needed")
            ]
        );
        // Its credential no longer works, and no store delivery is announced to anyone.
        assert!(
            fleet::protocol(
                &host,
                &json!({"id":id,"token":registration["token"],"action":"poll"})
            )
            .await
            .is_err()
        );
        // A finished record is not continued: the same machine again is already revoked.
        let again = lost(json!({"machine": id})).await;
        assert_eq!(again.data["steps"][0]["detail"], "It was already revoked.");
    }
    #[tokio::test]
    async fn the_coordinator_keeps_the_approved_store_key_and_what_a_machine_holds() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let id = uuid::Uuid::new_v4().to_string();
        let key = crate::mirror::handoff::Receiver::generate().unwrap();
        let registration = fleet::register(
            &host,
            &json!({"id":id,"name":"Laptop","os":"linux","enrollment":"login",
                "installation":"a".repeat(64),"store_key":key.public(),"handoff":"b".repeat(64)}),
        )
        .await
        .unwrap();
        // This coordinator has no account store: nothing to seal, and it says so.
        assert!(registration["store"]["unavailable"].is_string());
        let token = registration["token"].as_str().unwrap().to_owned();
        let held = "c".repeat(32);
        let delivery = |held: Value| {
            let (host, id, token) = (host.clone(), id.clone(), token.clone());
            async move {
                fleet::protocol(
                    &host,
                    &json!({"id":id,"token":token,"action":"store_delivery","held":held}),
                )
                .await
            }
        };
        let reply = delivery(json!({"token": held, "key": 1})).await.unwrap();
        assert_eq!(reply["state"], "unavailable");
        assert!(reply.get("sealed").is_none());
        {
            let data = host.state.implementation.registry.lock().await;
            let machine = data.machines.iter().find(|m| m.id == id).unwrap();
            assert_eq!(machine.store_key, key.public());
            assert_eq!(
                machine
                    .store_held
                    .as_ref()
                    .map(|h| (h.token.as_str(), h.key)),
                Some((held.as_str(), 1))
            );
        }
        let listed = store_machines(&host).await;
        assert_eq!(listed.len(), 1);
        assert!(listed[0].receives);
        // Only fingerprints are accepted, and a revoked machine is refused before anything else.
        assert!(
            delivery(json!({"token": "not a fingerprint", "key": 1}))
                .await
                .is_err()
        );
        {
            let mut data = host.state.implementation.registry.lock().await;
            management::revoke(&mut data, &id).unwrap();
            host.state.implementation.save(&data).unwrap();
        }
        assert!(delivery(json!({"token": held, "key": 1})).await.is_err());
        assert!(store_machines(&host).await.is_empty());
    }
    #[tokio::test]
    async fn worker_authentication_and_claiming_do_not_replay_commands() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let id = uuid::Uuid::new_v4().to_string();
        let registration = fleet::register(
            &host,
            &json!({"id":id,"name":"Worker","os":"linux","account_version":"synthetic"}),
        )
        .await
        .unwrap();
        let token = registration["token"].as_str().unwrap();
        assert!(
            fleet::protocol(&host, &json!({"id":id,"token":"invalid","action":"poll"}))
                .await
                .is_err()
        );
        assert!(
            fleet::protocol(
                &host,
                &json!({"id":id,"token":token,"action":"call","method":"start_assistant"})
            )
            .await
            .is_err()
        );
        host.state.implementation.calls.lock().await.push(Call {
            id: "request".into(),
            machine: id.clone(),
            method: "implementation_runs".into(),
            args: json!({}),
            claimed: false,
            result: None,
        });
        let poll = json!({"id":id,"token":token,"action":"poll"});
        assert_eq!(
            fleet::protocol(&host, &poll).await.unwrap()["command"]["id"],
            "request"
        );
        assert!(fleet::protocol(&host, &poll).await.unwrap()["command"].is_null());
        fleet::protocol(&host,&json!({"id":id,"token":token,"action":"poll","results":[{"id":"request","result":{"ok":true,"data":{"runs":[]}}}]})).await.unwrap();
        assert!(
            host.state.implementation.calls.lock().await[0]
                .result
                .is_some()
        );
        let _operations = host.state.operations.lock().await;
        let reply = control::dispatch(&host, control::Request::new("machine_settings")).await;
        assert!(reply.ok);
    }
    #[tokio::test]
    async fn machine_login_reenrolls_only_the_same_installation_without_duplicates_or_grants() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let id = uuid::Uuid::new_v4().to_string();
        let (own, other) = ("a".repeat(64), "b".repeat(64));
        let login = |installation: &str| json!({"id":id,"name":"Synthetic laptop","os":"windows","installation":installation,"enrollment":"login"});
        let poll = |token: &Value| json!({"id":id,"token":token,"action":"poll"});
        let first = fleet::register(&host, &login(&own)).await.unwrap();
        // Repeating sign-in from the same installation rotates its credential in place.
        let second = fleet::register(&host, &login(&own)).await.unwrap();
        assert_ne!(first["token"], second["token"]);
        assert!(
            fleet::protocol(&host, &poll(&first["token"]))
                .await
                .is_err()
        );
        assert!(
            fleet::protocol(&host, &poll(&second["token"]))
                .await
                .is_ok()
        );
        // Another installation claiming this public machine ID is refused and changes nothing.
        let clone = fleet::register(&host, &login(&other)).await.unwrap_err();
        assert!(
            clone.to_string().contains("another installation"),
            "{clone}"
        );
        assert!(fleet::register(&host, &login("")).await.is_err());
        assert!(
            fleet::protocol(&host, &poll(&second["token"]))
                .await
                .is_ok()
        );
        {
            let data = host.state.implementation.registry.lock().await;
            assert_eq!(data.machines.len(), 1);
            assert_eq!(data.machines[0].installation, own);
            assert_eq!(data.machines[0].os, "windows");
            // Trust grants no roots, Teams ownership or assistant start.
            assert_eq!(data.teams_machine, data.id);
            assert!(data.settings.roots.is_empty());
        }
        assert!(host.state.running.lock().await.is_none());
        // An operator pairing file explicitly authorizes this machine ID and may replace it.
        let paired = json!({"id":id,"name":"Synthetic laptop","os":"windows","installation":other,"enrollment":"pairing"});
        assert!(fleet::register(&host, &paired).await.is_ok());
        // A legacy pairing without an installation proof cannot be claimed by sign-in.
        let legacy = uuid::Uuid::new_v4().to_string();
        fleet::register(&host, &json!({"id":legacy,"name":"Older","os":"macos"}))
            .await
            .unwrap();
        let claim = json!({"id":legacy,"name":"Older","os":"macos","installation":own,"enrollment":"login"});
        assert!(fleet::register(&host, &claim).await.is_err());
        assert_eq!(
            host.state
                .implementation
                .registry
                .lock()
                .await
                .machines
                .len(),
            2
        );
    }
    #[tokio::test]
    async fn machine_login_operations_return_no_secrets_and_clean_up_locally() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let _operations = host.state.operations.lock().await;
        let none = control::dispatch(&host, control::Request::new("machine_login_status")).await;
        assert!(none.ok);
        assert_eq!(none.data["state"], "none");
        let finish = control::dispatch(&host, control::Request::new("machine_login_finish")).await;
        assert!(!finish.ok);
        assert_eq!(finish.code, "invalid_input");
        let cancel = control::dispatch(&host, control::Request::new("machine_login_cancel")).await;
        assert!(cancel.ok);
        // Only HTTPS coordinator origins without credentials, paths or queries are accepted.
        for url in [
            "http://assistant.example.com",
            "https://user@assistant.example.com",
            "https://assistant.example.com/other",
        ] {
            let mut request = control::Request::new("machine_login");
            request.args = json!({"url":url});
            assert!(!control::dispatch(&host, request).await.ok);
        }
        let mut request = control::Request::new("machine_login");
        request.args = json!({});
        let missing = control::dispatch(&host, request).await;
        assert!(!missing.ok);
        assert!(missing.message.unwrap().contains("HTTPS address"));
        // The installation key stays local and stable; only its hash is ever sent.
        let profile = &host.state.implementation.root;
        let first = login::installation(profile).unwrap();
        assert_eq!(first, login::installation(profile).unwrap());
        let local = fs::read_to_string(profile.join("installation.json")).unwrap();
        assert!(!local.contains(&first));
    }
    #[test]
    fn restart_preserves_uncertain_creations_without_replaying_writes() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path()).unwrap();
        {
            let mut data = state.registry.try_lock().unwrap();
            data.creations.insert(
                "synthetic-plan".into(),
                json!({"state":"sending","created":[]}),
            );
            state.save(&data).unwrap();
        }
        let restarted = State::new(root.path()).unwrap();
        assert_eq!(
            restarted.registry.try_lock().unwrap().creations["synthetic-plan"]["state"],
            "uncertain"
        );
        assert!(restarted.tasks.try_lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn worker_and_browser_share_the_coordinator_creation_ledger() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let worker = uuid::Uuid::new_v4().to_string();
        let registration = fleet::register(
            &host,
            &json!({"id":worker,"name":"Worker","os":"macos","account_version":"synthetic"}),
        )
        .await
        .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let record = json!({"state":"uncertain","parent":"https://github.com/example/repo/issues/1","children":[{"title":"Private reviewed text"}],"created":[{"url":"https://github.com/example/repo/issues/2"}]});
        host.state
            .implementation
            .registry
            .lock()
            .await
            .creations
            .insert(id.clone(), record);
        let read = operation(&host, "implementation_creation", &json!({"id":id}))
            .await
            .unwrap();
        let submitted = fleet::protocol(&host, &json!({"id":worker,"token":registration["token"],"action":"call","method":"implementation_create","args":{"machine":worker,"id":id,"confirmed":true}})).await.unwrap();
        assert_eq!(submitted, read);
        assert_eq!(submitted["state"], "uncertain");
        assert!(submitted.get("children").is_none());
        assert!(host.state.implementation.tasks.lock().await.is_empty());
    }
    #[tokio::test]
    async fn allowed_roots_require_explicit_canonical_grants_and_remote_edits_run_once() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let own = host.state.implementation.registry.lock().await.id.clone();
        let work = root.path().join("workspace");
        fs::create_dir(&work).unwrap();
        let work = work.canonicalize().unwrap();
        assert!(
            directories(&host, std::slice::from_ref(&work))
                .await
                .is_err()
        );
        let args = json!({"machine":own,"name":"Desk","roots":[work]});
        assert!(operation(&host, "machine_update", &args).await.is_ok());
        assert_eq!(
            directories(&host, std::slice::from_ref(&work))
                .await
                .unwrap(),
            vec![work.canonicalize().unwrap()]
        );
        assert!(
            operation(
                &host,
                "machine_update",
                &json!({"machine":own,"roots":["relative"]})
            )
            .await
            .is_err()
        );
        #[cfg(unix)]
        {
            let alias = root.path().join("alias");
            std::os::unix::fs::symlink(&work, &alias).unwrap();
            assert!(
                operation(
                    &host,
                    "machine_update",
                    &json!({"machine":own,"roots":[alias]})
                )
                .await
                .is_err()
            );
        }
        let worker = uuid::Uuid::new_v4().to_string();
        let registration =
            fleet::register(&host, &json!({"id":worker,"name":"Remote","os":"windows"}))
                .await
                .unwrap();
        assert!(
            operation(&host, "implementation_browse", &json!({"machine":worker}))
                .await
                .is_err()
        );
        let poll = json!({"id":worker,"token":registration["token"],"action":"poll","assistant_phase":"stopped","management_support":true,"roots":[]});
        fleet::protocol(&host, &poll).await.unwrap();
        let caller = host.clone();
        let target = worker.clone();
        let selected = work.clone();
        let edit = tokio::spawn(async move {
            operation(
                &caller,
                "machine_update",
                &json!({"machine":target,"name":"Remote desk","roots":[selected]}),
            )
            .await
        });
        let command = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let next = fleet::protocol(&host, &poll).await.unwrap();
                if !next["command"].is_null() {
                    break next["command"].clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(command["method"], "machine_roots");
        assert!(fleet::protocol(&host, &poll).await.unwrap()["command"].is_null());
        fleet::protocol(&host, &json!({"id":worker,"token":registration["token"],"action":"poll","assistant_phase":"stopped","results":[{"id":command["id"],"result":{"ok":true,"data":{"roots":[work]}}}]})).await.unwrap();
        assert!(edit.await.unwrap().is_ok());
        let detail = operation(&host, "machine_detail", &json!({"machine":worker}))
            .await
            .unwrap();
        // Shown in the plain spelling (no Windows verbatim prefix), whatever the worker reported.
        assert_eq!(detail["roots"], json!([paths::plain(&work)]));
        assert_eq!(detail["name"], "Remote desk");
        assert!(
            operation(&host, "machine_update", &json!({"machine":own,"roots":[]}))
                .await
                .is_ok()
        );
        assert!(directories(&host, &[work]).await.is_err());
    }
    #[tokio::test]
    async fn old_machine_credentials_cannot_proxy_into_a_workers_new_account() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let worker = uuid::Uuid::new_v4().to_string();
        let registration = fleet::register(
            &host,
            &json!({"id":worker,"name":"Previous account computer","os":"linux"}),
        )
        .await
        .unwrap();
        let own = host.state.implementation.registry.lock().await.id.clone();
        write_private(
            &host.state.implementation.root.join("connection.json"),
            &json!({"id":own,"url":"https://another-account.example.com","account":1002})
                .to_string(),
        )
        .unwrap();
        let error = fleet::protocol(&host, &json!({"id":worker,"token":registration["token"],"action":"call","method":"machine_list","args":{}})).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "A connected worker cannot serve coordinator requests."
        );
    }
}
