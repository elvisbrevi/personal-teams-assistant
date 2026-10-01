use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};
use personal_teams_assistant::{
    adapters::oauth::OAuth,
    config::Config,
    knowledge::{Access, KnowledgeMap},
    local_chat, runtime, security,
    security::Vault,
    simulation::{SimulationRequest, SimulationResult},
    state::Store,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{
    Manager, WindowEvent,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{Mutex, oneshot, watch};

pub mod cli;
pub mod control;
mod github;
mod skill;

struct Running {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<()>>,
    tunnel: Option<tokio::process::Child>,
}

struct DesktopState {
    operations: Mutex<()>,
    loaded_config: Mutex<Option<Config>>,
    config_path: PathBuf,
    map_path: PathBuf,
    running: Mutex<Option<Running>>,
    github_login: Mutex<Option<github::Pending>>,
    github_finish: Mutex<Option<tokio::task::JoinHandle<std::result::Result<(), String>>>>,
    microsoft_login: Mutex<Option<tokio::task::JoinHandle<std::result::Result<(), String>>>>,
    github_api: Mutex<()>,
    tunnel_path: PathBuf,
}

#[derive(Serialize)]
struct Snapshot {
    revision: String,
    loaded_config: Option<Config>,
    host_version: String,
    host_running: bool,
    profile: PathBuf,
    tunnel_running: bool,
    microsoft_connected: bool,
    config: Config,
    map: KnowledgeMap,
    credentials: BTreeMap<String, String>,
    running: bool,
    active_subscriptions: usize,
    subscription_issue: Option<String>,
    github_connected: bool,
    tunnel_config: String,
}

#[derive(Debug)]
struct SetupError(&'static str);

impl std::fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for SetupError {}

fn fail<E: Into<anyhow::Error>>(error: E) -> String {
    let error = error.into();
    if let Some(message) = error.downcast_ref::<SetupError>() {
        return format!("[not_ready] {}", message.0);
    }
    if error.downcast_ref::<reqwest::Error>().is_some() {
        return "[network] Dependency request failed. Check connectivity and authorization.".into();
    }
    tracing::warn!(event = "desktop_operation_failed");
    "La operación no se completó. Revisa la configuración y vuelve a intentar.".into()
}

fn validate_teams_setup(config: &Config) -> Result<()> {
    ensure!(
        !config.server.public_url.contains("example.com"),
        SetupError(
            "Configura una URL HTTPS pública real para iniciar Teams. El chat de prueba funciona sin esta URL."
        )
    );
    ensure!(
        !config.graph.client_id.ends_with("0002") && !config.graph.tenant_id.ends_with("0001"),
        SetupError("Configura el tenant y el Client ID de tu registro Entra existente.")
    );
    ensure!(
        config.graph.user_id != uuid::Uuid::nil().to_string(),
        SetupError("Conecta tu cuenta Microsoft antes de iniciar Teams.")
    );
    ensure!(
        config.graph.channels.is_empty(),
        SetupError(
            "Esta versión de escritorio admite chats de Teams. Quita los canales de la configuración importada."
        )
    );
    Ok(())
}

fn read_config(path: &Path) -> Result<Config> {
    Ok(toml::from_str(&fs::read_to_string(path)?)?)
}

fn write_private(path: &Path, content: &str) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    security::protect_file(&temp)?;
    {
        use std::io::Write;
        file.write_all(content.as_bytes())?;
    }
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
    }
    security::replace_private_file(&temp, path)?;
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
    for path in map.repositories.values() {
        let root = fs::canonicalize(path)?;
        ensure!(
            root.join(".git").exists(),
            "repository is not a Git checkout"
        );
    }
    for resource in &map.resources {
        if let Access::File { repository, path } = &resource.access {
            personal_teams_assistant::knowledge::read_repository_file(
                &map.repositories,
                repository,
                path,
            )?;
        }
    }
    Ok(())
}

fn protect_identity(previous: &Config, next: &Config) -> Result<()> {
    let changed = previous.graph.tenant_id != next.graph.tenant_id
        || previous.graph.client_id != next.graph.client_id
        || (previous.graph.user_id != uuid::Uuid::nil().to_string()
            && previous.graph.user_id != next.graph.user_id);
    ensure!(
        !changed || !previous.server.data_dir.join("assistant.db").exists(),
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
            "Elige el túnel Cloudflare con token o el archivo de configuración local; configura solo uno."
        )
    );
    Ok(())
}

