use crate::store::{
    notes::{LocalNotes, NotesStore},
    settings::{LocalSettings, SettingsStore},
};
use crate::{
    adapters::oauth::OAuth,
    config::Config,
    knowledge::{Access, KnowledgeMap},
    local_chat, runtime, security,
    security::Vault,
    simulation::{SimulationRequest, SimulationResult},
    state::Store,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, oneshot, watch};

mod activity;
mod agent;
mod authority;
mod azure_cli;
pub mod cli;
mod cloud;
mod consumers;
pub mod control;
mod deliveries;
mod diagnostics;
mod github;
mod github_cli;
mod gui_assets;
mod headless;
pub(crate) mod implementation;
mod maintenance;
mod mcp;
mod migration;
mod notes;
mod page;
mod portable;
mod readiness;
pub(crate) mod service;
mod skill;
mod standing;
mod tools;
mod variables;
mod web;
mod workflows;

/// What the process owning the host can do beyond operations: open the system browser and quit.
/// There is no window; the owner's GUI is the local page (`page.rs`) in the system browser.
pub(crate) trait Shell: Send + Sync {
    /// Open a URL in the user's browser. A host without a browser session only returns the
    /// URL to the caller.
    fn open_url(&self, url: &str) -> Result<()>;
    /// Whether `open_url` reaches a browser a person sees on this computer; the local page
    /// opens only then, and so do the desktop apps of a coding agent's session.
    fn browser(&self) -> bool {
        false
    }
    fn exit(&self);
}

/// The single owner of a profile: settings, credentials, the service and its UI shell.
pub(crate) struct Host {
    pub(crate) state: DesktopState,
    pub(crate) shell: Box<dyn Shell>,
}

struct Running {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<()>>,
    tunnel: Option<tokio::process::Child>,
    /// The service's pipeline, to improve and send unsent answers from the app.
    pipeline: Arc<crate::pipeline::Pipeline>,
}

/// Where the assistant service is in its lifecycle. `start`/`stop` and the service ending on
/// its own publish it, so every client follows changes made by any other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AssistantPhase {
    Stopped,
    Starting,
    Running,
    Stopping,
}

/// Publishes `phase` only when it changes, so followers are not woken for nothing.
fn set_phase(sender: &watch::Sender<AssistantPhase>, phase: AssistantPhase) {
    sender.send_if_modified(|current| std::mem::replace(current, phase) != phase);
}

pub(crate) struct DesktopState {
    authority: authority::Scope,
    operations: Mutex<()>,
    loaded_config: Mutex<Option<Config>>,
    settings: LocalSettings,
    running: Mutex<Option<Running>>,
    phase: watch::Sender<AssistantPhase>,
    github_login: Mutex<Option<github::Pending>>,
    github_finish: Mutex<Option<tokio::task::JoinHandle<std::result::Result<(), String>>>>,
    azure_login: Mutex<Option<azure_cli::Pending>>,
    microsoft_login: Mutex<Option<tokio::task::JoinHandle<std::result::Result<(), String>>>>,
    microsoft_redirect: Mutex<Option<String>>,
    github_api: Mutex<()>,
    activity_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    activity_graph: Mutex<Option<Arc<crate::adapters::graph::Graph>>>,
    activity_recovered: Mutex<Option<PathBuf>>,
    implementation: implementation::State,
    variables: variables::State,
    portable: portable::State,
    standing: standing::State,
    cloud: cloud::State,
    /// The owner's GUI in the system browser (`page.rs`).
    page: page::Page,
}

#[derive(Serialize)]
struct Snapshot {
    account: serde_json::Value,
    revision: String,
    loaded_config: Option<Config>,
    host_version: String,
    host_running: bool,
    /// Always true: every host runs without a window (kept for clients that read it).
    headless: bool,
    profile: PathBuf,
    tunnel_running: bool,
    microsoft_connected: bool,
    /// Whether the saved Microsoft session is known to need interactive renewal (Entra refused
    /// its refresh token): the assistant may still report running, but cannot start again until
    /// the person signs in. Read from the profile's records; asks Microsoft nothing (additive).
    microsoft_session_needs_renewal: bool,
    teams_identity_error: Option<String>,
    config: Config,
    map: KnowledgeMap,
    credentials: BTreeMap<String, String>,
    running: bool,
    active_subscriptions: usize,
    subscription_issue: Option<String>,
    github_connected: bool,
    tunnel_config: String,
    /// The built-in cases with their default texts, for editors (additive).
    cases: &'static [crate::cases::BuiltIn],
    /// The implementation agents' built-in prompts with their default instructions (additive).
    agent_prompts: &'static [crate::agent_prompts::BuiltIn],
}

#[derive(Debug)]
struct SetupError(&'static str);

impl std::fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for SetupError {}

/// Input or state the user can fix, with a fixed explanation that is safe to show as is.
#[derive(Debug)]
enum Rejected {
    Input(&'static str),
    Conflict(&'static str),
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Input(message) | Self::Conflict(message)) = self;
        formatter.write_str(message)
    }
}

impl std::error::Error for Rejected {}

#[track_caller]
fn fail<E: Into<anyhow::Error>>(error: E) -> String {
    let error = error.into();
    diagnostics::capture_error(&error, std::panic::Location::caller());
    if let Some(error) = error.downcast_ref::<azure_cli::LoginError>() {
        return format!("[not_ready] {error}");
    }
    if let Some(message) = error.downcast_ref::<SetupError>() {
        return format!("[not_ready] {}", message.0);
    }
    match error.downcast_ref::<Rejected>() {
        Some(Rejected::Input(message)) => return format!("[invalid_input] {message}"),
        Some(Rejected::Conflict(message)) => return format!("[conflict] {message}"),
        None => {}
    }
    if let Some(message) = error.downcast_ref::<crate::simulation::InvalidRequest>() {
        return format!("[invalid_input] {message}");
    }
    if let Some(refusal) = error.downcast_ref::<crate::adapters::chats::ReadRefusal>() {
        return format!("[invalid_input] {refusal}");
    }
    if let Some(refusal) = error.downcast_ref::<crate::pipeline::DraftRefusal>() {
        return match refusal {
            crate::pipeline::DraftRefusal::GuidanceTooLong
            | crate::pipeline::DraftRefusal::SensitiveGuidance
            | crate::pipeline::DraftRefusal::UnknownSource => {
                format!("[invalid_input] {refusal}")
            }
            _ => format!("[conflict] {refusal}"),
        };
    }
    if let Some(refusal) = error.downcast_ref::<crate::adapters::graph::PostRefusal>() {
        use crate::adapters::graph::PostRefusal::{Refused, Uncertain};
        return match refusal {
            Refused | Uncertain => format!("[error] {refusal}"),
            _ => format!("[invalid_input] {refusal}"),
        };
    }
    if let Some(message) = error.downcast_ref::<crate::adapters::oauth::AuthorizationRequired>() {
        return format!("[not_ready] {message}");
    }
    if error.downcast_ref::<reqwest::Error>().is_some() {
        return "[network] Dependency request failed. Check connectivity and authorization.".into();
    }
    tracing::warn!(event = "desktop_operation_failed");
    "The operation did not complete. Check the configuration and try again.".into()
}

/// The running assistant's pipeline: improving or sending an answer reads and writes Teams
/// with its session.
async fn running_pipeline(
    state: &DesktopState,
) -> std::result::Result<Arc<crate::pipeline::Pipeline>, String> {
    state
        .running
        .lock()
        .await
        .as_ref()
        .filter(|r| !r.task.is_finished())
        .map(|r| r.pipeline.clone())
        .ok_or_else(|| "[not_ready] Start the assistant to write or send an answer.".to_owned())
}

/// A job key from the GUI or CLI: its canonical form is checked again when Teams is read.
fn answer_resource(args: &serde_json::Value) -> std::result::Result<String, String> {
    match args.get("resource").and_then(serde_json::Value::as_str) {
        Some(resource)
            if !resource.is_empty()
                && resource.len() <= 512
                && !resource.chars().any(char::is_control)
                && !resource.starts_with("simulation:") =>
        {
            Ok(resource.to_owned())
        }
        _ => Err("[invalid_input] Invalid message.".into()),
    }
}

/// Write the answer to a message that was not sent one (a new one for a message left
/// unanswered, or the unsent one again) with the user's context and, optionally, the sources
/// they chose. Returns at once; the answer is written in the background and the job shows
/// `refining` until it ends.
async fn refine_answer(
    state: &DesktopState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let resource = answer_resource(args)?;
    let context = match args.get("context") {
        None | Some(serde_json::Value::Null) => "",
        Some(serde_json::Value::String(context)) => context.as_str(),
        _ => return Err("[invalid_input] Context must be text.".into()),
    };
    let sources: Vec<String> = match args.get("sources") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(ids)) if ids.len() <= 250 => ids
            .iter()
            .map(|id| id.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or("[invalid_input] Sources must be a list of source IDs.")?,
        _ => return Err("[invalid_input] Sources must be a list of source IDs.".into()),
    };
    let pipeline = running_pipeline(state).await?;
    pipeline
        .begin_refinement(&resource, context, &sources)
        .map_err(fail)?;
    tokio::spawn(async move {
        if pipeline.refine(&resource).await.is_err() {
            tracing::warn!(event = "answer_refinement_failed");
        }
    });
    Ok(serde_json::json!({"refining": true}))
}

/// Send an unsent answer once, at the user's request.
async fn send_answer(
    state: &DesktopState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let resource = answer_resource(args)?;
    let pipeline = running_pipeline(state).await?;
    let status = pipeline.send_draft(&resource).await.map_err(fail)?;
    Ok(serde_json::json!({"status": status}))
}

/// The Graph session to read or post in Teams: the running service's, else the profile's own
/// (which needs the assistant stopped, so two sessions never share the dataset).
enum TeamsGraph {
    Service(Arc<crate::adapters::graph::Graph>),
    Profile(ProfileGraph),
}
impl std::ops::Deref for TeamsGraph {
    type Target = crate::adapters::graph::Graph;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Service(graph) => graph,
            Self::Profile(graph) => graph,
        }
    }
}
async fn teams_graph(state: &DesktopState) -> Result<TeamsGraph> {
    if let Some(graph) = state.activity_graph.lock().await.clone() {
        // The account's reading scope applies to reads as soon as it changes, without a restart.
        let reading = state.settings.config().map(|c| c.teams_reading);
        if let Ok(reading) = reading
            && reading != graph.config.teams_reading
        {
            let mut config = (*graph.config).clone();
            config.teams_reading = reading;
            let mut graph = (*graph).clone();
            graph.config = Arc::new(config);
            return Ok(TeamsGraph::Service(Arc::new(graph)));
        }
        return Ok(TeamsGraph::Service(graph));
    }
    Ok(TeamsGraph::Profile(profile_graph(state).await?))
}

