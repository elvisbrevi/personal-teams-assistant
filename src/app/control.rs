//! One local control channel, owned by the desktop executable (also in hidden mode).
use super::*;
use axum::{Json, extract::DefaultBodyLimit, http::HeaderMap, routing::post};
use fs2::FileExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub const CONTRACT: u32 = 1;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub contract: u32,
    pub method: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub revision: Option<String>,
}
impl Request {
    pub fn new(method: &str) -> Self {
        Self {
            contract: CONTRACT,
            method: method.into(),
            args: json!({}),
            revision: None,
        }
    }
}
#[derive(Serialize, Deserialize)]
pub struct Reply {
    pub contract: u32,
    pub version: String,
    pub ok: bool,
    pub code: String,
    pub exit_code: i32,
    pub message: Option<String>,
    pub data: Value,
    pub revision: Option<String>,
}
impl Reply {
    pub fn success(data: Value) -> Self {
        Self {
            contract: CONTRACT,
            version: env!("CARGO_PKG_VERSION").into(),
            ok: true,
            code: "ok".into(),
            exit_code: 0,
            message: None,
            data,
            revision: None,
        }
    }
    pub fn error(code: &str, exit_code: i32, message: &str) -> Self {
        Self {
            ok: false,
            code: code.into(),
            exit_code,
            message: Some(message.into()),
            ..Self::success(Value::Null)
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Endpoint {
    #[serde(default)]
    wiki_support: bool,
    #[serde(default)]
    activity_registration_support: bool,
    #[serde(default)]
    web_support: bool,
    #[serde(default)]
    web_access_support: bool,
    contract: u32,
    port: u16,
    token: String,
    /// Fingerprint of the host executable when it started. Hosts older than this field have
    /// none, which also marks them as outdated once a new build is installed.
    #[serde(default)]
    binary: Option<String>,
}

const IDENTIFIER: &str = "dev.personalteams.assistant";

pub(super) fn isolated_profile_dir() -> Result<Option<PathBuf>> {
    std::env::var_os("PTA_PROFILE_DIR")
        .map(|value| {
            let dir = PathBuf::from(value);
            ensure!(dir.is_absolute(), "PTA_PROFILE_DIR must be absolute");
            let standard = standard_profile_dir()?;
            let same = dir == standard
                || dir
                    .canonicalize()
                    .ok()
                    .zip(standard.canonicalize().ok())
                    .is_some_and(|(dir, standard)| dir == standard);
            Ok((!same).then_some(dir))
        })
        .transpose()
        .map(Option::flatten)
}

/// Profile directory shared by GUI, headless host and CLI; matches Tauri's app_config_dir.
pub fn profile_dir() -> Result<PathBuf> {
    if let Some(dir) = isolated_profile_dir()? {
        return Ok(dir);
    }
    standard_profile_dir()
}
fn standard_profile_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(std::env::var_os("HOME").context("home directory unavailable")?)
        .join("Library/Application Support");
    #[cfg(target_os = "windows")]
    let base = PathBuf::from(std::env::var_os("APPDATA").context("AppData unavailable")?);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = xdg_dir("XDG_CONFIG_HOME", ".config")?;
    Ok(base.join(IDENTIFIER))
}
/// Data directory for a new profile; matches Tauri's app_data_dir. Existing profiles keep
/// `server.data_dir`.
pub fn default_data_dir() -> Result<PathBuf> {
    if let Some(dir) = isolated_profile_dir()? {
        return Ok(dir.join("data"));
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Ok(xdg_dir("XDG_DATA_HOME", ".local/share")?.join(IDENTIFIER));
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    profile_dir()
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn xdg_dir(variable: &str, fallback: &str) -> Result<PathBuf> {
    match std::env::var_os(variable).map(PathBuf::from) {
        Some(path) if path.is_absolute() => Ok(path),
        _ => Ok(
            PathBuf::from(std::env::var_os("HOME").context("home directory unavailable")?)
                .join(fallback),
        ),
    }
}
pub fn existing_host() -> Result<()> {
    // The lock proves ownership. A stale descriptor or reused PID does not.
    existing_host_at(&profile_dir()?)
}
pub(super) fn existing_host_at(dir: &Path) -> Result<()> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("control.lock"))?;
    if file.try_lock_exclusive().is_ok() {
        anyhow::bail!("host is stopped");
    }
    let _: Endpoint = serde_json::from_slice(&fs::read(dir.join("control.json"))?)?;
    Ok(())
}
/// Size and modification time of an executable: they change when `cargo install` replaces it.
fn fingerprint(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("{}-{modified}", metadata.len()))
}
/// Host binary installed by this package, and the one installed beside `pta` by the former
/// `personal-teams-desktop` package (0.4.0 and earlier).
const HOST_BINARY: &str = "personal-teams-assistant";
const LEGACY_HOST_BINARY: &str = "personal-teams-desktop";
/// A running host started from `installed` before that file was replaced (a new install),
/// the former package's host installed beside it, or a host whose executable was
/// uninstalled. A host started from another path (e.g. a development build) is never
/// considered outdated.
fn outdated_at(dir: &Path, installed: &Path) -> Result<bool> {
    existing_host_at(dir)?;
    let endpoint: Endpoint = serde_json::from_slice(&fs::read(dir.join("control.json"))?)?;
    let running = PathBuf::from(fs::read_to_string(dir.join("host-path.txt"))?.trim());
    if !running.is_file() {
        return Ok(true);
    }
    let same_directory = running
        .parent()
        .zip(installed.parent())
        .is_some_and(|(a, b)| fs::canonicalize(a).ok() == fs::canonicalize(b).ok());
    if same_directory
        && running.file_stem().and_then(|n| n.to_str()) == Some(LEGACY_HOST_BINARY)
        && installed.file_stem().and_then(|n| n.to_str()) == Some(HOST_BINARY)
    {
        return Ok(true);
    }
    if fs::canonicalize(&running)? != fs::canonicalize(installed)? {
        return Ok(false);
    }
    Ok(endpoint.binary != fingerprint(installed))
}
/// Whether the running host is an older build of the installed `installed` executable.
pub fn outdated_host(installed: &Path) -> bool {
    profile_dir()
        .and_then(|dir| outdated_at(&dir, installed))
        .unwrap_or(false)
}
/// The password-era host rejects the new account fields. Never silently reuse
/// it against an Access-enabled portal, including the existing desktop profile.
pub(super) fn require_web_access_host_at(dir: &Path) -> Result<()> {
    existing_host_at(dir)?;
    let endpoint: Endpoint = serde_json::from_slice(&fs::read(dir.join("control.json"))?)?;
    ensure!(
        endpoint.web_access_support,
        "incompatible Access account schema: update the host and CLI together, preserving the assistant's running state"
    );
    Ok(())
}
/// Stop an outdated host (assistant and tunnel first) and wait until it releases the profile.
/// Returns whether its assistant was running, so the caller can offer to keep it running.
pub async fn retire_outdated_host() -> Result<bool> {
    let was_running = client(Request::new("snapshot"), false)
        .await
        .map(|reply| reply.data["running"].as_bool() == Some(true))
        .unwrap_or(false);
    // The host exits right after replying; a lost reply is fine, the lock tells the outcome.
    let _ = client(Request::new("app_quit"), false).await;
    for _ in 0..600 {
        if existing_host().is_err() {
            return Ok(was_running);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("previous host did not stop; quit it from its menu and retry")
}
/// The installed host executable used by this CLI.
pub fn installed_host() -> Result<PathBuf> {
    host_executable()
}
/// `cargo install` places `pta` beside `personal-teams-assistant`; the host also registers
/// its path.
fn host_executable() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let sibling = exe.with_file_name(if cfg!(windows) {
        format!("{HOST_BINARY}.exe")
    } else {
        HOST_BINARY.to_owned()
    });
    if sibling.is_file() {
        return Ok(sibling);
    }
    let registered = profile_dir()?.join("host-path.txt");
    if let Ok(path) = fs::read_to_string(registered) {
        let path = PathBuf::from(path.trim());
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
    }
    anyhow::bail!("desktop host missing: install both binaries with cargo install")
}
pub async fn client(request: Request, start_host: bool) -> Result<Reply> {
    if existing_host().is_err() {
        ensure!(start_host, "host is stopped");
        let binary = host_executable()?;
        let mut command = if let Some(dir) = isolated_profile_dir()? {
            security::private_dir(&dir)?;
            security::private_dir(&dir.join("home"))?;
            super::web::profile_command(&binary, &dir)
        } else {
            std::process::Command::new(binary)
        };
        command
            .arg("--host")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // The host outlives a short-lived CLI and its terminal process group.
            command.process_group(0);
        }
        command.spawn()?;
        let mut ready = false;
        for _ in 0..100 {
            if existing_host().is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ensure!(ready, "desktop host did not become ready");
    }
    client_at(request, &profile_dir()?).await
}
/// Select a profile from the authenticated session, never from browser input.
pub(super) async fn client_at(request: Request, dir: &Path) -> Result<Reply> {
    existing_host_at(dir)?;
    // Model-backed operations wait for the model without a deadline.
    let mut client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    if !matches!(request.method.as_str(), "chat" | "test_providers") {
        client = client.timeout(Duration::from_secs(120));
    }
    let client = client.build()?;
    for attempt in 0..20 {
        // A newly claimed lock can precede descriptor replacement. Retry only
        // connection establishment failures, before any request was delivered.
        let endpoint: Endpoint = serde_json::from_slice(&fs::read(dir.join("control.json"))?)?;
        ensure!(endpoint.contract == CONTRACT, "incompatible host contract");
        ensure!(
            endpoint.web_support
                || request.args["config"]["server"]
                    .get("webhook_prefix")
                    .is_none(),
            "incompatible web profile host: update the host and CLI together"
        );
        ensure!(
            endpoint.activity_registration_support
                || (!request.method.starts_with("activity_")
                    && !request.args.to_string().contains("activity_registration")),
            "incompatible activity registration host: update the host and CLI together"
        );
        ensure!(
            endpoint.wiki_support
                || (request.method != "azure_wiki"
                    && !request.args.to_string().contains("azure_devops_wiki")),
            "incompatible Wiki host: use a CLI/host build supporting Wiki before applying its schema"
        );
        let response = client
            .post(format!("http://127.0.0.1:{}/control", endpoint.port))
            .bearer_auth(endpoint.token)
            .json(&request)
            .send()
            .await;
        let response = match response {
            Err(error) if error.is_connect() && attempt < 19 => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            result => result?,
        };
        ensure!(
            response.status().is_success(),
            "control endpoint rejected the request"
        );
        let reply: Reply = crate::adapters::bounded_json(response, 2_000_000).await?;
        ensure!(reply.contract == CONTRACT, "incompatible reply contract");
        return Ok(reply);
    }
    anyhow::bail!("desktop host did not accept a connection")
}
struct LocalState {
    host: Arc<Host>,
    token: String,
    _lock: fs::File,
}
pub fn claim() -> Result<fs::File> {
    claim_at(&profile_dir()?)
}
fn claim_at(dir: &Path) -> Result<fs::File> {
    security::private_dir(dir)?;
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(dir.join("control.lock"))?;
    security::protect_file(&dir.join("control.lock"))?;
    lock.try_lock_exclusive()
        .context("another desktop host owns this profile")?;
    // A crashed/quitted host leaves a descriptor, but not a usable endpoint.
    let _ = fs::remove_file(dir.join("control.json"));
    Ok(lock)
}
/// Publish the control endpoint and return the server future; the caller spawns it on
/// its runtime (Tauri's or the headless Tokio runtime).
pub(crate) fn launch(
    host: Arc<Host>,
    lock: fs::File,
) -> Result<impl std::future::Future<Output = ()> + Send + 'static> {
    let dir = profile_dir()?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    write_private(
        &dir.join("host-path.txt"),
        &std::env::current_exe()?.to_string_lossy(),
    )?;
    let token = security::random_secret();
    write_private(
        &dir.join("control.json"),
        &serde_json::to_string(&Endpoint {
            wiki_support: true,
            activity_registration_support: true,
            web_support: true,
            web_access_support: true,
            contract: CONTRACT,
            port: listener.local_addr()?.port(),
            token: token.clone(),
            binary: fingerprint(&std::env::current_exe()?),
        })?,
    )?;
    let router = Router::new()
        .route("/control", post(endpoint))
        .layer(DefaultBodyLimit::max(1_000_000))
        .with_state(Arc::new(LocalState {
            host: host.clone(),
            token,
            _lock: lock,
        }));
    Ok(async move {
        let scheduler = tokio::spawn(activity::schedule(host));
        if let Ok(listener) = tokio::net::TcpListener::from_std(listener) {
            let _ = axum::serve(listener, router).await;
        }
        scheduler.abort();
    })
}
async fn endpoint(
    State(local): State<Arc<LocalState>>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> std::result::Result<Json<Reply>, StatusCode> {
    // Browser requests have Origin. No CORS, cookie, GET, or unauthenticated administration.
    if !authorized(&headers, &local.token) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(Json(dispatch(&local.host, request).await))
}
fn authorized(headers: &HeaderMap, token: &str) -> bool {
    !headers.contains_key("origin")
        && headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| security::constant_eq(h, &format!("Bearer {token}")))
}
pub(super) fn revision(state: &DesktopState) -> Result<String> {
    let mut hash = Sha256::new();
    for path in [&state.config_path, &state.map_path, &state.tunnel_path] {
        hash.update(fs::read(path).unwrap_or_default());
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn arg<T: serde::de::DeserializeOwned>(
    value: &Value,
    name: &str,
) -> std::result::Result<T, String> {
    serde_json::from_value(value.get(name).cloned().unwrap_or(Value::Null))
        .map_err(|_| format!("invalid argument: {name}"))
}
pub(crate) async fn dispatch(host: &Arc<Host>, request: Request) -> Reply {
    if request.contract != CONTRACT {
        return Reply::error(
            "contract_mismatch",
            6,
            "Update the CLI and host to compatible versions.",
        );
    }
    let state = &host.state;
    let _guard = state.operations.lock().await;
    if activity::active(state).await
        && ![
            "snapshot",
            "llm_providers",
            "audit",
            "logs",
            "activity_status",
            "activity_history",
            "activity_pending",
            "activity_open",
            "app_open",
            "app_hide",
            "app_quit",
        ]
        .contains(&request.method.as_str())
    {
        return Reply::error(
            "state_conflict",
            6,
            "Activity registration is running. Wait for it to finish before changing settings or starting another operation.",
        );
    }
    if let Some(expected) = &request.revision
        && revision(state).ok().as_ref() != Some(expected)
    {
        return Reply::error(
            "revision_conflict",
            6,
            "Settings changed. Read status again before applying changes.",
        );
    }
    let microsoft_pending = state
        .microsoft_login
        .lock()
        .await
        .as_ref()
        .is_some_and(|t| !t.is_finished());
    if microsoft_pending
        && ![
            "snapshot",
            "llm_providers",
            "activity_status",
            "activity_history",
            "activity_pending",
            "activity_open",
            "app_quit",
            "self_chat_status",
            "audit",
            "logs",
            "finish_microsoft",
            "cancel_microsoft",
            "app_open",
            "app_hide",
        ]
        .contains(&request.method.as_str())
    {
        return Reply::error(
            "authorization_pending",
            4,
            "Finish or cancel Microsoft authorization before mutating this profile.",
        );
    }
    let github_pending = state
        .github_finish
        .lock()
        .await
        .as_ref()
        .is_some_and(|t| !t.is_finished());
    if github_pending
        && ![
            "snapshot",
            "llm_providers",
            "activity_status",
            "activity_history",
            "activity_pending",
            "activity_open",
            "self_chat_status",
            "audit",
            "logs",
            "finish_github_login",
            "cancel_github",
            "disconnect_github",
            "app_open",
            "app_hide",
            "app_quit",
        ]
        .contains(&request.method.as_str())
    {
        return Reply::error(
            "authorization_pending",
            4,
            "Finish or cancel GitHub authorization before mutating this profile.",
        );
    }
    let args = request.args;
    let method = request.method.as_str();
    let result = operate(host, method, &args).await;
    let mut reply = match result {
        Ok(data) => Reply::success(data),
        Err(message) => {
            let (code, exit) = if message.starts_with("[invalid_input]")
                || message.starts_with("invalid argument")
                || method == "validate"
            {
                ("invalid_input", 2)
            } else if message.starts_with("[conflict]") {
                ("state_conflict", 6)
            } else if message.starts_with("[not_ready]") {
                ("not_ready", 3)
            } else if message.starts_with("[network]") {
                ("dependency_or_network", 5)
            } else {
                ("operation_failed", 1)
            };
            let message = message
                .strip_prefix(&format!(
                    "[{}] ",
                    match exit {
                        2 => "invalid_input",
                        3 => "not_ready",
                        5 => "network",
                        6 => "conflict",
                        _ => "error",
                    }
                ))
                .unwrap_or(&message);
            Reply::error(code, exit, message)
        }
    };
    // No provider passed: fail, but keep each provider's failure class in `data`.
    if reply.ok
        && method == "test_providers"
        && let Some(summary) = crate::diagnostics::failed_summary(&reply.data)
    {
        reply.ok = false;
        reply.code = "dependency_or_network".into();
        reply.exit_code = 5;
        reply.message = Some(summary);
    }
    if reply.ok && reply.data.get("pending").and_then(Value::as_bool) == Some(true) {
        reply.ok = false;
        reply.code = "authorization_pending".into();
        reply.exit_code = 4;
        reply.message =
            Some("Complete authorization, then use login finish to check the result.".into());
    }
    reply.revision = revision(state).ok();
    reply
}

async fn operate(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> std::result::Result<Value, String> {
    let state = &host.state;
    let value = match method {
        "snapshot" => serde_json::to_value(snapshot(host).await?).map_err(fail)?,
        "activity_status" | "activity_history" | "activity_pending" => activity::inspect(
            state,
            args.get("day").and_then(Value::as_str),
            method == "activity_pending",
        )
        .await
        .map_err(fail)?,
        "activity_run" => activity::run(host, args.get("day").and_then(Value::as_str), None)
            .await
            .map_err(fail)?,
        "activity_resolve" => activity::resolve(host, arg(args, "id")?, arg(args, "resolution")?)
            .await
            .map_err(fail)?,
        "activity_open" => {
            let id = args.get("id").and_then(Value::as_str);
            if let Some(id) = id {
                uuid::Uuid::parse_str(id)
                    .map_err(|_| "[invalid_input] Invalid activity ID.".to_string())?;
            }
            host.shell.open_activity(id).map_err(fail)?;
            json!({"visible":true})
        }
        "start_assistant" => {
            state
                .restart_offer
                .store(false, std::sync::atomic::Ordering::Relaxed);
            start(state).await.map_err(fail)?;
            json!({"running":true})
        }
        "dismiss_restart_offer" => {
            state
                .restart_offer
                .store(false, std::sync::atomic::Ordering::Relaxed);
            json!({"restart_offer":false})
        }
        "stop_assistant" => {
            stop(state).await.map_err(fail)?;
            json!({"running":false})
        }
        "restart" => {
            state
                .restart_offer
                .store(false, std::sync::atomic::Ordering::Relaxed);
            stop(state).await.map_err(fail)?;
            start(state).await.map_err(fail)?;
            json!({"running":true})
        }
        "save_settings" => {
            save_settings(
                state,
                arg(args, "config")?,
                arg(args, "map")?,
                arg(args, "tunnel_config").or_else(|_| arg(args, "tunnelConfig"))?,
            )
            .await?;
            json!({"applied":true,"running":state.running.lock().await.is_some()})
        }
        "validate_tunnel" => {
            let config = read_config(&state.config_path).map_err(fail)?;
            let path = fs::read_to_string(&state.tunnel_path).unwrap_or_default();
            validate_tunnel_mode(&config, &path).map_err(fail)?;
            if !path.trim().is_empty() {
                validate_tunnel(Path::new(path.trim()), &config).map_err(fail)?;
            }
            if config.server.cloudflare_tunnel {
                security::secret("CLOUDFLARE_TUNNEL_TOKEN").map_err(fail)?;
            }
            json!({"local_valid":true,"network_checked":false})
        }
        "validate" => {
            validate_local(&arg(args, "config")?, &arg(args, "map")?)
                .map_err(|_| "Configuration or knowledge map is invalid.".to_string())?;
            json!({"valid":true})
        }
        "import_existing" => {
            import_existing(state, arg(args, "path")?).await?;
            json!({"imported":true})
        }
        "set_credential" => {
            set_credential(state, arg(args, "name")?, arg(args, "value")?).await?;
            json!({"stored":true})
        }
        "delete_credential" => {
            delete_credential(state, arg(args, "name")?).await?;
            json!({"deleted":true})
        }
        "azure_wiki" => {
            azure_wiki(
                state,
                arg(args, "source")?,
                arg(args, "operation")?,
                args.get("input").cloned().unwrap_or(json!({})),
            )
            .await?
        }
        "chat" => serde_json::to_value(chat(state, arg(args, "input")?).await?).map_err(fail)?,
        "begin_github_login" => serde_json::to_value(
            begin_github_login(
                state,
                arg(args, "client_id").or_else(|_| arg(args, "clientId"))?,
            )
            .await?,
        )
        .map_err(fail)?,
        "open_github_login" => {
            host.shell
                .open_url("https://github.com/login/device")
                .map_err(fail)?;
            Value::Null
        }
        "finish_github_login" => {
            let mut task = state.github_finish.lock().await;
            if task.is_none() {
                let pending = state
                    .github_login
                    .lock()
                    .await
                    .take()
                    .ok_or_else(|| "Begin GitHub login first.".to_string())?;
                *task = Some(tokio::spawn(async move {
                    github::finish(pending).await.map_err(fail)
                }));
            }
            if task.as_ref().is_some_and(|t| t.is_finished()) {
                task.take().unwrap().await.map_err(fail)??;
                json!({"connected":true})
            } else {
                json!({"pending":true,"provider":"github"})
            }
        }
        "cancel_github" => {
            if let Some(task) = state.github_finish.lock().await.take() {
                task.abort();
            }
            *state.github_login.lock().await = None;
            json!({"cancelled":true})
        }
        "github_repositories" => {
            serde_json::to_value(github_repositories(state).await?).map_err(fail)?
        }
        "disconnect_github" => {
            if let Some(task) = state.github_finish.lock().await.take() {
                task.abort();
            }
            *state.github_login.lock().await = None;
            disconnect_github().await?;
            json!({"local_logout":true,"remote_revoked":false})
        }
        "clone_github_repository" => {
            clone_github_repository(
                state,
                arg(args, "full_name").or_else(|_| arg(args, "fullName"))?,
                arg(args, "alias")?,
            )
            .await?;
            json!({"cloned":true})
        }
        "update_github_repository" => {
            update_github_repository(state, arg(args, "alias")?).await?;
            json!({"synced":true})
        }
        "connect_microsoft" => {
            let url = begin_microsoft(
                host,
                args.get("open").and_then(Value::as_bool).unwrap_or(true),
            )
            .await?;
            let mut reply = json!({"pending":true,"authorization_url":url,"provider":"microsoft"});
            if host.shell.headless() {
                reply["remote_browser"] = json!(
                    "If the browser runs on another machine, copy the final http://localhost address it fails to open and run: pta auth microsoft finish --redirect 'URL'"
                );
            }
            reply
        }
        "finish_microsoft" => {
            if let Some(url) = args.get("redirect_url").and_then(Value::as_str) {
                forward_microsoft_redirect(state, url).await?;
            }
            let mut task = state.microsoft_login.lock().await;
            if let Some(job) = task.as_ref() {
                if job.is_finished() {
                    task.take().unwrap().await.map_err(fail)??;
                    json!({"connected":true})
                } else {
                    json!({"pending":true,"provider":"microsoft"})
                }
            } else {
                return Err("No Microsoft authorization pending.".into());
            }
        }
        "cancel_microsoft" => {
            if let Some(task) = state.microsoft_login.lock().await.take() {
                task.abort();
            }
            *state.microsoft_redirect.lock().await = None;
            json!({"cancelled":true,"running":state.running.lock().await.is_some()})
        }
        "microsoft_logout" => {
            stop(state).await.map_err(fail)?;
            *state.activity_graph.lock().await = None;
            let config = read_config(&state.config_path).map_err(fail)?;
            Store::logout(&config.server.data_dir.join("assistant.db")).map_err(fail)?;
            json!({"local_logout":true,"remote_revoked":false,"running":false})
        }
        "llm_providers" => {
            // Installed CLIs, their models and efforts; probes only local commands.
            serde_json::to_value(crate::llm::catalog().await).map_err(fail)?
        }
        "test_providers" => {
            let config = read_config(&state.config_path).map_err(fail)?;
            crate::diagnostics::providers(&config).await.map_err(fail)?
        }
        "test_connectivity" => {
            let was_running = state.running.lock().await.is_some();
            stop(state).await.map_err(fail)?;
            let result = async {
                let graph = profile_graph(state).await.map_err(fail)?;
                graph.verify_account().await.map_err(fail)?;
                Ok::<_, String>(json!({"graph_account_verified":true,"send_tested":false}))
            }
            .await;
            if was_running {
                let restart = start(state).await.map_err(fail);
                result.as_ref().map_err(Clone::clone)?;
                restart?;
            }
            result?
        }
        "test_self_chat" => {
            let was_running = state.running.lock().await.is_some();
            stop(state).await.map_err(fail)?;
            let result=async { let graph=profile_graph(state).await.map_err(fail)?;
                    let chat=graph.config.graph.self_chat.as_ref().ok_or_else(||"Enable the personal chat first.".to_string())?;
                    graph.validate_self_chat(&chat.id).await.map_err(fail)?;
                    Ok::<_,String>(json!({"membership_validated":chat.id != "48:notes","account_scope_verified":true,"end_to_end_verified":false,"next":"Write a new question in Teams, then inspect audit and the received reply."})) }.await;
            if was_running {
                let restart = start(state).await.map_err(fail);
                result.as_ref().map_err(Clone::clone)?;
                restart?;
            }
            result?
        }
        "self_chat_reconcile" => {
            let nonce: String = arg(args, "nonce")?;
            let id: String = arg(args, "id")?;
            let was_running = state.running.lock().await.is_some();
            stop(state).await.map_err(fail)?;
            let result = async {
                let graph = profile_graph(state).await.map_err(fail)?;
                graph.reconcile_output(&nonce, &id).await.map_err(fail)
            }
            .await;
            if was_running {
                let restart = start(state).await.map_err(fail);
                result.as_ref().map_err(Clone::clone)?;
                restart?;
            }
            result?;
            json!({"reconciled":true})
        }
        "self_chat_status" => self_chat_status(state).await.map_err(fail)?,
        "self_chat_enable" => enable_self_chat(state, args.get("id").and_then(Value::as_str))
            .await
            .map_err(fail)?,
        "self_chat_disable" => {
            let mut config = read_config(&state.config_path).map_err(fail)?;
            config.graph.self_chat = None;
            let map = KnowledgeMap::load(&state.map_path).map_err(fail)?;
            let tunnel = fs::read_to_string(&state.tunnel_path).unwrap_or_default();
            save_settings(state, config, map, tunnel).await?;
            json!({"enabled":false})
        }
        "audit" | "logs" => {
            let config = read_config(&state.config_path).map_err(fail)?;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            json!(
                Store::inspect(
                    &config.server.data_dir.join("assistant.db"),
                    method == "logs",
                    limit,
                    args.get("resource").and_then(Value::as_str),
                    args.get("content")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                )
                .map_err(fail)?
            )
        }
        "app_open" => {
            host.shell.show().map_err(fail)?;
            json!({"visible":true})
        }
        "app_hide" => {
            host.shell.hide().map_err(fail)?;
            json!({"visible":false})
        }
        "app_quit" => {
            activity::shutdown(state).await;
            if let Some(task) = state.microsoft_login.lock().await.take() {
                task.abort();
            }
            if let Some(task) = state.github_finish.lock().await.take() {
                task.abort();
            }
            stop(state).await.map_err(fail)?;
            let host = host.clone();
            tokio::spawn(async move {
                // Let the reply reach the client before the process exits.
                tokio::time::sleep(Duration::from_millis(150)).await;
                host.shell.exit();
            });
            json!({"quitting":true})
        }
        _ => return Err("Unknown operation. Consult capabilities.".into()),
    };
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_descriptor_does_not_advertise_wiki_schema_support() {
        let legacy: Endpoint =
            serde_json::from_value(json!({"contract":1,"port":1,"token":"synthetic"})).unwrap();
        assert!(!legacy.wiki_support);
        assert!(!legacy.web_access_support);
        let new: Endpoint = serde_json::from_value(
            json!({"contract":1,"port":1,"token":"synthetic","wiki_support":true}),
        )
        .unwrap();
        assert!(new.wiki_support);
    }
    #[test]
    fn host_lock_does_not_make_a_stale_descriptor_ready() {
        let dir = tempfile::tempdir().unwrap();
        let descriptor = dir.path().join("control.json");
        write_private(
            &descriptor,
            &serde_json::to_string(&Endpoint {
                wiki_support: true,
                activity_registration_support: true,
                contract: CONTRACT,
                port: 1,
                token: "stale-token".into(),
                binary: None,
                web_support: true,
                web_access_support: true,
            })
            .unwrap(),
        )
        .unwrap();
        let lease = claim_at(dir.path()).unwrap();
        assert!(existing_host_at(dir.path()).is_err());
        assert!(claim_at(dir.path()).is_err());
        write_private(
            &descriptor,
            &serde_json::to_string(&Endpoint {
                wiki_support: true,
                activity_registration_support: true,
                contract: CONTRACT,
                port: 2,
                token: "new-instance".into(),
                binary: None,
                web_support: true,
                web_access_support: true,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(existing_host_at(dir.path()).is_ok());
        drop(lease);
        assert!(existing_host_at(dir.path()).is_err());
    }
    #[test]
    fn only_a_replaced_installed_executable_marks_the_running_host_outdated() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join(HOST_BINARY);
        let other = dir.path().join("development-build");
        fs::write(&installed, b"old build").unwrap();
        fs::write(&other, b"dev build").unwrap();
        let publish = |binary: Option<String>, path: &Path| {
            write_private(
                &dir.path().join("control.json"),
                &serde_json::to_string(&Endpoint {
                    wiki_support: true,
                    activity_registration_support: true,
                    contract: CONTRACT,
                    port: 1,
                    token: "instance".into(),
                    binary,
                    web_support: true,
                    web_access_support: true,
                })
                .unwrap(),
            )
            .unwrap();
            write_private(&dir.path().join("host-path.txt"), &path.to_string_lossy()).unwrap();
        };
        // No host running: nothing to retire.
        assert!(outdated_at(dir.path(), &installed).is_err());
        let _lease = claim_at(dir.path()).unwrap();
        publish(fingerprint(&installed), &installed);
        assert!(!outdated_at(dir.path(), &installed).unwrap());
        // `cargo install` replaced the file the host was started from.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&installed, b"new build, different size").unwrap();
        assert!(outdated_at(dir.path(), &installed).unwrap());
        // A host from before this field existed is outdated too.
        publish(None, &installed);
        assert!(outdated_at(dir.path(), &installed).unwrap());
        // A host started from another path is never retired by this install.
        publish(None, &other);
        assert!(!outdated_at(dir.path(), &installed).unwrap());
        // The former package's host beside the new one, or an uninstalled host, is outdated.
        let legacy = dir.path().join(LEGACY_HOST_BINARY);
        fs::write(&legacy, b"0.4.0 host").unwrap();
        publish(fingerprint(&legacy), &legacy);
        assert!(outdated_at(dir.path(), &installed).unwrap());
        fs::remove_file(&legacy).unwrap();
        assert!(outdated_at(dir.path(), &installed).unwrap());
    }
    #[test]
    fn local_control_requires_instance_auth_and_rejects_browser_origins() {
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, "test-instance-secret"));
        headers.insert(
            "authorization",
            "Bearer wrong-instance-secret".parse().unwrap(),
        );
        assert!(!authorized(&headers, "test-instance-secret"));
        headers.insert(
            "authorization",
            "Bearer test-instance-secret".parse().unwrap(),
        );
        assert!(authorized(&headers, "test-instance-secret"));
        for origin in ["null", "https://example.com", "http://localhost"] {
            headers.insert("origin", origin.parse().unwrap());
            assert!(!authorized(&headers, "test-instance-secret"));
        }
    }
}