fn cloudflared_command(
    binary: &Path,
    config_path: Option<&Path>,
    token: Option<&str>,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(binary);
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

fn init_state(app: &tauri::App) -> Result<DesktopState> {
    let config_dir = app.path().app_config_dir()?;
    let data_dir = app.path().app_data_dir()?;
    security::private_dir(&config_dir)?;
    security::private_dir(&data_dir)?;
    let config_path = config_dir.join("config.toml");
    let map_path = config_dir.join("knowledge-map.toml");
    let tunnel_path = config_dir.join("cloudflared-path.txt");
    if !map_path.exists() {
        write_private(
            &map_path,
            &toml::to_string_pretty(&KnowledgeMap {
                repositories: BTreeMap::new(),
                resources: Vec::new(),
            })?,
        )?;
    }
    if !config_path.exists() {
        let mut config = Config::desktop_template()?;
        config.knowledge_map = map_path.clone();
        config.server.data_dir = data_dir;
        config.server.bind = "127.0.0.1:38655".into();
        config.graph.user_id = uuid::Uuid::nil().to_string();
        write_private(&config_path, &toml::to_string_pretty(&config)?)?;
    }
    let journal = config_dir.join("settings-rollback.json");
    if journal.exists() {
        let previous: Vec<(PathBuf, String)> = serde_json::from_slice(&fs::read(&journal)?)?;
        for (path, content) in previous {
            ensure!(
                [&config_path, &map_path, &tunnel_path].contains(&&path),
                "invalid recovery path"
            );
            write_private(&path, &content)?;
        }
        fs::remove_file(journal)?;
    }
    security::keyring_profile(Some("default"))?;
    Ok(DesktopState {
        operations: Mutex::new(()),
        loaded_config: Mutex::new(None),
        config_path,
        map_path,
        running: Mutex::new(None),
        github_login: Mutex::new(None),
        github_finish: Mutex::new(None),
        microsoft_login: Mutex::new(None),
        github_api: Mutex::new(()),
        tunnel_path,
    })
}

fn credential_names(config: &Config) -> Vec<String> {
    let mut names = vec![
        "TYPESAFE_API_KEY".into(),
        "DEEPSEEK_API_KEY".into(),
        "GRAPH_WEBHOOK_SECRET".into(),
        "STATE_ENCRYPTION_KEY".into(),
    ];
    if config.server.cloudflare_tunnel {
        names.push("CLOUDFLARE_TUNNEL_TOKEN".into());
    }
    names.extend(config.secrets.values().cloned());
    names.sort();
    names.dedup();
    names
}

async fn snapshot(state: tauri::State<'_, DesktopState>) -> std::result::Result<Snapshot, String> {
    let config = read_config(&state.config_path).map_err(fail)?;
    let map = KnowledgeMap::load(&state.map_path).map_err(fail)?;
    let (active_subscriptions, subscription_issue) =
        Store::subscription_health(&config.server.data_dir.join("assistant.db")).map_err(fail)?;
    let mut credentials = BTreeMap::new();
    for name in credential_names(&config) {
        let source = security::secret_source(&name).map_err(fail)?;
        credentials.insert(name, source.unwrap_or("missing").into());
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
        revision: control::revision(&state).map_err(fail)?,
        loaded_config: state
            .loaded_config
            .lock()
            .await
            .clone()
            .filter(|_| running.is_some()),
        host_version: env!("CARGO_PKG_VERSION").into(),
        host_running: true,
        profile: state.config_path.parent().unwrap().to_path_buf(),
        tunnel_running: running.as_ref().is_some_and(|r| r.tunnel.is_some()),
        microsoft_connected: Store::has_token(&config.server.data_dir.join("assistant.db"))
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
        tunnel_config: fs::read_to_string(&state.tunnel_path).unwrap_or_default(),
    })
}