/// The chats the user belongs to, newest first, optionally filtered by a topic or member
/// name: `limit` of them from `offset`, with the total.
async fn teams_chats(
    state: &DesktopState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let query = args.get("query").and_then(serde_json::Value::as_str);
    let limit = teams_count(args, "limit", 50, 500)?;
    let offset = teams_count(args, "offset", 0, usize::MAX)?;
    let graph = teams_graph(state).await.map_err(fail)?;
    let mut chats = graph.chat_directory(query).await.map_err(fail)?;
    chats.sort_by(|a, b| {
        b["last_updated"]
            .as_str()
            .unwrap_or("")
            .cmp(a["last_updated"].as_str().unwrap_or(""))
    });
    let total = chats.len();
    let page: Vec<_> = chats.into_iter().skip(offset).take(limit).collect();
    let next = offset + page.len();
    Ok(
        serde_json::json!({"chats": page, "total": total, "offset": offset,
        "next_offset": (next < total).then_some(next)}),
    )
}

/// A count argument of a Teams read: `default` when absent, otherwise within `min(1)..=max`.
fn teams_count(
    args: &serde_json::Value,
    key: &str,
    default: usize,
    max: usize,
) -> std::result::Result<usize, String> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(default),
        Some(value) => value
            .as_u64()
            .map(|n| n as usize)
            .filter(|n| (usize::from(key != "offset")..=max).contains(n))
            .ok_or_else(|| format!("[invalid_input] {key} must be a whole number up to {max}.")),
    }
}

/// The window of a Teams read: `since`, `until` and the `before` cursor are days
/// (`YYYY-MM-DD`, this computer's time zone) or RFC 3339 instants; `until` includes its day.
fn teams_window(
    args: &serde_json::Value,
) -> std::result::Result<crate::adapters::chats::Window, String> {
    use crate::adapters::chats;
    let at = |key: &str, end_of_day: bool| match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => chats::instant(text.trim(), end_of_day)
            .map(Some)
            .map_err(fail),
        Some(_) => Err(format!("[invalid_input] {key} must be a date.")),
    };
    chats::Window::new(
        at("since", false)?,
        at("until", true)?,
        at("before", false)?,
    )
    .map_err(fail)
}

/// A page of the audit, newest first: `since`/`until` are days or instants and `before` a
/// cursor, the resource of the last message listed (or a day or instant).
fn audit_list(
    state: &DesktopState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    use crate::adapters::chats::instant;
    let config = state.settings.config().map_err(fail)?;
    let db = config.server.data_dir.join("assistant.db");
    let text = |key: &str| {
        args.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let at = |key: &str, end_of_day: bool| {
        text(key)
            .map(|t| instant(t, end_of_day).map(|at| at.timestamp()))
            .transpose()
            .map_err(fail)
    };
    let since = at("since", false)?;
    let mut until = at("until", true)?;
    let mut after = None;
    if let Some(before) = text("before") {
        match instant(before, false) {
            Ok(at) => until = Some(until.map_or(at.timestamp(), |u| u.min(at.timestamp()))),
            Err(_) => {
                let received = Store::received_at(&db, before)
                    .map_err(fail)?
                    .ok_or("[invalid_input] before names no message of the audit: use next_cursor, a resource from audit list, or a date.")?;
                after = Some((received, before));
            }
        }
    }
    let listing = crate::store::audit::Listing {
        resource: None,
        since,
        until,
        after,
        limit: teams_count(args, "limit", 50, 1000)?,
    };
    let content = args
        .get("content")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Store::inspect_page(&db, &listing, content).map_err(fail)
}

/// A failed Teams read, with what Graph said when it refused.
#[track_caller]
fn teams_read_failed(error: anyhow::Error) -> String {
    match crate::adapters::graph::http_status(&error) {
        Some(403 | 404) => "[invalid_input] Microsoft Graph refused the read (the chat or message does not exist or does not list you).".into(),
        Some(status) => format!("[network] Microsoft Graph answered HTTP {status}; nothing was read."),
        None => fail(error),
    }
}

/// A chat's messages, a single message (`chat` is its resource) or, with `mine`, the user's
/// own across the chats this profile reads; newest first, read-only.
async fn teams_messages(
    state: &DesktopState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    use crate::adapters::chats;
    let mine = match args.get("mine") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => value
            .as_bool()
            .ok_or("[invalid_input] mine must be true or false.")?,
    };
    let chat = args
        .get("chat")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty());
    if mine == chat.is_some() {
        return Err("[invalid_input] Name one chat (an ID from teams chats, self or a message resource), or ask for your own messages with mine.".into());
    }
    let mut window = teams_window(args)?;
    if mine {
        window = window.bounded();
    }
    let limit = teams_count(args, "limit", chats::DEFAULT_LIMIT, chats::MAX_LIMIT)?;
    let graph = teams_graph(state).await.map_err(fail)?;
    let page = match chat {
        Some(chat) if chat.starts_with("chats/") => graph.chat_message(chat).await,
        Some(chat) => graph.chat_messages(chat, window, limit).await,
        None => graph.own_chat_messages(window, limit).await,
    }
    .map_err(teams_read_failed)?;
    let mut value = serde_json::json!(page);
    value["chat"] = serde_json::json!(chat);
    value["mine"] = serde_json::json!(mine);
    value["window"] = window.describe();
    Ok(value)
}

/// Messages matching a text in the chats this profile reads; read-only.
async fn teams_search(
    state: &DesktopState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    use crate::adapters::chats;
    let query = args
        .get("query")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let chats_named = match args.get("chats") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(items)) if items.len() <= 20 => items
            .iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or("[invalid_input] chats is a list of chat IDs.")?,
        Some(_) => return Err("[invalid_input] chats is a list of at most 20 chat IDs.".into()),
    };
    let from = match args.get("from") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(name))
            if !name.trim().is_empty()
                && name.chars().count() <= 120
                && !name.chars().any(char::is_control) =>
        {
            Some(name.trim().to_owned())
        }
        Some(_) => return Err("[invalid_input] from is part of a person's name.".into()),
    };
    let window = teams_window(args)?;
    let limit = teams_count(
        args,
        "limit",
        chats::DEFAULT_SEARCH_LIMIT,
        chats::MAX_SEARCH_LIMIT,
    )?;
    let graph = teams_graph(state).await.map_err(fail)?;
    let filter = chats::SearchFilter {
        chats: chats_named,
        from,
        window,
    };
    let result = graph
        .search_messages(query, &filter, limit)
        .await
        .map_err(teams_read_failed)?;
    let mut value = serde_json::json!(result);
    value["query"] = serde_json::json!(query.trim());
    value["window"] = window.describe();
    Ok(value)
}

/// Post a message the user wrote, once, in a chat they belong to.
async fn teams_post(
    state: &DesktopState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let text = |key| {
        args.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let (chat, message) = (text("chat").trim(), text("text").trim());
    if chat.is_empty() || chat.len() > 512 || chat.chars().any(char::is_control) {
        return Err("[invalid_input] Name the chat: an ID from teams_chats, or self.".into());
    }
    if message.is_empty() || message.chars().count() > 20_000 {
        return Err("[invalid_input] The message is empty or too long.".into());
    }
    let graph = teams_graph(state).await.map_err(fail)?;
    graph.post_chat(chat, message).await.map_err(fail)
}

fn validate_teams_setup(config: &Config) -> Result<()> {
    ensure!(
        !config.server.public_url.contains("example.com"),
        SetupError("Set a real public HTTPS URL to start Teams. The test chat works without it.")
    );
    ensure!(
        !config.graph.client_id.ends_with("0002") && !config.graph.tenant_id.ends_with("0001"),
        SetupError("Set the tenant and Client ID of your existing Entra registration.")
    );
    ensure!(
        config.graph.user_id != uuid::Uuid::nil().to_string(),
        SetupError("Connect your Microsoft account before starting Teams.")
    );
    Ok(())
}

pub(crate) fn read_config(path: &Path) -> Result<Config> {
    Ok(toml::from_str(&fs::read_to_string(path)?)?)
}

pub(crate) fn write_private(path: &Path, content: &str) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let written =
        write_temporary(&temp, content).and_then(|()| security::replace_private_file(&temp, path));
    if written.is_err() {
        // A write that failed halfway leaves no stray file beside the one it meant to replace.
        let _ = fs::remove_file(&temp);
    }
    written
}

fn write_temporary(temp: &Path, content: &str) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(temp)?;
    security::protect_file(temp)?;
    {
        use std::io::Write;
        file.write_all(content.as_bytes())?;
    }
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn validate_local(config: &Config, map: &KnowledgeMap) -> Result<()> {
    config.validate()?;
    let bind: std::net::SocketAddr = config.server.bind.parse()?;
    ensure!(
        bind.ip().is_loopback(),
        "desktop server must bind to loopback"
    );
    KnowledgeMap::parse(&toml::to_string(map)?)?;
    crate::activity::validate_sources(&config.activity_registration, map)?;
    for path in map.repositories.values() {
        let root = fs::canonicalize(path)?;
        ensure!(
            root.join(".git").exists(),
            "repository is not a Git checkout"
        );
    }
    for resource in &map.resources {
        if let Access::File { repository, path } = &resource.access {
            crate::knowledge::read_repository_file(&map.repositories, repository, path)?;
        }
    }
    Ok(())
}

/// The example registration, the desktop template and a user not verified yet are nobody's
/// identity.
fn placeholder_identity(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.as_u128() <= 3)
}

/// The Microsoft session is the only thing a Teams database seals with the state key and ties
/// to a registration. One that cannot be read counts as holding a session.
fn holds_session(config: &Config) -> bool {
    let db = config.server.data_dir.join("assistant.db");
    db.exists() && Store::has_token(&db).unwrap_or(true)
}

