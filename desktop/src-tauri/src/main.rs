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

mod github;

struct Running {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<()>>,
    tunnel: Option<tokio::process::Child>,
}

struct DesktopState {
    config_path: PathBuf,
    map_path: PathBuf,
    running: Mutex<Option<Running>>,
    github_login: Mutex<Option<github::Pending>>,
    github_api: Mutex<()>,
    tunnel_path: PathBuf,
}

#[derive(Serialize)]
struct Snapshot {
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
        return message.0.into();
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
    fs::write(&temp, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(temp, path)?;
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
    security::keyring_profile(Some("default"))?;
    Ok(DesktopState {
        config_path,
        map_path,
        running: Mutex::new(None),
        github_login: Mutex::new(None),
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

#[tauri::command]
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
    Ok(())
}

#[tauri::command]
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
    ensure!(running.is_none(), "assistant is already running");
    let address: std::net::SocketAddr = config.server.bind.parse()?;
    let (stop, receiver) = watch::channel(false);
    let path = state.config_path.to_string_lossy().into_owned();
    let task = tokio::spawn(async move { runtime::serve_desktop(&path, receiver).await });
    let mut ready = false;
    for _ in 0..50 {
        if task.is_finished() {
            task.await??;
            anyhow::bail!("assistant exited before becoming ready");
        }
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if task.is_finished() {
                task.await??;
                anyhow::bail!("assistant exited before becoming ready");
            }
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if !ready {
        let _ = stop.send(true);
        let _ = task.await;
        anyhow::bail!("assistant did not open its local listener");
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
    *running = Some(Running { stop, task, tunnel });
    Ok(())
}

#[tauri::command]
async fn start_assistant(state: tauri::State<'_, DesktopState>) -> std::result::Result<(), String> {
    start(&state).await.map_err(fail)
}

#[tauri::command]
async fn save_settings(
    state: tauri::State<'_, DesktopState>,
    mut config: Config,
    map: KnowledgeMap,
    tunnel_config: String,
) -> std::result::Result<(), String> {
    let previous = read_config(&state.config_path).map_err(fail)?;
    config.knowledge_map = state.map_path.clone();
    validate_local(&config, &map).map_err(fail)?;
    protect_identity(&previous, &config).map_err(fail)?;
    validate_tunnel_mode(&config, &tunnel_config).map_err(fail)?;
    if !tunnel_config.trim().is_empty() {
        validate_tunnel(Path::new(tunnel_config.trim()), &config).map_err(fail)?;
    }
    let was_running = state.running.lock().await.is_some();
    if was_running {
        stop(&state).await.map_err(fail)?;
    }
    write_private(
        &state.map_path,
        &toml::to_string_pretty(&map).map_err(fail)?,
    )
    .map_err(fail)?;
    write_private(
        &state.config_path,
        &toml::to_string_pretty(&config).map_err(fail)?,
    )
    .map_err(fail)?;
    write_private(&state.tunnel_path, tunnel_config.trim()).map_err(fail)?;
    if was_running {
        start(&state).await.map_err(fail)?;
    }
    Ok(())
}

#[tauri::command]
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
    validate_local(&config, &map).map_err(fail)?;
    let previous = read_config(&state.config_path).map_err(fail)?;
    if previous.server.data_dir == config.server.data_dir {
        protect_identity(&previous, &config).map_err(fail)?;
    }
    ensure_stopped(&state).await.map_err(fail)?;
    write_private(
        &state.map_path,
        &toml::to_string_pretty(&map).map_err(fail)?,
    )
    .map_err(fail)?;
    write_private(
        &state.config_path,
        &toml::to_string_pretty(&config).map_err(fail)?,
    )
    .map_err(fail)?;
    Ok(())
}

async fn ensure_stopped(state: &DesktopState) -> Result<()> {
    ensure!(
        state.running.lock().await.is_none(),
        "stop the assistant before importing"
    );
    Ok(())
}

#[tauri::command]
async fn set_credential(
    state: tauri::State<'_, DesktopState>,
    name: String,
    value: String,
) -> std::result::Result<(), String> {
    let config = read_config(&state.config_path).map_err(fail)?;
    if !credential_names(&config).contains(&name) {
        return Err("Nombre de credencial no permitido".into());
    }
    security::put_desktop_secret("default", &name, &value).map_err(fail)
}

#[tauri::command]
async fn delete_credential(
    state: tauri::State<'_, DesktopState>,
    name: String,
) -> std::result::Result<(), String> {
    let config = read_config(&state.config_path).map_err(fail)?;
    if !credential_names(&config).contains(&name) {
        return Err("Nombre de credencial no permitido".into());
    }
    security::delete_desktop_secret("default", &name).map_err(fail)
}

#[tauri::command]
async fn chat(
    state: tauri::State<'_, DesktopState>,
    input: SimulationRequest,
) -> std::result::Result<SimulationResult, String> {
    local_chat::chat(&state.config_path, input)
        .await
        .map_err(fail)
}

#[tauri::command]
async fn begin_github_login(
    state: tauri::State<'_, DesktopState>,
    client_id: String,
) -> std::result::Result<github::DevicePrompt, String> {
    let (pending, prompt) = github::begin(client_id).await.map_err(fail)?;
    *state.github_login.lock().await = Some(pending);
    Ok(prompt)
}

#[tauri::command]
async fn open_github_login(app: tauri::AppHandle) -> std::result::Result<(), String> {
    app.opener()
        .open_url("https://github.com/login/device", None::<&str>)
        .map_err(fail)
}

#[tauri::command]
async fn finish_github_login(
    state: tauri::State<'_, DesktopState>,
) -> std::result::Result<(), String> {
    let pending = state
        .github_login
        .lock()
        .await
        .take()
        .ok_or_else(|| "Inicia la conexión con GitHub primero.".to_string())?;
    github::finish(pending).await.map_err(fail)
}

#[tauri::command]
async fn github_repositories(
    state: tauri::State<'_, DesktopState>,
) -> std::result::Result<Vec<github::Repository>, String> {
    let _guard = state.github_api.lock().await;
    github::repositories().await.map_err(fail)
}

#[tauri::command]
async fn disconnect_github() -> std::result::Result<(), String> {
    security::delete_desktop_secret("default", "GITHUB_OAUTH_TOKENS").map_err(fail)
}

#[tauri::command]
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
    validate_local(&config, &map).map_err(fail)?;
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

#[tauri::command]
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

#[tauri::command]
async fn connect_microsoft(
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
) -> std::result::Result<(), String> {
    let was_running = state.running.lock().await.is_some();
    if was_running {
        stop(&state).await.map_err(fail)?;
    }
    let login = connect_microsoft_inner(&app, &state).await;
    if was_running {
        let restarted = start(&state).await.map_err(fail);
        login?;
        restarted?;
    } else {
        login?;
    }
    Ok(())
}

async fn connect_microsoft_inner(
    app: &tauri::AppHandle,
    state: &DesktopState,
) -> std::result::Result<(), String> {
    let mut config = read_config(&state.config_path).map_err(fail)?;
    config.validate().map_err(fail)?;
    if !config.graph.channels.is_empty() {
        return Err("Esta versión no solicita permisos de canales.".into());
    }
    if security::secret_source("STATE_ENCRYPTION_KEY")
        .map_err(fail)?
        .is_none()
    {
        if config.server.data_dir.join("assistant.db").exists() {
            return Err("Importa la clave de estado existente antes de conectar Teams.".into());
        }
        security::put_desktop_secret(
            "default",
            "STATE_ENCRYPTION_KEY",
            &security::random_secret(),
        )
        .map_err(fail)?;
    }
    security::private_dir(&config.server.data_dir).map_err(fail)?;
    let store = Arc::new(Store::open(&config.server.data_dir.join("assistant.db")).map_err(fail)?);
    let vault =
        Vault::new(&security::secret("STATE_ENCRYPTION_KEY").map_err(fail)?).map_err(fail)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(fail)?;
    let oauth =
        Arc::new(OAuth::new_public(Arc::new(config.clone()), client, store, vault).map_err(fail)?);
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
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    if let Err(error) = app.opener().open_url(url, None::<&str>) {
        server.abort();
        return Err(fail(error));
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(600), receiver)
        .await
        .map_err(fail);
    server.abort();
    let user_id = result?.map_err(fail)?.ok_or_else(|| {
        "Microsoft no autorizó la conexión. Revisa si tu organización solicita aprobación."
            .to_string()
    })?;
    if config.graph.user_id == uuid::Uuid::nil().to_string() {
        config.graph.user_id = user_id;
        write_private(
            &state.config_path,
            &toml::to_string_pretty(&config).map_err(fail)?,
        )
        .map_err(fail)?;
    }
    Ok(())
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn import_secret_from_stdin(name: &str) -> Result<()> {
    let mut value = String::new();
    std::io::stdin().take(16_385).read_to_string(&mut value)?;
    ensure!(value.len() <= 16_384, "credential is too long");
    security::put_desktop_secret("default", name, value.trim_end_matches(['\r', '\n']))
}

fn main() {
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
    // Provider crates must never log prompts or content.
    tracing_subscriber::fmt()
        .with_env_filter("personal_teams_assistant=info,personal_teams_desktop=info")
        .try_init()
        .ok();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            save_settings,
            import_existing,
            set_credential,
            delete_credential,
            start_assistant,
            stop_assistant,
            chat,
            begin_github_login,
            open_github_login,
            finish_github_login,
            github_repositories,
            disconnect_github,
            clone_github_repository,
            update_github_repository,
            connect_microsoft,
        ])
        .setup(|app| {
            app.manage(init_state(app).map_err(Box::<dyn std::error::Error>::from)?);
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
                            let _ = start(&app.state::<DesktopState>()).await;
                        });
                    }
                    "stop" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = stop(&app.state::<DesktopState>()).await;
                        });
                    }
                    "quit" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = stop(&app.state::<DesktopState>()).await;
                            app.exit(0);
                        });
                    }
                    _ => {}
                })
                .build(app)?;
            show_main(app.handle());
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
                    let _ = stop(&app.state::<DesktopState>()).await;
                    app.exit(0);
                });
            }
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => show_main(app),
            _ => {}
        });
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