async fn stop(state: &DesktopState) -> Result<()> {
    if let Some(mut running) = state.running.lock().await.take() {
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
    Ok(())
}

async fn stop_assistant(state: tauri::State<'_, DesktopState>) -> std::result::Result<(), String> {
    stop(&state).await.map_err(fail)
}

async fn start(state: &DesktopState) -> Result<()> {
    let config = read_config(&state.config_path)?;
    let map = KnowledgeMap::load(&state.map_path)?;
    validate_local(&config, &map)?;
    validate_teams_setup(&config)?;
    for name in ["GRAPH_WEBHOOK_SECRET", "STATE_ENCRYPTION_KEY"] {
        if security::secret_source(name)?.is_none() {
            ensure!(
                name != "STATE_ENCRYPTION_KEY"
                    || !config.server.data_dir.join("assistant.db").exists(),
                "import the existing state key before opening the database"
            );
            security::put_desktop_secret("default", name, &security::random_secret())?;
        }
    }
    for name in credential_names(&config)
        .into_iter()
        .filter(|name| name != "ENTRA_CLIENT_SECRET" && name != "ADMIN_AUTH_KEY")
    {
        security::secret(&name)?;
    }
    let tunnel_path = if state.tunnel_path.exists() {
        fs::read_to_string(&state.tunnel_path)?
    } else {
        String::new()
    };
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
    let path = state.config_path.to_string_lossy().into_owned();
    let (ready_tx, ready_rx) = oneshot::channel();
    let task =
        tokio::spawn(async move { runtime::serve_desktop_ready(&path, receiver, ready_tx).await });
    if tokio::time::timeout(std::time::Duration::from_secs(60), ready_rx)
        .await
        .ok()
        .and_then(Result::ok)
        .is_none()
    {
        let _ = stop.send(true);
        task.abort();
        let _ = task.await;
        anyhow::bail!("assistant failed to become ready");
    }
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
    *running = Some(Running { stop, task, tunnel });
    Ok(())
}

async fn start_assistant(state: tauri::State<'_, DesktopState>) -> std::result::Result<(), String> {
    start(&state).await.map_err(fail)
}

async fn save_settings(
    state: tauri::State<'_, DesktopState>,
    mut config: Config,
    map: KnowledgeMap,
    tunnel_config: String,
) -> std::result::Result<(), String> {
    let previous = read_config(&state.config_path).map_err(fail)?;
    config.knowledge_map = state.map_path.clone();
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
    let was_running = state.running.lock().await.is_some();
    if was_running {
        stop(&state).await.map_err(fail)?;
    }
    commit_settings(&state, &config, &map, tunnel_config.trim()).map_err(fail)?;
    if was_running {
        start(&state).await.map_err(fail)?;
    }
    Ok(())
}

async fn import_existing(
    state: tauri::State<'_, DesktopState>,
    path: String,
) -> std::result::Result<(), String> {
    let source = PathBuf::from(path);
    let source_dir = source.parent().ok_or_else(|| "Ruta inválida".to_string())?;
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
    config.knowledge_map = state.map_path.clone();
    validate_local(&config, &map)
        .map_err(|_| "[invalid_input] Configuration or knowledge map is invalid.".to_string())?;
    let previous = read_config(&state.config_path).map_err(fail)?;
    if previous.server.data_dir == config.server.data_dir {
        protect_identity(&previous, &config).map_err(|_| {
            "[conflict] The existing database belongs to another identity.".to_string()
        })?;
    }
    ensure_stopped(&state)
        .await
        .map_err(|_| "[conflict] Stop the service before this operation.".to_string())?;
    let tunnel = fs::read_to_string(&state.tunnel_path).unwrap_or_default();
    validate_tunnel_mode(&config, &tunnel).map_err(fail)?;
    commit_settings(&state, &config, &map, &tunnel).map_err(fail)?;
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
    state: tauri::State<'_, DesktopState>,
    name: String,
    value: String,
) -> std::result::Result<(), String> {
    let config = read_config(&state.config_path).map_err(fail)?;
    if !credential_names(&config).contains(&name) {
        return Err("[invalid_input] Nombre de credencial no permitido".into());
    }
    if name == "STATE_ENCRYPTION_KEY" {
        validate_state_key(&config, &value).map_err(|_| {
            "[conflict] The existing database requires its original encryption key.".to_string()
        })?;
    }
    ensure_stopped(&state)
        .await
        .map_err(|_| "[conflict] Stop the service before this operation.".to_string())?;
    security::put_desktop_secret("default", &name, &value).map_err(fail)
}

async fn delete_credential(
    state: tauri::State<'_, DesktopState>,
    name: String,
) -> std::result::Result<(), String> {
    let config = read_config(&state.config_path).map_err(fail)?;
    if !credential_names(&config).contains(&name) {
        return Err("[invalid_input] Nombre de credencial no permitido".into());
    }
    if name == "STATE_ENCRYPTION_KEY" && config.server.data_dir.join("assistant.db").exists() {
        return Err("La clave protege una base existente y no puede eliminarse.".into());
    }
    ensure_stopped(&state)
        .await
        .map_err(|_| "[conflict] Stop the service before this operation.".to_string())?;
    security::delete_desktop_secret("default", &name).map_err(fail)
}

async fn chat(
    state: tauri::State<'_, DesktopState>,
    input: SimulationRequest,
) -> std::result::Result<SimulationResult, String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(110),
        local_chat::chat(&state.config_path, input),
    )
    .await
    .map_err(|_| {
        "[network] Local simulation deadline exceeded; no Graph message was sent.".to_owned()
    })?
    .map_err(fail)
}