/// A Teams database stays with the organization, client and user it was opened for. One opened
/// under the example registration (a first sign-in attempt creates it) belongs to nobody while
/// it holds no Microsoft session, so the real registration may still replace the example.
fn protect_identity(previous: &Config, next: &Config) -> Result<()> {
    let (before, after) = (&previous.graph, &next.graph);
    let differs = |before: &str, after: &str| before != after;
    let replaced = |before: &str, after: &str| before != after && !placeholder_identity(before);
    let changed = replaced(&before.tenant_id, &after.tenant_id)
        || replaced(&before.client_id, &after.client_id)
        || replaced(&before.user_id, &after.user_id);
    let registered = differs(&before.tenant_id, &after.tenant_id)
        || differs(&before.client_id, &after.client_id);
    let db = previous.server.data_dir.join("assistant.db");
    ensure!(
        !(db.exists() && changed) && !(registered && holds_session(previous)),
        "cannot reuse a Teams database with another organization, client, or user"
    );
    Ok(())
}

#[derive(Deserialize)]
struct CloudflareConfig {
    tunnel: String,
    #[serde(rename = "credentials-file")]
    credentials_file: PathBuf,
    ingress: Vec<CloudflareIngress>,
}

#[derive(Deserialize)]
struct CloudflareIngress {
    hostname: Option<String>,
    service: String,
}

fn validate_tunnel(path: &Path, config: &Config) -> Result<()> {
    let file = fs::read_to_string(path)?;
    ensure!(file.len() <= 128_000, "tunnel config is too large");
    let tunnel: CloudflareConfig = serde_yaml::from_str(&file)?;
    ensure!(!tunnel.tunnel.trim().is_empty(), "tunnel ID is missing");
    let credentials_file = if tunnel.credentials_file.is_absolute() {
        tunnel.credentials_file
    } else {
        path.parent()
            .context("invalid tunnel config path")?
            .join(tunnel.credentials_file)
    };
    ensure!(
        credentials_file.is_file(),
        "tunnel credentials file is missing"
    );
    let public = url::Url::parse(&config.server.public_url)?;
    let host = public.host_str().context("public hostname is missing")?;
    ensure!(
        tunnel.ingress.len() == 2,
        "use one hostname and a 404 catch-all"
    );
    let first = &tunnel.ingress[0];
    ensure!(
        first.hostname.as_deref() == Some(host),
        "tunnel hostname differs from Teams URL"
    );
    let expected = format!("http://{}", config.server.bind);
    let alternate = expected.replace("127.0.0.1", "localhost");
    ensure!(
        first.service == expected || first.service == alternate,
        "tunnel origin differs from local listener"
    );
    ensure!(
        tunnel.ingress[1].hostname.is_none() && tunnel.ingress[1].service == "http_status:404",
        "tunnel catch-all must return 404"
    );
    Ok(())
}

fn validate_tunnel_mode(config: &Config, tunnel_config: &str) -> Result<()> {
    ensure!(
        !config.server.cloudflare_tunnel || tunnel_config.trim().is_empty(),
        SetupError(
            "Choose the Cloudflare tunnel with a token or the local configuration file; set up only one."
        )
    );
    Ok(())
}

fn cloudflared_command(
    binary: &Path,
    config_path: Option<&Path>,
    token: Option<&str>,
) -> tokio::process::Command {
    let mut command = security::program_async(binary);
    command.kill_on_drop(true);
    command.arg("tunnel").arg("--no-autoupdate");
    if let Some(path) = config_path {
        command.arg("--config").arg(path);
    }
    command.arg("run");
    command
        .env_remove("TUNNEL_TOKEN")
        .env_remove("TUNNEL_TOKEN_FILE");
    if let Some(token) = token {
        command.env("TUNNEL_TOKEN", token);
    }
    command
}

/// Credentials resolve from `NAME_FILE`, `NAME`, then the OS keychain (macOS/Windows) or the
/// private file store `<profile>/credentials/default/NAME` (Linux).
pub(crate) fn configure_credentials(config_dir: &Path) -> Result<()> {
    security::keyring_profile(Some("default"))?;
    security::file_credential_store(Some(&config_dir.join("credentials")));
    security::prefer_file_credentials(control::isolated_profile_dir()?.is_some());
    security::account_only_credentials(
        web::account_scope(config_dir)
            .map(|scope| scope.shared && !scope.owner)
            .unwrap_or_else(|_| {
                config_dir
                    .canonicalize()
                    .unwrap_or_else(|_| config_dir.to_path_buf())
                    .parent()
                    .is_some_and(|p| {
                        p.file_name().is_some_and(|n| n == "profiles")
                            && p.parent()
                                .is_some_and(|p| p.file_name().is_some_and(|n| n == "web"))
                    })
            }),
    );
    Ok(())
}

pub(crate) fn init_state(config_dir: &Path, data_dir: &Path) -> Result<DesktopState> {
    let (config_dir, data_dir) = (config_dir.to_path_buf(), data_dir.to_path_buf());
    security::private_dir(&config_dir)?;
    security::private_dir(&data_dir)?;
    let settings = LocalSettings::open(&config_dir, &data_dir)?;
    Ok(DesktopState {
        authority: web::account_scope(&config_dir)?,
        implementation: implementation::State::new(&config_dir)?,
        variables: variables::State::new(&config_dir),
        portable: portable::State::new(&config_dir),
        standing: standing::State::default(),
        cloud: cloud::State::default(),
        page: page::Page::default(),
        operations: Mutex::new(()),
        loaded_config: Mutex::new(None),
        settings,
        running: Mutex::new(None),
        phase: watch::Sender::new(AssistantPhase::Stopped),
        github_login: Mutex::new(None),
        github_finish: Mutex::new(None),
        azure_login: Mutex::new(None),
        microsoft_login: Mutex::new(None),
        microsoft_redirect: Mutex::new(None),
        github_api: Mutex::new(()),
        activity_task: Mutex::new(None),
        activity_graph: Mutex::new(None),
        activity_recovered: Mutex::new(None),
    })
}

/// Credentials earlier versions required and nothing reads any more. A stored one is listed as
/// `retired` so it can be removed; it is never deleted automatically and cannot be set.
const RETIRED_CREDENTIALS: [&str; 2] = ["TYPESAFE_API_KEY", "AZURE_IMPLEMENTATION_WRITE_TOKEN"];

fn credential_names(config: &Config) -> Vec<String> {
    let mut names = vec![
        "DEEPSEEK_API_KEY".into(),
        "GRAPH_WEBHOOK_SECRET".into(),
        "STATE_ENCRYPTION_KEY".into(),
        "PTA_MACHINE_TOKEN".into(),
    ];
    names.extend(cloud::CREDENTIALS.map(String::from));
    if config.server.cloudflare_tunnel {
        names.push("CLOUDFLARE_TUNNEL_TOKEN".into());
    }
    names.extend(config.secrets.values().cloned());
    names.sort();
    names.dedup();
    names
}

/// Credentials the service needs to start. `DEEPSEEK_API_KEY` stays settable but is required
/// only while DeepSeek is in the LLM chain; Codex and Claude use their own CLI login.
fn required_credentials(config: &Config) -> Vec<String> {
    let mut names = credential_names(config);
    names
        .retain(|name| name != "PTA_MACHINE_TOKEN" && !cloud::CREDENTIALS.contains(&name.as_str()));
    if !crate::llm::uses_deepseek(&config.llm) {
        names.retain(|name| name != "DEEPSEEK_API_KEY");
    }
    names
}

async fn snapshot(host: &Host) -> std::result::Result<Snapshot, String> {
    let teams_identity_error = variables::identity::restore(host).await.err();
    let state = &host.state;
    let config = state.settings.config().map_err(fail)?;
    let map = state.settings.map().map_err(fail)?;
    let (active_subscriptions, subscription_issue) =
        Store::subscription_health(&config.server.data_dir.join("assistant.db")).map_err(fail)?;
    let mut credentials = BTreeMap::new();
    let required = required_credentials(&config);
    for name in credential_names(&config) {
        let source = security::secret_source(&name).map_err(fail)?;
        let missing = if required.contains(&name) {
            "missing"
        } else {
            "unused"
        };
        credentials.insert(name, source.unwrap_or(missing).into());
    }
    for name in RETIRED_CREDENTIALS {
        if security::secret_source(name).map_err(fail)?.is_some() {
            credentials.insert(name.into(), "retired".into());
        }
    }
    let mut running = state.running.lock().await;
    if running.as_mut().is_some_and(|r| {
        r.tunnel
            .as_mut()
            .is_some_and(|t| t.try_wait().ok().flatten().is_some())
    }) && let Some(failed) = running.take()
    {
        let _ = failed.stop.send(true);
        let _ = failed.task.await;
    }
    if running.as_ref().is_some_and(|r| r.task.is_finished())
        && let Some(finished) = running.take()
    {
        let _ = finished.task.await;
    }
    Ok(Snapshot {
        teams_identity_error,
        account: web::account_identity(&state.authority).map_err(fail)?,
        revision: control::revision(state).map_err(fail)?,
        loaded_config: state
            .loaded_config
            .lock()
            .await
            .clone()
            .filter(|_| running.is_some()),
        host_version: env!("CARGO_PKG_VERSION").into(),
        host_running: true,
        headless: true,
        profile: state.settings.profile().to_path_buf(),
        tunnel_running: running.as_ref().is_some_and(|r| r.tunnel.is_some()),
        microsoft_connected: Store::has_token(&config.server.data_dir.join("assistant.db"))
            .map_err(fail)?,
        microsoft_session_needs_renewal: Store::session_needs_renewal(
            &config.server.data_dir.join("assistant.db"),
        )
        .map_err(fail)?,
        config,
        map,
        credentials,
        running: running.is_some(),
        active_subscriptions,
        subscription_issue,
        github_connected: security::secret_source("GITHUB_OAUTH_TOKENS")
            .map_err(fail)?
            .is_some(),
        tunnel_config: state.settings.tunnel().unwrap_or_default(),
        cases: &crate::cases::BUILT_IN,
        agent_prompts: &crate::agent_prompts::BUILT_IN,
    })
}

