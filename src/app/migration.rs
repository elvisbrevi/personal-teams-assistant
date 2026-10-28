//! Assisted migration of selected application credentials into the signed-in account.
//!
//! Discovery reports only the names, origins and eligibility of the credentials this installation
//! is configured to use, never a value and never the rest of the environment. The user chooses
//! entries, confirms the destination account and writes the descriptions; the host then reads
//! each chosen value from the installation's own store and creates the account variable itself,
//! so a value never passes through a client, an agent, an argument or a file. A step is journaled
//! (without values) before and after it happens; a deterministic entry ID makes a repeated
//! creation a replay, and every uncertain write is inspected before it is repeated. Consumers are
//! switched only after the destination value was read back and matched. The original stores are
//! never modified.
use super::{authority, consumers, control::Reply, implementation, portable, standing, *};
use crate::{
    consumers as rules,
    variables::{self as vault, Secret},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Profiles with a migration in progress: one at a time per installation.
static BUSY: std::sync::Mutex<std::collections::BTreeSet<PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

struct Running(PathBuf);
impl Running {
    fn start(host: &Host) -> Option<Self> {
        let profile = host.state.config_path.parent().unwrap().to_path_buf();
        BUSY.lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(profile.clone())
            .then_some(Self(profile))
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        BUSY.lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

const MAX_ITEMS: usize = 8;
const SCHEMA: u32 = 1;

/// Application state that is never an account variable, with the reason shown to the user.
const INTERNAL: &str =
    "Internal application key: it protects local state and never leaves this installation.";
const MACHINE: &str =
    "Machine or control credential of this installation: signing in issues it again.";
const WEBHOOK: &str = "Webhook secret of this installation's listener.";
const HOSTING: &str = "Hosting or tunnel credential of the server that runs this installation.";
const SESSION: &str =
    "Provider sign-in session: authenticate it again on the computer that needs it.";
const RETIRED: &str = "Retired credential that nothing reads any more.";
const RESERVED: &str = "Protected runtime setting: it cannot be set from a variable.";

/// Why a configured credential is never migrated, or `None` when it may be.
fn excluded(name: &str) -> Option<(&'static str, &'static str)> {
    let upper = name.to_ascii_uppercase();
    let has = |part: &str| upper.contains(part);
    if has("ENCRYPTION") {
        Some(("internal", INTERNAL))
    } else if upper.starts_with("PTA_") || has("MACHINE") || has("CONTROL") {
        Some(("machine", MACHINE))
    } else if has("WEBHOOK") {
        Some(("webhook", WEBHOOK))
    } else if has("TUNNEL") || has("CLOUDFLARE") {
        Some(("infrastructure", HOSTING))
    } else if has("OAUTH") || has("SESSION") {
        Some(("provider_session", SESSION))
    } else if super::RETIRED_CREDENTIALS.contains(&name) {
        Some(("retired", RETIRED))
    } else if rules::reserved_env(name) {
        Some(("reserved", RESERVED))
    } else {
        None
    }
}

/// The credentials this installation is configured to use, plus the few names that may exist
/// without being configured so they are reported as excluded instead of silently ignored.
fn configured(config: &Config) -> Vec<String> {
    let mut names = super::credential_names(config);
    names.push("GITHUB_OAUTH_TOKENS".into());
    names.extend(super::RETIRED_CREDENTIALS.iter().map(|n| (*n).to_owned()));
    names.sort();
    names.dedup();
    names
}

pub(super) fn handles(method: &str) -> bool {
    method.starts_with("variable_migration_")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    destination: String,
    items: Vec<ItemArgs>,
    #[serde(default)]
    settings: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemArgs {
    credential: String,
    #[serde(default)]
    name: Option<String>,
    description: String,
    #[serde(default)]
    existing: Option<Existing>,
    #[serde(default)]
    bind: bool,
}
/// The explicit resolution of a name that already exists in the destination, naming the version
/// the user reviewed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Existing {
    action: String,
    version: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    credential: String,
    name: String,
    id: String,
    /// `create`, `use` (the existing entry already holds the value) or `replace` (rotate it).
    action: String,
    /// The version of the existing entry the user reviewed (`use` and `replace`).
    expected: Option<u64>,
    consumer: Option<String>,
    bind: bool,
    /// `planned`, `writing`, `uncertain`, `written`, `verification_failed`, `verified`, `complete`.
    stage: String,
    version: Option<u64>,
    error: Option<String>,
    at: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsStage {
    stage: String,
    error: Option<String>,
    at: i64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    destination: String,
    updated_at: i64,
    items: Vec<Stage>,
    settings: Option<SettingsStage>,
}

fn input(message: impl Into<String>) -> vault::Error {
    vault::Error::Input(message.into())
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

struct Destination {
    key: String,
    view: Value,
    connected: bool,
}

/// The account that would receive the variables, named by an opaque key the caller must echo.
fn destination(host: &Host) -> std::result::Result<Destination, vault::Error> {
    let link = implementation::link(&host.state)
        .map_err(|_| vault::Error::Storage("The coordinator connection is unreadable.".into()))?;
    let standing = host.state.standing.view(link.as_ref());
    if let Some(link) = link {
        let key = standing::qualifier(&link);
        return Ok(Destination {
            view: json!({
                "key": key,
                "mode": "connected",
                "coordinator": link.origin,
                "account": format!("Account {} on {}", link.account, link.origin),
                "standing": standing,
            }),
            key,
            connected: true,
        });
    }
    let identity = web::account_identity(&host.state.authority).unwrap_or(Value::Null);
    let owner = identity["username"].as_str().unwrap_or("").to_owned();
    let digest = Sha256::digest(
        [
            b"personal-teams-assistant/migration-destination/v1".as_slice(),
            host.state
                .authority
                .account
                .as_ref()
                .map_or("installation", |a| a.id.as_str())
                .as_bytes(),
        ]
        .concat(),
    );
    let key: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    Ok(Destination {
        view: json!({
            "key": key,
            "mode": "own",
            "coordinator": null,
            "account": if owner.is_empty() {
                "This installation's own account".to_owned()
            } else {
                format!("Account {owner} on this installation")
            },
            "standing": standing,
        }),
        key,
        connected: false,
    })
}

/// The same entry ID for the same account and credential, so repeating a creation is a replay
/// and never a second entry.
fn entry_id(destination: &str, credential: &str) -> String {
    let digest = Sha256::digest(
        [
            b"personal-teams-assistant/migration-entry/v1".as_slice(),
            destination.as_bytes(),
            b"\0",
            credential.as_bytes(),
        ]
        .concat(),
    );
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .hyphenated()
        .to_string()
}

async fn call(host: &Arc<Host>, method: &str, args: Value) -> Reply {
    let mut request = control::Request::new(method);
    request.args = args;
    if method.starts_with("portable_") {
        portable::operate(host, &request).await
    } else {
        // Boxed: variables dispatch reaches this module, so the futures would be recursive.
        Box::pin(super::variables::operate(host, &request)).await
    }
}

fn describe(reply: &Reply) -> String {
    reply
        .message
        .clone()
        .unwrap_or_else(|| "The operation failed.".into())
}

/// The destination account's variables, from the coordinator's synchronized catalog or this
/// installation's own store.
async fn catalog(
    host: &Arc<Host>,
    connected: bool,
    refresh: bool,
) -> std::result::Result<Vec<vault::Metadata>, Box<Reply>> {
    if connected && refresh {
        let synced = call(host, "variable_sync", json!({})).await;
        if !synced.ok {
            return Err(Box::new(synced));
        }
    }
    let listed = call(host, "variable_list", json!({})).await;
    if !listed.ok {
        return Err(Box::new(listed));
    }
    serde_json::from_value(listed.data["variables"].clone()).map_err(|_| {
        Box::new(Reply::error(
            "operation_failed",
            1,
            "The variable catalog was unreadable.",
        ))
    })
}

fn journal_path(host: &Host, key: &str) -> PathBuf {
    host.state
        .config_path
        .parent()
        .unwrap()
        .join("migration")
        .join(format!("{key}.json"))
}

fn load(host: &Host, key: &str) -> Option<Journal> {
    let bytes = fs::read(journal_path(host, key)).ok()?;
    serde_json::from_slice::<Journal>(&bytes)
        .ok()
        .filter(|j| j.schema == SCHEMA && j.destination == key)
}

fn save(host: &Host, journal: &mut Journal) -> std::result::Result<(), vault::Error> {
    journal.updated_at = now();
    let path = journal_path(host, &journal.destination);
    if let Some(parent) = path.parent() {
        security::private_dir(parent)?;
    }
    write_private(
        &path,
        &serde_json::to_string(journal).map_err(|_| {
            vault::Error::Storage("The migration record could not be saved.".into())
        })?,
    )
    .map_err(|_| vault::Error::Storage("The migration record could not be saved.".into()))
}

fn suggestion(name: &str) -> String {
    match rules::built_in_credential(name) {
        Some(consumer) => format!("{} migrated from this installation", consumer.title),
        None => format!("Credential {name} migrated from this installation"),
    }
}

fn view(stage: &Stage) -> Value {
    json!({
        "credential": stage.credential,
        "name": stage.name,
        "id": stage.id,
        "action": stage.action,
        "stage": stage.stage,
        "complete": stage.stage == "complete",
        "version": stage.version,
        "bind": stage.bind,
        "consumer": stage.consumer,
        "error": stage.error,
        "at": stage.at,
    })
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Reply {
    let result = async {
        let invocation = authority::invocation(&host.state)
            .map_err(|_| vault::Error::Conflict(authority::ACCOUNT_UNAVAILABLE.into()))?;
        // Machine credentials never reach the installation's own credential stores.
        if invocation.transport == authority::Transport::Machine {
            return Err(vault::Error::Conflict(
                "This variable operation is not available from this client.".into(),
            ));
        }
        match request.method.as_str() {
            "variable_migration_inspect" => {
                serde_json::from_value::<Empty>(request.args.clone())
                    .map_err(|_| input("Invalid arguments for this migration operation."))?;
                inspect(host).await
            }
            "variable_migration_status" => {
                serde_json::from_value::<Empty>(request.args.clone())
                    .map_err(|_| input("Invalid arguments for this migration operation."))?;
                status(host)
            }
            "variable_migration_run" => {
                let args: RunArgs = serde_json::from_value(request.args.clone())
                    .map_err(|_| input("Invalid arguments for this migration operation."))?;
                return Ok(run(host, args).await);
            }
            _ => Err(input("Unknown migration operation.")),
        }
        .map(Reply::success)
    }
    .await;
    match result {
        Ok(reply) => reply,
        Err(error) => super::variables::failure(error),
    }
}

/// What can be migrated, into which account, and what is already there. Reads names, origins
/// and metadata only.
async fn inspect(host: &Arc<Host>) -> std::result::Result<Value, vault::Error> {
    let destination = destination(host)?;
    let config = read_config(&host.state.config_path)
        .map_err(|_| vault::Error::Storage("The configuration could not be read.".into()))?;
    // The catalog may be unavailable offline: the review then says so instead of guessing.
    let (catalog, catalog_error) = match catalog(host, destination.connected, true).await {
        Ok(catalog) => (catalog, None),
        Err(reply) => (Vec::new(), Some(describe(&reply))),
    };
    let journal = load(host, &destination.key);
    let mut credentials = Vec::new();
    for name in configured(&config) {
        let origin = origin_of(host, &name);
        let present = matches!(origin, Origin::Found(_));
        let excluded = excluded(&name);
        if !present && excluded.is_some() {
            // An excluded name nothing stores is not worth a row.
            continue;
        }
        let consumer = rules::built_in_credential(&name);
        let id = entry_id(&destination.key, &name);
        let existing = catalog.iter().find(|v| v.name.eq_ignore_ascii_case(&name));
        let migrated = journal
            .as_ref()
            .and_then(|j| j.items.iter().find(|s| s.credential == name))
            .map(view);
        let (status, reason) = match (&excluded, &origin) {
            (Some((_, reason)), _) => ("excluded", Some(*reason)),
            (None, Origin::Unreadable) => (
                "unavailable",
                Some("The stored credential could not be inspected."),
            ),
            (None, Origin::Missing) => ("not_set", None),
            (None, Origin::Found(_)) => ("eligible", None),
        };
        credentials.push(json!({
            "credential": name,
            "origin": match &origin { Origin::Found(origin) => Some(*origin), _ => None },
            "status": status,
            "excluded": excluded.map(|(code, _)| code),
            "reason": reason,
            "account_bound": security::secret_source(&name).ok().flatten() == Some("variable"),
            "consumer": consumer.map(|c| json!({"id":c.id,"title":c.title,"destination":c.destination})),
            "suggested": {"name": name, "description": suggestion(&name)},
            "entry_id": id,
            "existing": existing.map(|v| json!({
                "id": v.id, "name": v.name, "description": v.description, "version": v.version,
                "same_entry": v.id == id,
            })),
            "migration": migrated,
        }));
    }
    let settings = if destination.connected {
        let reply = call(host, "portable_status", json!({})).await;
        if reply.ok {
            let sync = &reply.data["sync"];
            json!({
                "mode": "replica",
                "state": sync["state"],
                "local_changes": sync["local_changes"],
                "loaded": sync["loaded"],
                "saved": sync["saved"],
                "adopt": "variable_migration_run settings=adopt_account replaces this computer's portable settings with the account's after backing them up",
            })
        } else {
            json!({"mode":"replica","state":"unknown","error":describe(&reply)})
        }
    } else {
        json!({"mode":"authoritative","state":"authoritative"})
    };
    Ok(json!({
        "destination": destination.view,
        "credentials": credentials,
        "settings": settings,
        "variables": catalog,
        "catalog_error": catalog_error,
        "limits": {"items": MAX_ITEMS, "name":vault::MAX_NAME, "description":vault::MAX_DESCRIPTION},
        "remains_local": [
            "Microsoft Teams sign-in",
            "Coding-provider sign-ins (Codex, Claude Code, OpenCode)",
            "GitHub and Azure CLI sessions",
            "Repository folders and allowed directories of this computer",
        ],
        "originals": "Nothing here changes or deletes the original credentials; they stay as the way back.",
    }))
}

enum Origin {
    Found(&'static str),
    Missing,
    Unreadable,
}

/// Credentials a test pretends the installation's own stores hold, by profile and name.
#[cfg(test)]
pub(super) static STORED: std::sync::Mutex<BTreeMap<(PathBuf, String), String>> =
    std::sync::Mutex::new(BTreeMap::new());

#[cfg(test)]
fn stored(host: &Host, name: &str) -> Option<String> {
    let key = (
        host.state.config_path.parent().unwrap().to_path_buf(),
        name.to_owned(),
    );
    STORED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned()
}

/// Where the installation's own stores hold a credential, ignoring account variables.
fn origin_of(host: &Host, name: &str) -> Origin {
    let _ = host;
    #[cfg(test)]
    if stored(host, name).is_some() {
        return Origin::Found("system");
    }
    match security::legacy_source(name) {
        Ok(Some(origin)) => Origin::Found(origin),
        Ok(None) => Origin::Missing,
        Err(_) => Origin::Unreadable,
    }
}

/// The value held by the installation's own store, read only to copy it.
fn read(host: &Host, name: &str) -> Result<String> {
    #[cfg(test)]
    if let Some(value) = stored(host, name) {
        return Ok(value);
    }
    let _ = host;
    security::legacy_secret(name)
}

fn status(host: &Arc<Host>) -> std::result::Result<Value, vault::Error> {
    let destination = destination(host)?;
    let journal = load(host, &destination.key);
    Ok(json!({
        "destination": destination.view,
        "items": journal.as_ref().map(|j| j.items.iter().map(view).collect::<Vec<_>>()).unwrap_or_default(),
        "settings": journal.as_ref().and_then(|j| j.settings.clone()).map(|s| json!({"stage":s.stage,"error":s.error,"at":s.at})),
        "updated_at": journal.as_ref().map(|j| j.updated_at),
        "complete": journal.as_ref().is_some_and(|j| !j.items.is_empty() && j.items.iter().all(|s| s.stage == "complete")),
    }))
}

/// A reviewed item with the value read from the original store (memory only).
struct Planned {
    stage: Stage,
    description: String,
    /// The description of the existing entry a `replace` rotates.
    existing_description: Option<String>,
    value: Secret,
}

struct Problem {
    credential: String,
    problem: String,
}

fn refusal(code: &str, exit: i32, message: &str, problems: Vec<Problem>) -> Reply {
    let mut reply = Reply::error(code, exit, message);
    reply.data = json!({
        "transferred": false,
        "problems": problems.iter().map(|p| json!({"credential":p.credential,"problem":p.problem})).collect::<Vec<_>>(),
    });
    reply
}

async fn run(host: &Arc<Host>, args: RunArgs) -> Reply {
    let Some(_busy) = Running::start(host) else {
        return Reply::error("state_conflict", 6, "A migration is already running.");
    };
    let destination = match destination(host) {
        Ok(destination) => destination,
        Err(error) => return super::variables::failure(error),
    };
    // The reviewed account must still be the signed-in one.
    if args.destination != destination.key {
        return refusal(
            "state_conflict",
            6,
            "The destination account is not the one that was reviewed (the installation signed in elsewhere or you named another account). Inspect again and confirm the account shown. Nothing was transferred.",
            Vec::new(),
        );
    }
    if args.items.is_empty() || args.items.len() > MAX_ITEMS {
        return refusal(
            "invalid_input",
            2,
            &format!("Select between 1 and {MAX_ITEMS} credentials to migrate."),
            Vec::new(),
        );
    }
    if let Some(settings) = &args.settings
        && (settings != "adopt_account" || !destination.connected)
    {
        return refusal(
            "invalid_input",
            2,
            "Settings can only be set to adopt_account, on an installation signed in to a coordinator.",
            Vec::new(),
        );
    }
    let config = match read_config(&host.state.config_path) {
        Ok(config) => config,
        Err(_) => {
            return Reply::error(
                "operation_failed",
                1,
                "The configuration could not be read.",
            );
        }
    };
    // A live, current view of the destination before anything is decided or written.
    let catalog = match catalog(host, destination.connected, true).await {
        Ok(catalog) => catalog,
        Err(reply) => return *reply,
    };
    let configured = configured(&config);
    let journal = load(host, &destination.key);
    let mut planned: Vec<Planned> = Vec::new();
    let mut problems: Vec<Problem> = Vec::new();
    for item in &args.items {
        match plan(host, &destination, &configured, &catalog, item, &planned) {
            Ok(entry) => planned.push(entry),
            Err(problem) => problems.push(Problem {
                credential: item.credential.clone(),
                problem,
            }),
        }
    }
    if !problems.is_empty() {
        let conflict = problems.iter().any(|p| p.problem.starts_with("collision"));
        return refusal(
            if conflict {
                "state_conflict"
            } else {
                "invalid_input"
            },
            if conflict { 6 } else { 2 },
            "The selection cannot be migrated as reviewed. Nothing was transferred; fix the items listed and review again.",
            problems,
        );
    }
    let mut journal = journal.unwrap_or(Journal {
        schema: SCHEMA,
        destination: destination.key.clone(),
        updated_at: now(),
        items: Vec::new(),
        settings: None,
    });
    for entry in &planned {
        match journal
            .items
            .iter_mut()
            .find(|s| s.credential == entry.stage.credential)
        {
            Some(old) if resumable(old, &entry.stage) => {}
            Some(old) => *old = entry.stage.clone(),
            None => journal.items.push(entry.stage.clone()),
        }
    }
    if let Err(error) = save(host, &mut journal) {
        return super::variables::failure(error);
    }
    let mut restart = false;
    let mut fatal: Option<(String, i32, String)> = None;
    for entry in &planned {
        if let Some((_, _, message)) = &fatal {
            // A failure that affects every item (offline, revoked) is not retried per item.
            let stage = stage_mut(&mut journal, &entry.stage.credential);
            if stage.stage != "complete" {
                stage.error = Some(format!("Not attempted: {message}"));
            }
            continue;
        }
        let outcome = process(host, &destination, &catalog, entry, &mut journal).await;
        restart |= outcome.restart;
        if let Some(failure) = outcome.fatal {
            fatal = Some(failure);
        }
    }
    let settings = if args.settings.is_some() {
        Some(adopt_settings(host, &mut journal).await)
    } else {
        None
    };
    let _ = save(host, &mut journal);
    let items: Vec<Value> = planned
        .iter()
        .filter_map(|e| {
            journal
                .items
                .iter()
                .find(|s| s.credential == e.stage.credential)
        })
        .map(view)
        .collect();
    let complete = items.iter().all(|i| i["complete"] == true)
        && settings.as_ref().is_none_or(|s| s["stage"] == "complete");
    let data = json!({
        "destination": destination.view,
        "items": items,
        "settings": settings,
        "complete": complete,
        "restart_required": restart,
        "originals": "The original credentials were not modified. To return a consumer to its original credential, unbind it (`pta consumers unbind CONSUMER`).",
    });
    if complete {
        return Reply::success(data);
    }
    let (code, exit) = fatal.as_ref().map_or_else(
        || {
            if items.iter().any(|i| i["stage"] == "uncertain") {
                ("dependency_or_network", 5)
            } else {
                ("operation_failed", 1)
            }
        },
        |(code, exit, _)| (code.as_str(), *exit),
    );
    let mut reply = Reply::error(
        code,
        exit,
        "The migration is incomplete at the stage shown for each item. The original setup is unchanged and usable; fix the cause and run the same migration again to resume safely.",
    );
    reply.data = data;
    reply
}

/// Whether the journaled item is the same reviewed plan, so the run continues it.
fn resumable(old: &Stage, new: &Stage) -> bool {
    old.name == new.name
        && old.id == new.id
        && old.action == new.action
        && old.expected == new.expected
        && old.bind == new.bind
        && old.consumer == new.consumer
        && old.stage != "planned"
}

fn stage_mut<'a>(journal: &'a mut Journal, credential: &str) -> &'a mut Stage {
    journal
        .items
        .iter_mut()
        .find(|s| s.credential == credential)
        .expect("every planned item is journaled")
}

/// Check one reviewed item against what exists now, before anything is transferred.
fn plan(
    host: &Host,
    destination: &Destination,
    configured: &[String],
    catalog: &[vault::Metadata],
    item: &ItemArgs,
    earlier: &[Planned],
) -> std::result::Result<Planned, String> {
    let credential = item.credential.as_str();
    if !configured.iter().any(|c| c == credential) {
        return Err("This is not a credential this installation is configured to use.".into());
    }
    if let Some((_, reason)) = excluded(credential) {
        return Err(format!("Excluded from migration: {reason}"));
    }
    let name = item.name.clone().unwrap_or_else(|| credential.to_owned());
    vault::validate_name(&name).map_err(|e| e.to_string())?;
    vault::validate_description(&item.description).map_err(|e| e.to_string())?;
    if earlier
        .iter()
        .any(|p| p.stage.name.eq_ignore_ascii_case(&name) || p.stage.credential == credential)
    {
        return Err("The selection names this credential or variable twice.".into());
    }
    let value = read(host, credential).map(Secret::new).map_err(|_| {
        "The original credential could not be read, so nothing can be copied.".to_owned()
    })?;
    vault::validate_value(value.expose()).map_err(|e| e.to_string())?;
    let consumer = rules::built_in_credential(credential);
    if let Some(consumer) = consumer {
        rules::validate_value(consumer, value.expose()).map_err(|e| e.to_string())?;
    }
    if item.bind && consumer.is_none() {
        return Err("No consumer uses this credential yet, so there is nothing to switch.".into());
    }
    let id = entry_id(&destination.key, credential);
    let existing = catalog.iter().find(|v| v.name.eq_ignore_ascii_case(&name));
    let (action, expected, existing_description) = match (existing, &item.existing) {
        // Our own earlier creation: the same entry, resumed or repeated.
        (Some(found), None) if found.id == id && found.description == item.description => {
            ("create", None, None)
        }
        (None, None) => ("create", None, None),
        (None, Some(_)) => {
            return Err(
                "There is no existing variable with this name to use or replace; remove the resolution."
                    .into(),
            );
        }
        (Some(found), None) => {
            return Err(format!(
                "collision: the account already has a variable named {} (version {}). Choose another name, or explicitly use or replace it at that version.",
                found.name, found.version
            ));
        }
        (Some(found), Some(resolution)) => {
            if resolution.version != found.version {
                return Err(format!(
                    "collision: the variable {} is now at version {}, not {}. Review it again.",
                    found.name, found.version, resolution.version
                ));
            }
            let action = match resolution.action.as_str() {
                "use" => "use",
                "replace" => "replace",
                _ => return Err("The resolution must be \"use\" or \"replace\".".into()),
            };
            if action == "use" && found.description != item.description {
                return Err(format!(
                    "collision: using the existing variable keeps its description, which differs from the one you wrote. Edit the description to match or choose replace (version {}).",
                    found.version
                ));
            }
            (action, Some(found.version), Some(found.description.clone()))
        }
    };
    let id = existing
        .filter(|_| action != "create")
        .map_or(id, |v| v.id.clone());
    Ok(Planned {
        stage: Stage {
            credential: credential.to_owned(),
            name,
            id,
            action: action.to_owned(),
            expected,
            consumer: consumer.map(|c| c.id.to_owned()),
            bind: item.bind,
            stage: "planned".into(),
            version: None,
            error: None,
            at: now(),
        },
        description: item.description.clone(),
        existing_description,
        value,
    })
}

struct Outcome {
    restart: bool,
    /// An error that would repeat for every item: code, exit code, message.
    fatal: Option<(String, i32, String)>,
}

fn set(host: &Host, journal: &mut Journal, credential: &str, stage: &str, error: Option<String>) {
    let item = stage_mut(journal, credential);
    item.stage = stage.into();
    item.error = error;
    item.at = now();
    // Failing to save the record must not stop a step that is already safe to take.
    let _ = save(host, journal);
}

fn finish(host: &Host, destination: &Destination, id: &str, word: &str) {
    if !destination.connected
        && let Ok(store) = host.state.variables.store(host)
    {
        store.audit("migrate", Some(id), word);
    }
}

fn shared_failure(reply: &Reply) -> Option<(String, i32, String)> {
    matches!(
        reply.code.as_str(),
        "dependency_or_network" | "not_ready" | "authority_denied"
    )
    .then(|| (reply.code.clone(), reply.exit_code, describe(reply)))
}

/// Take one item as far as it safely goes: write, read back, switch.
async fn process(
    host: &Arc<Host>,
    destination: &Destination,
    catalog: &[vault::Metadata],
    entry: &Planned,
    journal: &mut Journal,
) -> Outcome {
    let credential = entry.stage.credential.as_str();
    let mut outcome = Outcome {
        restart: false,
        fatal: None,
    };
    let current = stage_mut(journal, credential).clone();
    if current.stage == "complete" {
        // Repeated: confirm the entry is still there instead of trusting the record.
        let present = catalog
            .iter()
            .any(|v| v.id == current.id && Some(v.version) >= current.version);
        if present {
            return outcome;
        }
        set(host, journal, credential, "planned", None);
    }
    let current = stage_mut(journal, credential).clone();
    // An earlier write whose outcome is unknown is inspected, never blindly repeated.
    let write_needed = matches!(current.stage.as_str(), "planned" | "writing" | "uncertain");
    if write_needed {
        let seen = catalog.iter().find(|v| v.id == entry.stage.id);
        let uncertain = current.stage != "planned";
        let applied = match (entry.stage.action.as_str(), seen) {
            ("create", Some(found)) if uncertain && found.description == entry.description => {
                Some(found.version)
            }
            ("replace", Some(found)) if uncertain && Some(found.version) > entry.stage.expected => {
                Some(found.version)
            }
            ("use", Some(found)) => Some(found.version),
            _ => None,
        };
        let version = if let Some(found) = applied {
            Some(found)
        } else {
            set(host, journal, credential, "writing", None);
            let reply = match entry.stage.action.as_str() {
                "replace" => {
                    let mut args = json!({
                        "id": entry.stage.id,
                        "version": entry.stage.expected,
                        "value": entry.value.expose(),
                    });
                    if entry.existing_description.as_deref() != Some(entry.description.as_str()) {
                        args["description"] = json!(entry.description);
                    }
                    call(host, "variable_update", args).await
                }
                _ => {
                    call(
                        host,
                        "variable_create",
                        json!({
                            "id": entry.stage.id,
                            "name": entry.stage.name,
                            "description": entry.description,
                            "value": entry.value.expose(),
                        }),
                    )
                    .await
                }
            };
            if !reply.ok {
                // An unreachable coordinator may or may not have applied the write.
                let word = if reply.code == "dependency_or_network" {
                    "uncertain"
                } else {
                    "planned"
                };
                set(host, journal, credential, word, Some(describe(&reply)));
                finish(host, destination, &entry.stage.id, "failed");
                outcome.fatal = shared_failure(&reply);
                return outcome;
            }
            finish(host, destination, &entry.stage.id, "ok");
            reply.data["variable"]["version"].as_u64()
        };
        stage_mut(journal, credential).version = version;
        set(host, journal, credential, "written", None);
    }
    let current = stage_mut(journal, credential).clone();
    if matches!(current.stage.as_str(), "written" | "verification_failed") {
        // Read it back as a consumer would and compare with the original.
        match consumers::resolve_entry(host, &current.id, current.version).await {
            Ok((_, stored)) => {
                let same = security::constant_eq(stored.expose(), entry.value.expose());
                let valid = entry
                    .stage
                    .consumer
                    .as_deref()
                    .and_then(rules::built_in)
                    .is_none_or(|c| rules::validate_value(c, stored.expose()).is_ok());
                if same && valid {
                    set(host, journal, credential, "verified", None);
                } else {
                    let message = if same {
                        "The account value is not valid for its consumer."
                    } else {
                        "The account variable does not hold the original value. Choose to replace it, or use another name."
                    };
                    set(
                        host,
                        journal,
                        credential,
                        "verification_failed",
                        Some(message.into()),
                    );
                    finish(host, destination, &current.id, "rejected");
                    return outcome;
                }
            }
            Err(error) => {
                let reply = super::variables::failure(error);
                set(host, journal, credential, "written", Some(describe(&reply)));
                outcome.fatal = shared_failure(&reply);
                return outcome;
            }
        }
    }
    let current = stage_mut(journal, credential).clone();
    if current.stage == "verified" {
        if !current.bind {
            set(host, journal, credential, "complete", None);
            return outcome;
        }
        let reply = call(
            host,
            "variable_consumer_bind",
            json!({
                "consumer": current.consumer,
                "variable": current.id,
                "version": current.version,
            }),
        )
        .await;
        if reply.ok {
            outcome.restart = reply.data["restart_required"] == true;
            set(host, journal, credential, "complete", None);
            finish(host, destination, &current.id, "ok");
        } else {
            set(
                host,
                journal,
                credential,
                "verified",
                Some(describe(&reply)),
            );
            finish(host, destination, &current.id, "failed");
            outcome.fatal = shared_failure(&reply);
        }
    }
    outcome
}

/// Replace this computer's portable settings with the account's (backed up first by the same
/// operation `pta account sync --reset` uses). Folders, sign-ins and providers stay local.
async fn adopt_settings(host: &Arc<Host>, journal: &mut Journal) -> Value {
    let reply = call(host, "portable_sync", json!({"reset": true})).await;
    let settings = if reply.ok {
        SettingsStage {
            stage: "complete".into(),
            error: None,
            at: now(),
        }
    } else {
        SettingsStage {
            stage: "failed".into(),
            error: Some(describe(&reply)),
            at: now(),
        }
    };
    let view = json!({
        "stage": settings.stage,
        "error": settings.error,
        "state": reply.data["sync"]["state"],
    });
    journal.settings = Some(settings);
    view
}

#[cfg(test)]
mod tests;