async fn begin_github_login(
    state: tauri::State<'_, DesktopState>,
    client_id: String,
) -> std::result::Result<github::DevicePrompt, String> {
    let (pending, prompt) = github::begin(client_id).await.map_err(fail)?;
    *state.github_login.lock().await = Some(pending);
    Ok(prompt)
}

async fn open_github_login(app: tauri::AppHandle) -> std::result::Result<(), String> {
    app.opener()
        .open_url("https://github.com/login/device", None::<&str>)
        .map_err(fail)
}

async fn github_repositories(
    state: tauri::State<'_, DesktopState>,
) -> std::result::Result<Vec<github::Repository>, String> {
    let _guard = state.github_api.lock().await;
    github::repositories().await.map_err(fail)
}

async fn disconnect_github() -> std::result::Result<(), String> {
    security::delete_desktop_secret("default", "GITHUB_OAUTH_TOKENS").map_err(fail)
}

async fn clone_github_repository(
    state: tauri::State<'_, DesktopState>,
    full_name: String,
    alias: String,
) -> std::result::Result<(), String> {
    let _guard = state.github_api.lock().await;
    let config = read_config(&state.config_path).map_err(fail)?;
    let mut map = KnowledgeMap::load(&state.map_path).map_err(fail)?;
    if map.repositories.contains_key(&alias) {
        return Err("Ese alias ya existe.".into());
    }
    let path = github::clone_repository(&full_name, &alias, &config.server.data_dir)
        .await
        .map_err(fail)?;
    map.repositories.insert(alias, path);
    validate_local(&config, &map)
        .map_err(|_| "[invalid_input] Configuration or knowledge map is invalid.".to_string())?;
    write_private(
        &state.map_path,
        &toml::to_string_pretty(&map).map_err(fail)?,
    )
    .map_err(fail)?;
    if state.running.lock().await.is_some() {
        stop(&state).await.map_err(fail)?;
        start(&state).await.map_err(fail)?;
    }
    Ok(())
}