async fn stop(state: &DesktopState) -> Result<()> {
    if let Some(mut running) = state.running.lock().await.take() {
        set_phase(&state.phase, AssistantPhase::Stopping);
        if let Some(mut tunnel) = running.tunnel.take() {
            let _ = tunnel.kill().await;
        }
        let _ = running.stop.send(true);
        if tokio::time::timeout(std::time::Duration::from_secs(20), &mut running.task)
            .await
            .is_err()
        {
            running.task.abort();
            let _ = running.task.await;
        }
    }
    *state.loaded_config.lock().await = None;
    set_phase(&state.phase, AssistantPhase::Stopped);
    Ok(())
}

async fn start(state: &DesktopState) -> Result<()> {
    // Machine service controls retain their existing Teams lease; arbitrary execution does not.
    ensure!(
        authority::invocation(state)?.scope.admitted,
        "{}",
        authority::EXECUTION_DENIED
    );
    // Starting an assistant that already runs changes nothing, so it does not flash `starting`.
    if *state.phase.borrow() == AssistantPhase::Stopped {
        set_phase(&state.phase, AssistantPhase::Starting);
    }
    let result = start_service(state).await;
    let running = state
        .running
        .lock()
        .await
        .as_ref()
        .is_some_and(|r| !r.task.is_finished());
    set_phase(
        &state.phase,
        if running {
            AssistantPhase::Running
        } else {
            AssistantPhase::Stopped
        },
    );
    result
}

async fn service_ready(
    task: &mut tokio::task::JoinHandle<Result<()>>,
    ready_rx: oneshot::Receiver<runtime::Service>,
    stop: &watch::Sender<bool>,
) -> Result<runtime::Service> {
    match tokio::time::timeout(std::time::Duration::from_secs(60), ready_rx).await {
        Ok(Ok(service)) => Ok(service),
        Ok(Err(_)) => {
            // The service failed before readiness; retain its sanitized error category.
            let _ = stop.send(true);
            task.await??;
            anyhow::bail!("assistant failed to become ready")
        }
        Err(_) => {
            let _ = stop.send(true);
            task.abort();
            let _ = task.await;
            anyhow::bail!("assistant failed to become ready")
        }
    }
}