async fn update_github_repository(
    state: tauri::State<'_, DesktopState>,
    alias: String,
) -> std::result::Result<(), String> {
    let _guard = state.github_api.lock().await;
    let config = read_config(&state.config_path).map_err(fail)?;
    let map = KnowledgeMap::load(&state.map_path).map_err(fail)?;
    let path = map
        .repositories
        .get(&alias)
        .ok_or_else(|| "Repositorio desconocido.".to_string())?;
    github::update_repository(&alias, path, &config.server.data_dir)
        .await
        .map_err(fail)?;
    if state.running.lock().await.is_some() {
        stop(&state).await.map_err(fail)?;
        start(&state).await.map_err(fail)?;
    }
    Ok(())
}

struct LoginState {
    oauth: Arc<OAuth>,
    csrf: String,
    result: Mutex<Option<oneshot::Sender<Option<String>>>>,
}

async fn desktop_callback(
    State(state): State<Arc<LoginState>>,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, &'static str) {
    let Some(csrf) = query.get("state") else {
        return (StatusCode::BAD_REQUEST, "Respuesta de Microsoft inválida.");
    };
    if !security::constant_eq(csrf, &state.csrf) {
        return (StatusCode::BAD_REQUEST, "Respuesta de Microsoft inválida.");
    }
    let result = if let Some(code) = query.get("code") {
        state.oauth.complete_desktop(csrf, code).await.ok()
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
            "Cuenta conectada. Puedes cerrar esta ventana.",
        )
    } else {
        (
            StatusCode::BAD_REQUEST,
            "No se pudo conectar la cuenta. Vuelve a la aplicación.",
        )
    }
}

async fn begin_microsoft(
    app: &tauri::AppHandle,
    open: bool,
) -> std::result::Result<String, String> {
    let state = app.state::<DesktopState>();
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
    let config = read_config(&state.config_path).map_err(fail)?;
    config.validate().map_err(fail)?;
    if !config.graph.channels.is_empty() {
        return Err("Desktop does not request channel permissions.".into());
    }
    if security::secret_source("STATE_ENCRYPTION_KEY")
        .map_err(fail)?
        .is_none()
    {
        if config.server.data_dir.join("assistant.db").exists() {
            return Err("Import the existing encryption key first.".into());
        }
        security::put_desktop_secret(
            "default",
            "STATE_ENCRYPTION_KEY",
            &security::random_secret(),
        )
        .map_err(fail)?;
    }
    stop(&state).await.map_err(fail)?;
    security::private_dir(&config.server.data_dir).map_err(fail)?;
    let dataset_lock = claim_dataset(&config.server.data_dir).map_err(fail)?;
    let store = Arc::new(Store::open(&config.server.data_dir.join("assistant.db")).map_err(fail)?);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(fail)?;
    let oauth = Arc::new(
        OAuth::new_public(
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
            oauth,
            csrf,
            result: Mutex::new(Some(sender)),
        }));
    let app = app.clone();
    if open {
        app.opener().open_url(&url, None::<&str>).map_err(fail)?;
    }
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
        let state = app.state::<DesktopState>();
        let _guard = state.operations.lock().await;
        let user = result
            .map_err(|_| "Microsoft authorization timed out.".to_string())?
            .map_err(fail)?
            .ok_or_else(|| "Microsoft authorization failed.".to_string())?;
        let mut next = read_config(&state.config_path).map_err(fail)?;
        next.graph.user_id = user;
        write_private(
            &state.config_path,
            &toml::to_string_pretty(&next).map_err(fail)?,
        )
        .map_err(fail)?;
        drop(dataset_lock);
        if was_running {
            start(&state).await.map_err(fail)?;
        }
        Ok(())
    }));
    Ok(url)
}

fn commit_settings(
    state: &DesktopState,
    config: &Config,
    map: &KnowledgeMap,
    tunnel: &str,
) -> Result<()> {
    let paths = [&state.config_path, &state.map_path, &state.tunnel_path];
    let previous: Vec<(PathBuf, String)> = paths
        .iter()
        .map(|p| (p.to_path_buf(), fs::read_to_string(p).unwrap_or_default()))
        .collect();
    let journal = state.config_path.with_file_name("settings-rollback.json");
    let contents = [
        toml::to_string_pretty(config)?,
        toml::to_string_pretty(map)?,
        tunnel.into(),
    ];
    write_private(&journal, &serde_json::to_string(&previous)?)?;
    for (path, content) in paths.iter().zip(contents) {
        if let Err(error) = write_private(path, &content) {
            for (path, old) in &previous {
                write_private(path, old)?;
            }
            fs::remove_file(&journal)?;
            return Err(error);
        }
    }
    fs::remove_file(journal)?;
    Ok(())
}
struct ProfileGraph {
    graph: personal_teams_assistant::adapters::graph::Graph,
    _lock: fs::File,
}
impl std::ops::Deref for ProfileGraph {
    type Target = personal_teams_assistant::adapters::graph::Graph;
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
    let config = Arc::new(read_config(&state.config_path)?);
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
    let token = Arc::new(OAuth::new_public(
        config.clone(),
        client.clone(),
        store.clone(),
        Vault::new(&security::secret("STATE_ENCRYPTION_KEY")?)?,
    )?);
    Ok(ProfileGraph {
        _lock: dataset_lock,
        graph: personal_teams_assistant::adapters::graph::Graph {
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
    let config = read_config(&state.config_path)?;
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
        let mut config = read_config(&state.config_path)?;
        // Preserve activation boundary when re-enabling the same validated chat.
        let enabled_at = config
            .graph
            .self_chat
            .as_ref()
            .filter(|c| c.id == id)
            .map(|c| c.enabled_at)
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        config.graph.self_chat = Some(personal_teams_assistant::config::SelfChat {
            id,
            user_id: config.graph.user_id.clone(),
            enabled_at,
        });
        let map = KnowledgeMap::load(&state.map_path)?;
        commit_settings(
            state,
            &config,
            &map,
            &fs::read_to_string(&state.tunnel_path).unwrap_or_default(),
        )?;
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

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
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
    let config = read_config(&control::profile_dir()?.join("config.toml"))?;
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

pub fn run() {
    if std::env::var_os("PERSONAL_TEAMS_GIT_ASKPASS").is_some() {
        github::askpass();
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--import-secret") {
        let result = args
            .get(1)
            .filter(|_| args.len() == 2)
            .context("missing credential name")
            .and_then(|name| import_secret_from_stdin(name));
        if result.is_err() {
            eprintln!("Could not import the credential.");
            std::process::exit(1);
        }
        return;
    }
    if control::existing_host().is_ok() {
        let hidden = args.iter().any(|a| a == "--host");
        if !hidden {
            let _ = tauri::async_runtime::block_on(control::client(
                control::Request::new("app_open"),
                false,
            ));
        }
        return;
    }
    let host_lock = match control::claim() {
        Ok(lock) => lock,
        Err(_) => {
            eprintln!("Another host owns this profile, or its private directory is unavailable.");
            return;
        }
    };
    // Provider crates must never log prompts or content.
    tracing_subscriber::fmt()
        .with_env_filter("personal_teams_assistant=info,personal_teams_desktop=info")
        .try_init()
        .ok();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![control::command])
        .setup(move |app| {
            app.manage(init_state(app).map_err(Box::<dyn std::error::Error>::from)?);
            control::launch(app.handle().clone(), host_lock)
                .map_err(Box::<dyn std::error::Error>::from)?;
            let open = MenuItem::with_id(
                app,
                "open",
                "Abrir configuración y chat",
                true,
                None::<&str>,
            )?;
            let start_item =
                MenuItem::with_id(app, "start", "Iniciar asistente", true, None::<&str>)?;
            let stop_item =
                MenuItem::with_id(app, "stop", "Detener asistente", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Salir", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &start_item, &stop_item, &quit])?;
            TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .context("missing app icon")?
                        .clone(),
                )
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "start" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ =
                                control::dispatch(&app, control::Request::new("start_assistant"))
                                    .await;
                        });
                    }
                    "stop" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ =
                                control::dispatch(&app, control::Request::new("stop_assistant"))
                                    .await;
                        });
                    }
                    "quit" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ =
                                control::dispatch(&app, control::Request::new("app_quit")).await;
                        });
                    }
                    _ => {}
                })
                .build(app)?;
            if !std::env::args().any(|a| a == "--host") {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("cannot build desktop application")
        .run(|app, event| match event {
            // Native macOS Quit/termination may bypass ExitRequested and deliver Exit directly.
            // Await cleanup on the main event thread while Tokio keeps servicing background tasks.
            tauri::RunEvent::Exit => {
                let _ = tauri::async_runtime::block_on(stop(&app.state::<DesktopState>()));
            }
            tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } => {
                api.prevent_exit();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = control::dispatch(&app, control::Request::new("stop_assistant")).await;
                    app.exit(0);
                });
            }
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => show_main(app),
            _ => {}
        });
}