async fn start_service(state: &DesktopState) -> Result<()> {
    ensure!(
        implementation::fleet::teams_allowed(state).await?,
        "This machine is not the selected Teams machine, or the previous machine still holds its lease."
    );
    consumers::stage(state).await;
    let config = state.settings.config()?;
    web::validate_profile_identity(state.settings.profile(), &config)?;
    let map = state.settings.map()?;
    validate_local(&config, &map)?;
    validate_teams_setup(&config)?;
    for name in ["GRAPH_WEBHOOK_SECRET", "STATE_ENCRYPTION_KEY"] {
        if security::secret_source(name)?.is_none() {
            // A database without a session has nothing sealed with the missing key.
            ensure!(
                name != "STATE_ENCRYPTION_KEY" || !holds_session(&config),
                "import the existing state key before opening the database"
            );
            security::put_desktop_secret("default", name, &security::random_secret())?;
        }
    }
    for name in required_credentials(&config) {
        security::secret(&name)?;
    }
    let tunnel_path = state.settings.tunnel()?;
    let tunnel_config_path = if tunnel_path.trim().is_empty() {
        None
    } else {
        let path = fs::canonicalize(tunnel_path.trim())?;
        validate_tunnel(&path, &config)?;
        Some(path)
    };
    validate_tunnel_mode(&config, &tunnel_path)?;
    let tunnel_token = if config.server.cloudflare_tunnel {
        Some(security::secret("CLOUDFLARE_TUNNEL_TOKEN")?)
    } else {
        None
    };
    let mut running = state.running.lock().await;
    if running.as_ref().is_some_and(|r| r.task.is_finished())
        && let Some(finished) = running.take()
    {
        finished.task.await??;
    }
    if running.is_some() {
        return Ok(());
    }
    let (stop, receiver) = watch::channel(false);
    let path = state.settings.config_path().to_string_lossy().into_owned();
    let (ready_tx, ready_rx) = oneshot::channel();
    let phase = state.phase.clone();
    let mut task = tokio::spawn(async move {
        let result = runtime::serve(&path, receiver, ready_tx).await;
        // Also when the service ends on its own (an error), not only through `stop`.
        set_phase(&phase, AssistantPhase::Stopped);
        result
    });
    let service = service_ready(&mut task, ready_rx, &stop).await?;
    let tunnel_result = async {
        if tunnel_config_path.is_none() && tunnel_token.is_none() {
            return Ok(None);
        }
        let binary = if cfg!(target_os = "macos")
            && Path::new("/opt/homebrew/bin/cloudflared").exists()
        {
            PathBuf::from("/opt/homebrew/bin/cloudflared")
        } else if cfg!(target_os = "macos") && Path::new("/usr/local/bin/cloudflared").exists() {
            PathBuf::from("/usr/local/bin/cloudflared")
        } else {
            PathBuf::from("cloudflared")
        };
        let mut child = cloudflared_command(
            &binary,
            tunnel_config_path.as_deref(),
            tunnel_token.as_deref(),
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        ensure!(
            child.try_wait()?.is_none(),
            "cloudflared exited during startup"
        );
        Ok::<_, anyhow::Error>(Some(child))
    }
    .await;
    let tunnel = match tunnel_result {
        Ok(tunnel) => tunnel,
        Err(error) => {
            let _ = stop.send(true);
            let _ = task.await;
            return Err(error);
        }
    };
    *state.loaded_config.lock().await = Some(config);
    *state.activity_graph.lock().await = Some(service.graph);
    *running = Some(Running {
        stop,
        task,
        tunnel,
        pipeline: service.pipeline,
    });
    Ok(())
}

async fn save_settings(
    host: &Arc<Host>,
    config: Config,
    mut map: KnowledgeMap,
    tunnel_config: String,
) -> std::result::Result<(), String> {
    // Workflows, their blocks and their settings change only through their own operations,
    // which check them against the machine, the catalog, the process and the caller's
    // authority: editors keep the stored ones.
    let stored = host.state.settings.map().map_err(fail)?;
    map.workflows = stored.workflows;
    map.workflow_blocks = stored.workflow_blocks;
    map.workflow_settings = stored.workflow_settings;
    commit_settings(host, config, map, tunnel_config).await
}

/// The settings commit behind `save_settings`, for the operations that checked their own part.
async fn commit_settings(
    host: &Arc<Host>,
    mut config: Config,
    mut map: KnowledgeMap,
    tunnel_config: String,
) -> std::result::Result<(), String> {
    let state = &host.state;
    let previous = state.settings.config().map_err(fail)?;
    config.knowledge_map = state.settings.map_path().to_path_buf();
    // The account store link changes only through `cloud_connect`: editors keep the stored one.
    config.cloud = previous.cloud.clone();
    // The map's own rules say what to fix (a duplicate context, an unknown source); they never
    // carry secrets.
    toml::to_string(&map)
        .map_err(anyhow::Error::from)
        .and_then(|text| KnowledgeMap::parse(&text))
        .map_err(|e| format!("[invalid_input] The knowledge map is invalid: {e}."))?;
    validate_local(&config, &map)
        .map_err(|_| "[invalid_input] Configuration or knowledge map is invalid.".to_string())?;
    protect_identity(&previous, &config)
        .map_err(|_| "[conflict] The existing database belongs to another identity.".to_string())?;
    if config.server.data_dir != previous.server.data_dir {
        return Err("Use config import while stopped to change the data directory.".into());
    }
    validate_tunnel_mode(&config, &tunnel_config).map_err(fail)?;
    if !tunnel_config.trim().is_empty() {
        validate_tunnel(Path::new(tunnel_config.trim()), &config).map_err(fail)?;
    }
    // An installation signed in to an account first sends the portable part of the change to the
    // account's coordinator, which decides it under optimistic concurrency.
    let previous_map = state.settings.map().map_err(fail)?;
    crate::organization::stamp(
        &previous_map.organization,
        &mut map.organization,
        chrono::Utc::now().timestamp(),
    );
    let forwarded =
        portable::before_save(state, (&previous, &previous_map), (&config, &map)).await?;
    // With the account store as the source of truth, it accepts the configuration first.
    let accepted = cloud::primary::before_save(host, &config, &map).await?;
    variables::identity::save(host, &previous, &config).await?;
    let was_running = state.running.lock().await.is_some();
    if was_running {
        stop(state).await.map_err(fail)?;
    }
    state
        .settings
        .commit(&config, &map, tunnel_config.trim())
        .map_err(fail)?;
    if let Some(accepted) = accepted {
        accepted.settle();
    }
    portable::after_save(state, forwarded);
    forget_removed_sources(&config, &map);
    *state.activity_graph.lock().await = None;
    if was_running {
        start(state).await.map_err(fail)?;
    }
    Ok(())
}

/// The knowledge index keeps no documents of repository sources that left the map, and holds
/// the map's current questions and organization.
fn forget_removed_sources(config: &Config, map: &KnowledgeMap) {
    let path = config.server.data_dir.join(crate::index::FILE);
    if !path.exists() {
        return;
    }
    let kept: std::collections::BTreeSet<&str> = map
        .resources
        .iter()
        .filter(|r| matches!(r.access, Access::Repository { .. }))
        .map(|r| r.id.as_str())
        .collect();
    let forgotten = crate::index::Index::open(&path).and_then(|index| {
        if let Some(questions) = &map.questions {
            let ids: Vec<&str> = questions.iter().map(|q| q.id.as_str()).collect();
            LocalNotes::new(&index).retain_evaluations(&ids)?;
            index.sync_questions(questions)?;
        }
        index.sync_organization(&map.organization, &map.work_item_guides)?;
        for source in index.status()?.into_keys() {
            if !source.starts_with('@') && !kept.contains(source.as_str()) {
                index.forget(&source)?;
            }
        }
        Ok(())
    });
    if forgotten.is_err() {
        tracing::warn!(event = "knowledge_index_cleanup_failed");
    }
}

async fn import_existing(state: &DesktopState, path: String) -> std::result::Result<(), String> {
    let source = PathBuf::from(path);
    let source_dir = source.parent().ok_or_else(|| "Invalid path".to_string())?;
    let mut config: Config =
        toml::from_str(&fs::read_to_string(&source).map_err(fail)?).map_err(fail)?;
    if config.knowledge_map.is_relative() {
        config.knowledge_map = source_dir.join(&config.knowledge_map);
    }
    if config.server.data_dir.is_relative() {
        config.server.data_dir = source_dir.join(&config.server.data_dir);
    }
    let map_dir = config
        .knowledge_map
        .parent()
        .context("invalid map path")
        .map_err(fail)?;
    let mut map = KnowledgeMap::load(&config.knowledge_map).map_err(fail)?;
    for path in map.repositories.values_mut() {
        if path.is_relative() {
            *path = map_dir.join(&path);
        }
    }
    config.knowledge_map = state.settings.map_path().to_path_buf();
    validate_local(&config, &map)
        .map_err(|_| "[invalid_input] Configuration or knowledge map is invalid.".to_string())?;
    let previous = state.settings.config().map_err(fail)?;
    if previous.server.data_dir == config.server.data_dir {
        protect_identity(&previous, &config).map_err(|_| {
            "[conflict] The existing database belongs to another identity.".to_string()
        })?;
    }
    ensure_stopped(state)
        .await
        .map_err(|_| "[conflict] Stop the service before this operation.".to_string())?;
    let tunnel = state.settings.tunnel().unwrap_or_default();
    validate_tunnel_mode(&config, &tunnel).map_err(fail)?;
    state
        .settings
        .commit(&config, &map, &tunnel)
        .map_err(fail)?;
    *state.activity_graph.lock().await = None;
    Ok(())
}

async fn ensure_stopped(state: &DesktopState) -> Result<()> {
    ensure!(
        state.running.lock().await.is_none(),
        "stop the assistant before importing"
    );
    Ok(())
}

async fn set_credential(
    host: &Arc<Host>,
    name: String,
    value: String,
) -> std::result::Result<(), String> {
    let state = &host.state;
    let config = state.settings.config().map_err(fail)?;
    if !credential_names(&config).contains(&name) {
        return Err("[invalid_input] Credential name not allowed".into());
    }
    if variables::credentials::managed(&config, &name) {
        return variables::credentials::set(host, name, value).await;
    }
    if name == "STATE_ENCRYPTION_KEY" {
        validate_state_key(&config, &value).map_err(|_| {
            "[conflict] The existing database requires its original encryption key.".to_string()
        })?;
    }
    ensure_stopped(state)
        .await
        .map_err(|_| "[conflict] Stop the service before this operation.".to_string())?;
    security::put_desktop_secret("default", &name, &value).map_err(fail)
}

async fn delete_credential(host: &Arc<Host>, name: String) -> std::result::Result<(), String> {
    let state = &host.state;
    let config = state.settings.config().map_err(fail)?;
    if variables::credentials::managed(&config, &name) {
        let entry = if let Some(link) = implementation::link(state).map_err(fail)? {
            variables::cached(host, &link)
        } else {
            let store = state.variables.store(host).map_err(fail)?;
            variables::credentials::migrate(host, &store).map_err(fail)?;
            store.list(None).map_err(fail)?
        }
        .into_iter()
        .find(|v| v.name.eq_ignore_ascii_case(&name))
        .ok_or_else(|| "[invalid_input] There is no stored variable with that name.".to_owned())?;
        let mut request = control::Request::new("variable_delete");
        request.args = serde_json::json!({"id":entry.id,"version":entry.version});
        let reply = variables::operate(host, &request).await;
        return if reply.ok {
            Ok(())
        } else {
            Err(reply.message.unwrap_or_default())
        };
    }
    if !credential_names(&config).contains(&name) && !RETIRED_CREDENTIALS.contains(&name.as_str()) {
        return Err("[invalid_input] Credential name not allowed".into());
    }
    if name == "STATE_ENCRYPTION_KEY" && config.server.data_dir.join("assistant.db").exists() {
        return Err("The key protects an existing database and cannot be removed.".into());
    }
    ensure_stopped(state)
        .await
        .map_err(|_| "[conflict] Stop the service before this operation.".to_string())?;
    security::delete_desktop_secret("default", &name).map_err(fail)
}

async fn chat(
    state: &DesktopState,
    input: SimulationRequest,
) -> std::result::Result<SimulationResult, String> {
    consumers::stage(state).await;
    let teams = simulation_teams(state, &input).await;
    // Every provider call carries its own deadline; maximum-effort reasoning takes minutes.
    local_chat::chat(state.settings.config_path(), input, teams)
        .await
        .map_err(fail)
}

/// The Microsoft session a simulation reads the user's own Teams messages with, when one of
/// the sources it may read is that source: the running assistant's, or a token the profile's
/// session gives for this simulation. Otherwise why there is none.
async fn simulation_teams(state: &DesktopState, input: &SimulationRequest) -> local_chat::Teams {
    use local_chat::Teams;
    let wanted = state.settings.map().is_ok_and(|map| {
        map.resources.iter().any(|r| {
            r.enabled
                && r.external_processing
                && matches!(
                    &r.access,
                    Access::Tool {
                        tool: crate::tools::ToolSpec::TeamsMessages {}
                    }
                )
                && (input.sources.is_empty() || input.sources.contains(&r.id))
        })
    });
    if !wanted {
        return Teams::Unused;
    }
    if let Some(graph) = state.activity_graph.lock().await.clone() {
        return Teams::Session(graph.token.clone());
    }
    let signed_in = state.settings.config().is_ok_and(|config| {
        Store::has_token(&config.server.data_dir.join("assistant.db")).unwrap_or(false)
    });
    if !signed_in {
        return Teams::Missing(
            "this profile has no Microsoft session; sign in with pta auth microsoft login",
        );
    }
    let graph = match profile_graph(state).await {
        Ok(graph) => graph,
        Err(_) => {
            return Teams::Missing(
                "the profile's Microsoft session is in use by another process; try again",
            );
        }
    };
    match graph.token.access_token().await {
        Ok(token) => Teams::Session(Arc::new(local_chat::FixedToken(token))),
        Err(error)
            if error
                .downcast_ref::<crate::adapters::oauth::AuthorizationRequired>()
                .is_some() =>
        {
            Teams::Missing(
                "the Microsoft session needs renewal; run pta auth microsoft login, then finish",
            )
        }
        Err(_) => Teams::Missing(
            "the Microsoft session could not be renewed; check pta test connectivity",
        ),
    }
}

async fn begin_github_login(
    state: &DesktopState,
    client_id: String,
) -> std::result::Result<github::DevicePrompt, String> {
    let (pending, prompt) = github::begin(client_id).await.map_err(fail)?;
    *state.github_login.lock().await = Some(pending);
    Ok(prompt)
}

async fn github_repositories(
    state: &DesktopState,
) -> std::result::Result<Vec<github::Repository>, String> {
    let _guard = state.github_api.lock().await;
    github::repositories().await.map_err(fail)
}

async fn disconnect_github() -> std::result::Result<(), String> {
    security::delete_desktop_secret("default", "GITHUB_OAUTH_TOKENS").map_err(fail)
}

async fn clone_github_repository(
    state: &DesktopState,
    full_name: String,
    alias: String,
) -> std::result::Result<(), String> {
    let _guard = state.github_api.lock().await;
    let config = state.settings.config().map_err(fail)?;
    let mut map = state.settings.map().map_err(fail)?;
    if map.repositories.contains_key(&alias) {
        return Err("That alias already exists.".into());
    }
    let path = github::clone_repository(&full_name, &alias, &config.server.data_dir)
        .await
        .map_err(fail)?;
    map.repositories.insert(alias, path);
    validate_local(&config, &map)
        .map_err(|_| "[invalid_input] Configuration or knowledge map is invalid.".to_string())?;
    state.settings.save_map(&map).map_err(fail)?;
    if state.running.lock().await.is_some() {
        stop(state).await.map_err(fail)?;
        start(state).await.map_err(fail)?;
    }
    Ok(())
}

async fn update_github_repository(
    state: &DesktopState,
    alias: String,
) -> std::result::Result<(), String> {
    let _guard = state.github_api.lock().await;
    let config = state.settings.config().map_err(fail)?;
    let map = state.settings.map().map_err(fail)?;
    let path = map
        .repositories
        .get(&alias)
        .ok_or_else(|| "Repositorio desconocido.".to_string())?;
    github::update_repository(&alias, path, &config.server.data_dir)
        .await
        .map_err(fail)?;
    if state.running.lock().await.is_some() {
        stop(state).await.map_err(fail)?;
        start(state).await.map_err(fail)?;
    }
    Ok(())
}

struct LoginState {
    host: Arc<Host>,
    oauth: Arc<OAuth>,
    csrf: String,
    result: Mutex<Option<oneshot::Sender<Option<String>>>>,
}

async fn desktop_callback(
    State(state): State<Arc<LoginState>>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, &'static str) {
    let Some(csrf) = query.get("state") else {
        return (StatusCode::BAD_REQUEST, "Invalid response from Microsoft.");
    };
    if !security::constant_eq(csrf, &state.csrf) {
        return (StatusCode::BAD_REQUEST, "Invalid response from Microsoft.");
    }
    let result = if let Some(code) = query.get("code") {
        state
            .oauth
            .complete_desktop_checked(csrf, code, || authority::require_account(&state.host.state))
            .await
            .ok()
    } else {
        None
    };
    let success = result.is_some();
    if let Some(sender) = state.result.lock().await.take() {
        let _ = sender.send(result);
    }
    if success {
        (
            StatusCode::OK,
            "Account connected. You can close this window.",
        )
    } else {
        (
            StatusCode::BAD_REQUEST,
            "The account could not be connected. Go back to the app.",
        )
    }
}

async fn begin_microsoft(host: &Arc<Host>, open: bool) -> std::result::Result<String, String> {
    variables::identity::restore(host).await?;
    let state = &host.state;
    if state
        .microsoft_login
        .lock()
        .await
        .as_ref()
        .is_some_and(|task| !task.is_finished())
    {
        return Err("Microsoft login already pending.".into());
    }
    let was_running = state.running.lock().await.is_some();
    let config = state.settings.config().map_err(fail)?;
    config.validate().map_err(fail)?;
    if security::secret_source("STATE_ENCRYPTION_KEY")
        .map_err(fail)?
        .is_none()
    {
        // A database without a session has nothing sealed with the missing key.
        if holds_session(&config) {
            return Err("Import the existing encryption key first.".into());
        }
        security::put_desktop_secret(
            "default",
            "STATE_ENCRYPTION_KEY",
            &security::random_secret(),
        )
        .map_err(fail)?;
    }
    stop(state).await.map_err(fail)?;
    *state.activity_graph.lock().await = None;
    security::private_dir(&config.server.data_dir).map_err(fail)?;
    let dataset_lock = claim_dataset(&config.server.data_dir).map_err(fail)?;
    let store = Arc::new(Store::open(&config.server.data_dir.join("assistant.db")).map_err(fail)?);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(fail)?;
    let oauth = Arc::new(
        OAuth::new(
            Arc::new(config.clone()),
            client,
            store,
            Vault::new(&security::secret("STATE_ENCRYPTION_KEY").map_err(fail)?).map_err(fail)?,
        )
        .map_err(fail)?,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(fail)?;
    let redirect = format!(
        "http://localhost:{}/",
        listener.local_addr().map_err(fail)?.port()
    );
    let (url, csrf) = oauth.begin_desktop(&redirect).await.map_err(fail)?;
    let (sender, receiver) = oneshot::channel();
    let router = Router::new()
        .route("/", get(desktop_callback))
        .with_state(Arc::new(LoginState {
            host: host.clone(),
            oauth,
            csrf,
            result: Mutex::new(Some(sender)),
        }));
    if open {
        host.shell.open_url(&url).map_err(fail)?;
    }
    *state.microsoft_redirect.lock().await = Some(redirect);
    let host = host.clone();
    *state.microsoft_login.lock().await = Some(tokio::spawn(async move {
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        struct Abort(tokio::task::JoinHandle<()>);
        impl Drop for Abort {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _server = Abort(server);
        let result = tokio::time::timeout(std::time::Duration::from_secs(600), receiver).await;
        let state = &host.state;
        *state.microsoft_redirect.lock().await = None;
        let _guard = state.operations.lock().await;
        authority::require_account(state).map_err(fail)?;
        let user = result
            .map_err(|_| "Microsoft authorization timed out.".to_string())?
            .map_err(fail)?
            .ok_or_else(|| "Microsoft authorization failed.".to_string())?;
        let before = state.settings.config().map_err(fail)?;
        let mut next = before.clone();
        next.graph.user_id = user;
        variables::identity::save(&host, &before, &next).await?;
        state.settings.save_config(&next).map_err(fail)?;
        drop(dataset_lock);
        if was_running {
            start(state).await.map_err(fail)?;
        }
        Ok(())
    }));
    Ok(url)
}

/// Complete a login whose browser ran on another machine: the user pastes the final
/// `http://localhost:PORT/?code=…&state=…` URL and the host replays it to its own listener.
async fn forward_microsoft_redirect(
    state: &DesktopState,
    pasted: &str,
) -> std::result::Result<(), String> {
    let invalid = || "[invalid_input] Paste the complete http://localhost redirect URL.".to_owned();
    let expected = state
        .microsoft_redirect
        .lock()
        .await
        .clone()
        .ok_or_else(|| "[not_ready] No Microsoft authorization pending.".to_owned())?;
    let expected = url::Url::parse(&expected).map_err(|_| invalid())?;
    let pasted = url::Url::parse(pasted).map_err(|_| invalid())?;
    if pasted.scheme() != "http"
        || pasted.host_str() != Some("localhost")
        || pasted.port() != expected.port()
        || pasted.path() != "/"
        || pasted.query().is_none()
    {
        return Err(invalid());
    }
    let mut target = expected;
    target.set_host(Some("127.0.0.1")).map_err(|_| invalid())?;
    target.set_query(pasted.query());
    let response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(fail)?
        .get(target)
        .send()
        .await
        .map_err(fail)?;
    // The loopback callback answers 200 only after storing the verified account's tokens.
    if !response.status().is_success() {
        return Err(
            "[invalid_input] The pasted redirect was rejected or is stale; start login again."
                .into(),
        );
    }
    Ok(())
}

struct ProfileGraph {
    graph: crate::adapters::graph::Graph,
    _lock: fs::File,
}
impl std::ops::Deref for ProfileGraph {
    type Target = crate::adapters::graph::Graph;
    fn deref(&self) -> &Self::Target {
        &self.graph
    }
}
fn claim_dataset(path: &Path) -> Result<fs::File> {
    use fs2::FileExt;
    security::private_dir(path)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.join("instance.lock"))?;
    lock.try_lock_exclusive()
        .context("another service owns this dataset")?;
    Ok(lock)
}
async fn profile_graph(state: &DesktopState) -> Result<ProfileGraph> {
    ensure_stopped(state).await?;
    let config = Arc::new(state.settings.config()?);
    ensure!(
        Store::has_token(&config.server.data_dir.join("assistant.db"))?,
        "Microsoft login required"
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let dataset_lock = claim_dataset(&config.server.data_dir)?;
    let store = Arc::new(Store::attach(&config.server.data_dir.join("assistant.db"))?);
    let token = Arc::new(OAuth::new(
        config.clone(),
        client.clone(),
        store.clone(),
        Vault::new(&security::secret("STATE_ENCRYPTION_KEY")?)?,
    )?);
    Ok(ProfileGraph {
        _lock: dataset_lock,
        graph: crate::adapters::graph::Graph {
            client,
            token,
            config,
            store,
            base_url: "https://graph.microsoft.com/v1.0".into(),
            client_state: String::new(),
        },
    })
}
async fn self_chat_status(state: &DesktopState) -> Result<serde_json::Value> {
    let config = state.settings.config()?;
    Ok(
        serde_json::json!({"enabled":config.graph.self_chat.is_some(),"configuration":config.graph.self_chat,
        "reception":"webhook_and_bounded_poll_10s", "diagnostics":Store::self_chat_diagnostics(&config.server.data_dir.join("assistant.db"))?}),
    )
}
async fn enable_self_chat(state: &DesktopState, id: Option<&str>) -> Result<serde_json::Value> {
    let was_running = state.running.lock().await.is_some();
    stop(state).await?;
    let result = async {
        let graph = profile_graph(state).await?;
        let id = match id {
            Some(id) => id.to_string(),
            None => {
                tokio::time::timeout(
                    std::time::Duration::from_secs(90),
                    graph.discover_self_chat(),
                )
                .await??
            }
        };
        graph.validate_self_chat(&id).await?;
        let mut config = state.settings.config()?;
        // Preserve activation boundary when re-enabling the same validated chat.
        let enabled_at = config
            .graph
            .self_chat
            .as_ref()
            .filter(|c| c.id == id)
            .map(|c| c.enabled_at)
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        config.graph.self_chat = Some(crate::config::SelfChat {
            id,
            user_id: config.graph.user_id.clone(),
            enabled_at,
        });
        let map = state.settings.map()?;
        state
            .settings
            .commit(&config, &map, &state.settings.tunnel().unwrap_or_default())?;
        self_chat_status(state).await
    }
    .await;
    if was_running {
        let restarted = start(state).await;
        result?;
        restarted?;
    } else {
        result?;
    }
    self_chat_status(state).await
}

fn validate_state_key(config: &Config, value: &str) -> Result<()> {
    let vault = Vault::new(value)?;
    let path = config.server.data_dir.join("assistant.db");
    if path.exists() {
        if security::secret_source("STATE_ENCRYPTION_KEY")?.is_some() {
            ensure!(
                security::constant_eq(&security::secret("STATE_ENCRYPTION_KEY")?, value),
                "the database requires its original encryption key"
            );
        }
        if let Some(encrypted) = Store::read_encrypted_token(&path)? {
            vault.open(&encrypted)?;
        }
    }
    Ok(())
}
fn import_secret_from_stdin(name: &str) -> Result<()> {
    let mut value = String::new();
    std::io::stdin().take(16_385).read_to_string(&mut value)?;
    ensure!(value.len() <= 16_384, "credential is too long");
    let config = LocalSettings::at(&control::profile_dir()?).config()?;
    ensure!(
        credential_names(&config).iter().any(|n| n == name),
        "credential name is not allowed"
    );
    let value = value.trim_end_matches(['\r', '\n']);
    if name == "STATE_ENCRYPTION_KEY" {
        validate_state_key(&config, value)?;
    }
    security::put_desktop_secret("default", name, value)
}

/// After an update stopped a running assistant, ask on the terminal whether to keep it
/// running with the new build. Without a terminal (or when asked not to prompt) the previous
/// state is kept.
pub(crate) fn keep_running_after_update(interactive: bool) -> bool {
    use std::io::{IsTerminal, Write};
    if !interactive || !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return true;
    }
    eprint!("The assistant was running. Keep it running with the new version? [Y/n] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return true;
    }
    !matches!(answer.trim().to_lowercase().as_str(), "n" | "no")
}

/// Entry point of the `personal-teams-assistant` binary. The host always runs without a window:
/// its owner's GUI is the local page in the system browser, and `pta` operates it. `--headless`
/// and `PTA_HEADLESS` are accepted and ignored, so launchers, units and updates of installations
/// that used them keep working.
pub fn run() {
    if std::env::var_os("PERSONAL_TEAMS_GIT_ASKPASS").is_some() {
        github::askpass();
        return;
    }
    // A service manager that cannot set variables (the Windows Task Scheduler) names an isolated
    // profile with `--profile-dir`; it means exactly `PTA_PROFILE_DIR`.
    let (args, profile) = service::profile_argument(std::env::args().skip(1).collect());
    if let Some(profile) = profile {
        // SAFETY: nothing else runs yet; every thread and runtime starts after this point.
        unsafe { std::env::set_var("PTA_PROFILE_DIR", profile) };
    }
    // Lets an update verify a staged build without starting a host.
    if args.as_slice() == ["--version"] {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return;
    }
    // The update's helper process: replaces the executables while no host runs (see
    // `maintenance::apply`). It is the same binary, so no separate updater exists.
    if args.as_slice() == [maintenance::HELPER_FLAG] {
        std::process::exit(maintenance::run_helper());
    }
    if args.as_slice() == ["--web"] {
        if web::run().is_err() {
            eprintln!("Web portal stopped. Check pta web status and the listener configuration.");
            std::process::exit(1);
        }
        return;
    }
    if args.first().map(String::as_str) == Some("--import-secret") {
        let result = args
            .get(1)
            .filter(|_| args.len() == 2)
            .context("missing credential name")
            .and_then(|name| {
                configure_credentials(&control::profile_dir()?)?;
                import_secret_from_stdin(name)
            });
        if result.is_err() {
            eprintln!("Could not import the credential.");
            std::process::exit(1);
        }
        return;
    }
    // Started at login by the entry `pta service install` created.
    let service = args.iter().any(|a| a == service::FLAG);
    // The update's helper starts the new host itself, or through the service manager, which
    // cannot pass it a variable: then the journal it holds names the hand-over.
    let handover = std::env::var_os(maintenance::HANDOVER).is_some()
        || (service
            && control::profile_dir().is_ok_and(|profile| {
                crate::maintenance::apply::handover(&profile.join(crate::maintenance::DIRECTORY))
            }));
    // Started by `pta`, the service manager or the update's helper (not by a person launching
    // the application): no page opens by itself.
    let background = args.iter().any(|a| a == "--host") || service || handover;
    // A new `cargo install` replaced this executable while the previous build keeps running:
    // stop that host (assistant and tunnel first) and take its place.
    let mut previous_running = false;
    // The successor a backup's restore started waits for the host it replaces.
    cloud::restore::wait_for_previous();
    if control::existing_host().is_ok()
        && std::env::current_exe().is_ok_and(|exe| control::outdated_host(&exe))
    {
        match tokio::runtime::Runtime::new()
            .map(|runtime| runtime.block_on(control::retire_outdated_host()))
        {
            Ok(Ok(running)) => previous_running = running,
            _ => {
                eprintln!(
                    "Could not stop the previous version of the host. Quit it with pta app quit and retry."
                );
                std::process::exit(1);
            }
        }
    }
    if control::existing_host().is_ok() {
        if background {
            return;
        }
        // Launching the application again opens the running host's page in the browser.
        let opened = tokio::runtime::Runtime::new().is_ok_and(|runtime| {
            runtime
                .block_on(control::client(control::Request::new("app_open"), false))
                .is_ok_and(|reply| reply.ok)
        });
        if !opened {
            eprintln!(
                "Another host already owns this profile. Run pta app open in a desktop session to open its page."
            );
            std::process::exit(1);
        }
        return;
    }
    // Never start a host between the two halves of an update; undo one that was interrupted.
    if !handover
        && let Ok(profile) = control::profile_dir()
        && let Some(reason) =
            crate::maintenance::apply::startup_block(&profile.join(crate::maintenance::DIRECTORY))
    {
        eprintln!("{reason}");
        std::process::exit(1);
    }
    let host_lock = match control::claim() {
        Ok(lock) => lock,
        Err(_) => {
            eprintln!("Another host owns this profile, or its private directory is unavailable.");
            std::process::exit(1);
        }
    };
    // A restore recorded before the restart is applied while no store is open.
    if let Ok(profile) = control::profile_dir()
        && let Some(running) = cloud::restore::apply_pending(&profile)
    {
        previous_running |= running;
    }
    // Provider crates must never log prompts or content.
    tracing_subscriber::fmt()
        .with_env_filter("personal_teams_assistant=info")
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .try_init()
        .ok();
    let keep = previous_running && keep_running_after_update(true);
    if previous_running
        && !keep
        && let Ok(profile) = control::profile_dir()
    {
        // The person chose not to keep it running: a later login does not start it either.
        service::remember(&profile, false);
    }
    let start = args.iter().any(|a| a == "--start") || keep;
    // Startup errors carry no credential values; show the chain to the operator.
    if let Err(error) = headless::run(host_lock, start, !background, service && !start) {
        eprintln!("Headless host stopped: {error:#}");
        std::process::exit(1);
    }
}

/// Operator-local Wiki queries use no Graph client or model and grant no audience permissions.
async fn azure_wiki(
    state: &DesktopState,
    id: String,
    operation: String,
    input: serde_json::Value,
    review: Option<crate::ado::wiki::Review>,
) -> std::result::Result<serde_json::Value, String> {
    if review.is_some() && operation != "search" {
        return Err("[invalid_input] limit and offset page a Wiki search only.".into());
    }
    use crate::{ado::wiki, tools::ToolSpec};
    let config = state.settings.config().map_err(fail)?;
    let map = state.settings.map().map_err(fail)?;
    let source = map
        .resources
        .iter()
        .find(|r| r.id == id)
        .ok_or("[invalid_input] Unknown Wiki source.")?;
    if operation != "list" && !source.enabled {
        return Err("[not_ready] Enable the selected Wiki source before search/read.".into());
    }
    let Access::Tool {
        tool:
            tool @ ToolSpec::AzureDevopsWiki {
                secret_ref,
                wiki_ids,
                author_mode,
                repository_docs,
                page_urls,
                ..
            },
    } = &source.access
    else {
        return Err("[invalid_input] Source is not an Azure DevOps Wiki tool.".into());
    };
    if operation == "list"
        && let Some(urls) = page_urls
    {
        return Ok(serde_json::json!({"page_urls": urls}));
    }
    let catalog = tool.catalog(&map.repositories).map_err(fail)?;
    let key = security::resolve(secret_ref, &config.secrets).map_err(|_| {
        "[not_ready] Azure credential unavailable; inspect credentials list.".to_owned()
    })?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(fail)?;
    // A review reads up to 25 pages with their authorship; the CLI waits 120 s for a reply.
    let reader = wiki::Reader::new(&client, &key, &catalog, wiki_ids)
        .map_err(|_| "[invalid_input] Invalid Wiki catalog or scope.".to_owned())?
        .with_deadline(if review.is_some() { 100 } else { 30 })
        .own_scope()
        .await
        .map_err(|_| {
            "[network] Team membership could not be read; check the Azure credential (vso.project)."
                .to_owned()
        })?;
    if let Some(urls) = page_urls {
        return reader
            .read_urls(urls)
            .await
            .map(|v| serde_json::json!(v))
            .map_err(fail);
    }
    let result = match operation.as_str() {
        "list" => reader.list().await,
        "search" => {
            let input: wiki::SearchInput = serde_json::from_value(input).map_err(|_| "[invalid_input] Invalid Wiki search JSON.".to_owned())?;
            input.validate().map_err(|e| format!("[invalid_input] {e}"))?;
            let mut result = match review {
                Some(review) => reader.search_page(&input, *author_mode, review).await,
                None => reader.search(&input, *author_mode).await,
            };
            if *repository_docs && let Ok(result) = &mut result {
                // Its own deadline, as in the assistant's tool call.
                if let Ok(docs) = wiki::Reader::new(&client, &key, &catalog, wiki_ids) {
                    match docs.own_scope().await {
                        Ok(docs) => docs.repository_docs(result).await,
                        Err(error) => result.warn(&format!(
                            "Team membership unreadable; repositories not read: {error}"
                        )),
                    }
                }
            }
            result
        }
        "read" => {
            let input: wiki::ReadInput = serde_json::from_value(input).map_err(|_| "[invalid_input] Invalid Wiki read JSON.".to_owned())?;
            input.validate().map_err(|e| format!("[invalid_input] {e}"))?;
            reader.read(&input).await
        }
        _ => return Err("[invalid_input] Unknown Wiki operation.".into()),
    }.map_err(|e| {
        let safe = e.to_string();
        if safe.starts_with("invalid") { format!("[invalid_input] {safe}") }
        else if safe.starts_with("Wiki API") || safe.starts_with("Wiki operation timeout") { format!("[network] {safe}") }
        else { "[network] Wiki read could not complete; check catalog, published version and connectivity.".into() }
    })?;
    let redactor =
        security::Redactor::new(&config.policy.sensitive_patterns, vec![key]).map_err(fail)?;
    let mut value = serde_json::to_value(result).map_err(fail)?;
    redact_wiki(&mut value, &redactor);
    Ok(value)
}

/// Redact a Wiki result's strings, keeping its schema (author emails are never serialized).
/// Redaction may hide text that only looks sensitive (a run of digits reads as a phone
/// number), so the pages it changed are named in `warnings` and the result is `partial`,
/// instead of returning a silently shorter page.
fn redact_wiki(value: &mut serde_json::Value, redactor: &security::Redactor) {
    fn redact(value: &mut serde_json::Value, r: &security::Redactor) {
        match value {
            serde_json::Value::String(s) => *s = r.redact(s),
            serde_json::Value::Array(a) => a.iter_mut().for_each(|v| redact(v, r)),
            serde_json::Value::Object(o) => o.values_mut().for_each(|v| redact(v, r)),
            _ => {}
        }
    }
    let before: Vec<String> = value["pages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| p["content"].as_str().unwrap_or("").to_owned())
        .collect();
    redact(value, redactor);
    let changed: Vec<String> = value["pages"]
        .as_array()
        .into_iter()
        .flatten()
        .zip(&before)
        .filter(|(page, content)| page["content"].as_str() != Some(content.as_str()))
        .map(|(page, _)| page["path"].as_str().unwrap_or("?").to_owned())
        .collect();
    if !changed.is_empty() {
        value["partial"] = serde_json::json!(true);
        if let Some(warnings) = value["warnings"].as_array_mut() {
            warnings.push(serde_json::json!(format!(
                "Text that looks sensitive (a secret, an email, a phone number or an ID) was replaced with [REDACTED] in {}",
                changed.join(", ")
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_simulation_without_sources_says_what_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let host = variables::tests::host(root.path());
        let mut map = host.state.settings.map().unwrap();
        for source in &mut map.resources {
            source.enabled = false;
        }
        host.state.settings.save_map(&map).unwrap();
        let reply = variables::tests::call(
            &host,
            authority::Transport::Local,
            "chat",
            serde_json::json!({"input": {"session": "t", "text": "What is state 10?", "group": false, "mentioned": false}}),
        )
        .await;
        assert_eq!(reply.code, "invalid_input", "{:?}", reply.message);
        assert!(
            reply
                .message
                .unwrap_or_default()
                .contains("enable a source")
        );
    }

    #[tokio::test]
    async fn the_audit_list_pages_with_a_message_or_a_date_cursor() {
        let root = tempfile::tempdir().unwrap();
        let host = variables::tests::host(root.path());
        let db = host
            .state
            .settings
            .config()
            .unwrap()
            .server
            .data_dir
            .join("assistant.db");
        let store = Store::open(&db).unwrap();
        for (resource, at) in [
            ("chats/c/messages/1", 1_700_000_000),
            ("chats/c/messages/2", 1_700_000_100),
        ] {
            store.enqueue(resource).unwrap();
            store
                .connection()
                .execute(
                    "UPDATE jobs SET created_at=?2 WHERE resource=?1",
                    rusqlite::params![resource, at],
                )
                .unwrap();
        }
        let list = |args: serde_json::Value| {
            variables::tests::call(&host, authority::Transport::Local, "audit_list", args)
        };
        let first = list(serde_json::json!({"limit": 1})).await;
        assert!(first.ok, "{:?}", first.message);
        assert_eq!(first.data["entries"][0]["resource"], "chats/c/messages/2");
        assert_eq!(first.data["next_cursor"], "chats/c/messages/2");
        assert_eq!(first.data["total"], 2);
        let next = list(serde_json::json!({"limit": 1, "before": first.data["next_cursor"]})).await;
        assert_eq!(next.data["entries"][0]["resource"], "chats/c/messages/1");
        assert!(next.data["next_cursor"].is_null());
        let dated = list(serde_json::json!({"before": "2023-11-14T22:14:00Z"})).await;
        assert_eq!(dated.data["entries"].as_array().unwrap().len(), 1);
        let unknown = list(serde_json::json!({"before": "chats/c/messages/9"})).await;
        assert_eq!(unknown.code, "invalid_input", "{:?}", unknown.message);
    }

    #[test]
    fn a_wiki_page_that_redaction_changed_is_named() {
        let redactor = security::Redactor::new(&[], vec![]).unwrap();
        let mut value = serde_json::json!({"partial": false, "warnings": [], "pages": [
            {"path": "/Flujo", "content": "La SPS pasa a estado `10` (pendiente)."},
            {"path": "/Estados", "content": "Estados 2 - 3 - 4 - 10 - 11 en orden."}]});
        redact_wiki(&mut value, &redactor);
        assert_eq!(
            value["pages"][0]["content"],
            "La SPS pasa a estado `10` (pendiente)."
        );
        assert!(
            value["pages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("[REDACTED]")
        );
        assert_eq!(value["partial"], true);
        let warning = value["warnings"][0].as_str().unwrap();
        assert!(warning.ends_with("in /Estados"), "{warning}");
    }

    #[test]
    fn tunnel_token_is_passed_only_in_the_child_environment() {
        let command =
            cloudflared_command(Path::new("cloudflared"), None, Some("test-private-token"));
        let args: Vec<_> = command.as_std().get_args().collect();
        assert_eq!(args, ["tunnel", "--no-autoupdate", "run"]);
        assert!(command.as_std().get_envs().any(|(name, value)| {
            name == "TUNNEL_TOKEN" && value == Some(std::ffi::OsStr::new("test-private-token"))
        }));
        let local = cloudflared_command(
            Path::new("cloudflared"),
            Some(Path::new("private.yml")),
            None,
        );
        assert!(
            local
                .as_std()
                .get_envs()
                .any(|(name, value)| name == "TUNNEL_TOKEN" && value.is_none())
        );
        let mut config = Config::desktop_template().unwrap();
        assert!(!config.server.cloudflare_tunnel);
        config.server.cloudflare_tunnel = true;
        assert!(
            credential_names(&config)
                .iter()
                .any(|name| name == "CLOUDFLARE_TUNNEL_TOKEN")
        );
        assert!(validate_tunnel_mode(&config, "").is_ok());
        assert!(validate_tunnel_mode(&config, "private.yml").is_err());
    }

    #[test]
    fn deepseek_key_is_required_only_while_deepseek_is_in_the_chain() {
        let mut config = Config::desktop_template().unwrap();
        assert_eq!(config.llm.active()[0].provider, "codex");
        assert!(required_credentials(&config).contains(&"DEEPSEEK_API_KEY".to_string()));
        for choice in &mut config.llm.chain {
            choice.enabled = choice.provider != "deepseek";
        }
        assert!(!required_credentials(&config).contains(&"DEEPSEEK_API_KEY".to_string()));
        // Still settable, so it can be stored before re-enabling DeepSeek.
        assert!(credential_names(&config).contains(&"DEEPSEEK_API_KEY".to_string()));
    }

    #[test]
    fn teams_setup_error_is_actionable_without_exposing_internal_errors() {
        let config = Config::desktop_template().unwrap();
        let message = fail(validate_teams_setup(&config).unwrap_err());
        assert!(message.contains("real public HTTPS URL"));
        assert!(message.contains("test chat"));
        let internal = fail(anyhow::anyhow!("private token value"));
        assert!(!internal.contains("private token value"));
    }

    #[tokio::test]
    async fn startup_keeps_the_authorization_error_for_gui_cli_and_web() {
        let (ready, receiver) = oneshot::channel();
        let (stop, _) = watch::channel(false);
        let mut task = tokio::spawn(async move {
            drop(ready);
            Err(crate::adapters::oauth::AuthorizationRequired.into())
        });
        let error = match service_ready(&mut task, receiver, &stop).await {
            Ok(_) => panic!("a rejected Microsoft session cannot start"),
            Err(error) => error,
        };
        assert!(
            error
                .downcast_ref::<crate::adapters::oauth::AuthorizationRequired>()
                .is_some()
        );
        let message = fail(error);
        assert!(message.starts_with("[not_ready] Microsoft session needs renewal."));
        assert!(message.contains("Settings"));
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_timeout_still_stops_and_aborts_the_service() {
        let (ready, receiver) = oneshot::channel();
        let (stop, stop_rx) = watch::channel(false);
        let mut task = tokio::spawn(async move {
            let _ready = ready;
            std::future::pending::<Result<()>>().await
        });
        assert!(service_ready(&mut task, receiver, &stop).await.is_err());
        assert!(*stop_rx.borrow());
        assert!(task.is_finished());
    }

    #[test]
    fn tunnel_cannot_route_another_host_or_service() {
        let dir = tempfile::tempdir().unwrap();
        let credentials = dir.path().join("tunnel.json");
        fs::write(&credentials, "{}").unwrap();
        let path = dir.path().join("config.yml");
        let config = Config::desktop_template().unwrap();
        let valid = format!(
            "tunnel: example\ncredentials-file: {}\ningress:\n  - hostname: assistant.example.com\n    service: http://127.0.0.1:3000\n  - service: http_status:404\n",
            credentials.display()
        );
        fs::write(&path, &valid).unwrap();
        assert!(validate_tunnel(&path, &config).is_ok());
        fs::write(
            &path,
            valid.replace("assistant.example.com", "other.example.com"),
        )
        .unwrap();
        assert!(validate_tunnel(&path, &config).is_err());
        fs::write(&path, valid.replace("127.0.0.1:3000", "127.0.0.1:9000")).unwrap();
        assert!(validate_tunnel(&path, &config).is_err());
    }

    #[test]
    fn teams_state_cannot_be_reused_for_another_tenant() {
        let dir = tempfile::tempdir().unwrap();
        let mut previous = Config::desktop_template().unwrap();
        previous.server.data_dir = dir.path().to_path_buf();
        previous.graph.tenant_id = uuid::Uuid::new_v4().to_string();
        let mut next = previous.clone();
        next.graph.tenant_id = uuid::Uuid::new_v4().to_string();
        assert!(protect_identity(&previous, &next).is_ok());
        Store::open(&dir.path().join("assistant.db")).unwrap();
        assert!(protect_identity(&previous, &next).is_err());
    }

    #[test]
    fn the_example_registration_gives_way_to_the_real_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut previous = Config::desktop_template().unwrap();
        previous.server.data_dir = dir.path().to_path_buf();
        let mut next = previous.clone();
        next.graph.tenant_id = uuid::Uuid::new_v4().to_string();
        next.graph.client_id = uuid::Uuid::new_v4().to_string();
        next.graph.user_id = uuid::Uuid::new_v4().to_string();
        // A first sign-in attempt under the example registration leaves a database behind.
        let store = Store::open(&dir.path().join("assistant.db")).unwrap();
        assert!(protect_identity(&previous, &next).is_ok());
        // A Microsoft session in it means the database is somebody's.
        store.put_token(b"session").unwrap();
        assert!(protect_identity(&previous, &next).is_err());
        // The verified user still completes a connected registration.
        let mut connected = next.clone();
        connected.graph.user_id = uuid::Uuid::nil().to_string();
        assert!(protect_identity(&connected, &next).is_ok());
        // A database that cannot be read is never handed over.
        drop(store);
        fs::write(dir.path().join("assistant.db"), "").unwrap();
        assert!(protect_identity(&previous, &next).is_err());
    }

    #[test]
    fn a_private_write_that_fails_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        // The target is a directory with content, so the final replacement fails.
        let target = dir.path().join("revision.json");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("kept"), "x").unwrap();
        assert!(write_private(&target, "{}").is_err());
        let left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["revision.json"]);
        write_private(&dir.path().join("other.json"), "{}").unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("other.json")).unwrap(),
            "{}"
        );
    }

    #[test]
    fn only_a_database_with_a_session_needs_its_state_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::desktop_template().unwrap();
        config.server.data_dir = dir.path().to_path_buf();
        assert!(!holds_session(&config));
        // Opened by a sign-in that never finished: nothing in it is sealed.
        let store = Store::open(&dir.path().join("assistant.db")).unwrap();
        assert!(!holds_session(&config));
        store.put_token(b"session").unwrap();
        assert!(holds_session(&config));
        drop(store);
        fs::write(dir.path().join("assistant.db"), "").unwrap();
        assert!(holds_session(&config));
    }

    #[tokio::test]
    async fn phase_follows_a_failed_start_and_ignores_an_idle_stop() {
        let dir = tempfile::tempdir().unwrap();
        let state = init_state(&dir.path().join("profile"), &dir.path().join("data")).unwrap();
        let mut phase = state.phase.subscribe();
        stop(&state).await.unwrap();
        assert!(
            !phase.has_changed().unwrap(),
            "an idle stop wakes no follower"
        );
        // The template has no Teams setup, so it fails before opening anything.
        assert!(start(&state).await.is_err());
        // It went through `starting` (only changes notify) and settled back on `stopped`.
        assert!(phase.has_changed().unwrap());
        assert_eq!(*phase.borrow_and_update(), AssistantPhase::Stopped);
        assert!(state.running.lock().await.is_none());
    }
}