/// Operator-local Wiki queries use no Graph client or model and grant no audience permissions.
async fn azure_wiki(
    state: &DesktopState,
    id: String,
    operation: String,
    input: serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    use personal_teams_assistant::{ado::wiki, tools::ToolSpec};
    let config = read_config(&state.config_path).map_err(fail)?;
    let map = KnowledgeMap::load(&state.map_path).map_err(fail)?;
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
            ToolSpec::AzureDevopsWiki {
                repository,
                path,
                secret_ref,
                wiki_ids,
                author_mode,
            },
    } = &source.access
    else {
        return Err("[invalid_input] Source is not an Azure DevOps Wiki tool.".into());
    };
    let catalog = personal_teams_assistant::knowledge::read_repository_file(
        &map.repositories,
        repository,
        path,
    )
    .map_err(fail)?;
    let key = security::resolve(secret_ref, &config.secrets).map_err(|_| {
        "[not_ready] Azure credential unavailable; inspect credentials list.".to_owned()
    })?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(fail)?;
    let reader = wiki::Reader::new(&client, &key, &catalog, wiki_ids)
        .map_err(|_| "[invalid_input] Invalid Wiki catalog or scope.".to_owned())?;
    let result = match operation.as_str() {
        "list" => reader.list().await,
        "search" => {
            let input: wiki::SearchInput = serde_json::from_value(input).map_err(|_| "[invalid_input] Invalid Wiki search JSON.".to_owned())?;
            input.validate().map_err(|e| format!("[invalid_input] {e}"))?;
            reader.search(&input, *author_mode).await
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
    // Redact strings recursively; retain schema/types and never serialize author emails.
    fn redact(value: &mut serde_json::Value, r: &security::Redactor) {
        match value {
            serde_json::Value::String(s) => *s = r.redact(s),
            serde_json::Value::Array(a) => a.iter_mut().for_each(|v| redact(v, r)),
            serde_json::Value::Object(o) => o.values_mut().for_each(|v| redact(v, r)),
            _ => {}
        }
    }
    redact(&mut value, &redactor);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn teams_setup_error_is_actionable_without_exposing_internal_errors() {
        let config = Config::desktop_template().unwrap();
        let message = fail(validate_teams_setup(&config).unwrap_err());
        assert!(message.contains("URL HTTPS pública real"));
        assert!(message.contains("chat de prueba"));
        let internal = fail(anyhow::anyhow!("private token value"));
        assert!(!internal.contains("private token value"));
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
        let mut next = previous.clone();
        next.graph.tenant_id = uuid::Uuid::new_v4().to_string();
        assert!(protect_identity(&previous, &next).is_ok());
        fs::write(dir.path().join("assistant.db"), "").unwrap();
        assert!(protect_identity(&previous, &next).is_err());
    }
}
