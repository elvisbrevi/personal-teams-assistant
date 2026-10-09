//! Authenticated browser transport for the existing GUI. Each account owns one host;
//! the desktop's private IPC remains loopback-only and rejects browser requests.
use super::*;
use axum::{
    Json,
    body::Bytes,
    extract::{DefaultBodyLimit, Path as WebPath},
    http::{HeaderMap, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::post,
};
use fs2::FileExt;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

mod profiles;
mod store;
use profiles::{Worker, create_profile, profile_dir};
use store::{Account, accounts, edit_accounts, hash_password, username, verify_password};
pub(super) fn admin(args: &[String]) -> Result<control::Reply> {
    store::admin(args)
}
pub(super) fn profile_command(binary: &Path, dir: &Path) -> std::process::Command {
    profiles::worker_command(binary, dir).into_std()
}

/// Apply the same identity guard when an operator starts a profile from the CLI or
/// desktop rather than from the portal. The default profile keeps its existing paths.
pub(super) fn validate_profile_identity(dir: &Path, config: &Config) -> Result<()> {
    let root = if !config.server.webhook_prefix.is_empty() {
        dir.parent()
            .filter(|p| p.file_name().is_some_and(|n| n == "profiles"))
            .and_then(Path::parent)
            .context("invalid web profile directory")?
            .to_path_buf()
    } else {
        dir.join("web")
    };
    if !root.join("accounts.json").exists() {
        return Ok(());
    }
    let entries = accounts(&root)?;
    let own = entries
        .iter()
        .find(|entry| profile_dir(&root, entry) == dir);
    ensure!(
        own.is_none_or(|entry| entry.enabled),
        "This web account is disabled."
    );
    if config.graph.user_id == uuid::Uuid::nil().to_string() {
        return Ok(());
    }
    let mut directories: Vec<_> = entries
        .iter()
        .map(|entry| profile_dir(&root, entry))
        .collect();
    directories.push(root.parent().unwrap().to_path_buf());
    for other in directories.into_iter().filter(|other| other != dir) {
        if let Ok(other) = read_config(&other.join("config.toml")) {
            ensure!(
                !other
                    .graph
                    .tenant_id
                    .eq_ignore_ascii_case(&config.graph.tenant_id)
                    || !other
                        .graph
                        .user_id
                        .eq_ignore_ascii_case(&config.graph.user_id),
                "This Teams account already belongs to another profile."
            );
        }
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    bind: String,
    public_url: String,
    session_hours: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:38656".into(),
            public_url: "http://localhost:38656".into(),
            session_hours: 12,
        }
    }
}
impl Settings {
    fn validate(&self) -> Result<()> {
        let bind: std::net::SocketAddr = self.bind.parse()?;
        ensure!(
            bind.ip().is_loopback() && bind.port() > 0,
            "invalid web listener: use a loopback address"
        );
        let url = url::Url::parse(&self.public_url)?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        ensure!(
            (url.scheme() == "https" || (url.scheme() == "http" && local))
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
                && (1..=168).contains(&self.session_hours),
            "invalid web settings: use an HTTPS origin"
        );
        Ok(())
    }
    fn origin(&self) -> String {
        url::Url::parse(&self.public_url)
            .unwrap()
            .origin()
            .ascii_serialization()
    }
    fn https(&self) -> bool {
        self.origin().starts_with("https://")
    }
    fn cookie_name(&self) -> &'static str {
        if self.https() {
            "__Host-pta-session"
        } else {
            "pta-session"
        }
    }
    fn cookie(&self, token: &str, expires: bool) -> String {
        format!(
            "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
            self.cookie_name(),
            if expires {
                0
            } else {
                self.session_hours * 3600
            },
            if self.https() { "; Secure" } else { "" }
        )
    }
}
fn settings(root: &Path) -> Result<Settings> {
    let path = root.join("settings.json");
    let config: Settings = if path.exists() {
        serde_json::from_slice(&fs::read(path)?)?
    } else {
        Settings::default()
    };
    config.validate()?;
    Ok(config)
}

#[derive(Clone)]
struct Session {
    account_id: String,
    version: String,
    csrf: String,
    expires: Instant,
}
struct Portal {
    root: PathBuf,
    settings: Settings,
    binary: PathBuf,
    sessions: Mutex<HashMap<String, Session>>,
    attempts: Mutex<HashMap<String, VecDeque<Instant>>>,
    hashing: Arc<Semaphore>,
    requests: Semaphore,
    dummy_hash: String,
    workers: Mutex<HashMap<String, Worker>>,
    identities: Mutex<()>,
}
impl Portal {
    fn new(root: PathBuf, binary: PathBuf) -> Result<Self> {
        security::private_dir(&root)?;
        Ok(Self {
            settings: settings(&root)?,
            root,
            binary,
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            hashing: Arc::new(Semaphore::new(2)),
            requests: Semaphore::new(32),
            dummy_hash: hash_password(&security::random_secret())?,
            workers: Mutex::new(HashMap::new()),
            identities: Mutex::new(()),
        })
    }
    fn origin(&self, headers: &HeaderMap) -> bool {
        headers.get(header::ORIGIN).and_then(|h| h.to_str().ok())
            == Some(self.settings.origin().as_str())
    }
    fn session_key(&self, headers: &HeaderMap) -> Option<String> {
        let name = self.settings.cookie_name();
        let mut tokens = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .flat_map(|h| h.split(';'))
            .filter_map(|cookie| cookie.trim().split_once('='))
            .filter(|(key, _)| *key == name)
            .map(|(_, token)| token);
        let token = tokens.next()?;
        if tokens.next().is_some() || token.len() > 128 {
            return None;
        }
        Some(format!("{:x}", Sha256::digest(token.as_bytes())))
    }
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        write: bool,
    ) -> std::result::Result<(Account, Session), StatusCode> {
        let key = self.session_key(headers).ok_or(StatusCode::UNAUTHORIZED)?;
        let session = self
            .sessions
            .lock()
            .await
            .get(&key)
            .cloned()
            .ok_or(StatusCode::UNAUTHORIZED)?;
        if session.expires <= Instant::now() {
            self.sessions.lock().await.remove(&key);
            return Err(StatusCode::UNAUTHORIZED);
        }
        let entry = accounts(&self.root)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
            .into_iter()
            .find(|e| e.enabled && e.id == session.account_id && e.version == session.version)
            .ok_or(StatusCode::UNAUTHORIZED)?;
        if write
            && (!self.origin(headers)
                || !headers
                    .get("x-pta-csrf")
                    .and_then(|h| h.to_str().ok())
                    .is_some_and(|csrf| security::constant_eq(csrf, &session.csrf)))
        {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok((entry, session))
    }
    async fn issue_session(
        &self,
        account: &Account,
    ) -> std::result::Result<(String, Session), StatusCode> {
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.expires > Instant::now());
        if sessions.len() >= 1000 {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        let token = security::random_secret();
        let session = Session {
            account_id: account.id.clone(),
            version: account.version.clone(),
            csrf: security::random_secret(),
            expires: Instant::now()
                + Duration::from_secs(u64::from(self.settings.session_hours) * 3600),
        };
        sessions.insert(
            format!("{:x}", Sha256::digest(token.as_bytes())),
            session.clone(),
        );
        Ok((token, session))
    }
    async fn login_limit(&self, name: &str) -> bool {
        let now = Instant::now();
        let mut attempts = self.attempts.lock().await;
        attempts.retain(|_, times| {
            times.retain(|time| now.duration_since(*time) < Duration::from_secs(60));
            !times.is_empty()
        });
        if attempts.len() >= 1024 {
            return false;
        }
        for (key, limit) in [("*", 60), (name, 5)] {
            let times = attempts.entry(key.to_owned()).or_default();
            if times.len() >= limit {
                return false;
            }
            times.push_back(now);
        }
        true
    }
}

fn router(portal: Arc<Portal>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route(
            "/login",
            get(|| async {
                asset(
                    "text/html; charset=utf-8",
                    include_str!("../../desktop/ui/login.html"),
                )
            }),
        )
        .route(
            "/login.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../../desktop/ui/login.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_str!("../../desktop/ui/style.css"),
                )
            }),
        )
        .route("/app.js", get(app_js))
        .route("/transport.js", get(transport_js))
        .route("/api/login", post(login).layer(DefaultBodyLimit::max(4096)))
        .route("/api/session", get(session))
        .route("/api/logout", post(logout))
        .route(
            "/api/password",
            post(password).layer(DefaultBodyLimit::max(4096)),
        )
        .route("/api/control", post(command))
        .route(
            "/webhooks/{id}/graph/{kind}",
            post(webhook).layer(DefaultBodyLimit::max(256_000)),
        )
        .layer(DefaultBodyLimit::max(1_000_000))
        .layer(middleware::from_fn_with_state(
            portal.clone(),
            secure_headers,
        ))
        .with_state(portal)
}
fn asset(content_type: &'static str, content: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], content).into_response()
}
async fn secure_headers(
    State(portal): State<Arc<Portal>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let origin = url::Url::parse(&portal.settings.origin()).unwrap();
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    let expected = &origin[url::Position::BeforeHost..url::Position::AfterPort];
    let mut response = if host != Some(expected) {
        StatusCode::MISDIRECTED_REQUEST.into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    headers.insert(header::X_FRAME_OPTIONS, "DENY".parse().unwrap());
    response
}
async fn index(State(portal): State<Arc<Portal>>, headers: HeaderMap) -> Response {
    if portal.authenticate(&headers, false).await.is_err() {
        return Redirect::to("/login").into_response();
    }
    asset(
        "text/html; charset=utf-8",
        include_str!("../../desktop/ui/index.html"),
    )
}
async fn app_js(State(portal): State<Arc<Portal>>, headers: HeaderMap) -> Response {
    if let Err(status) = portal.authenticate(&headers, false).await {
        return status.into_response();
    }
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../desktop/ui/app.js"),
    )
}
async fn transport_js(State(portal): State<Arc<Portal>>, headers: HeaderMap) -> Response {
    if let Err(status) = portal.authenticate(&headers, false).await {
        return status.into_response();
    }
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../desktop/ui/transport.js"),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    username: String,
    password: String,
}
async fn login(
    State(portal): State<Arc<Portal>>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> Response {
    if !portal.origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let name = username(&input.username).unwrap_or_default();
    if !portal.login_limit(&name).await {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(permit) = portal.hashing.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let Ok(entries) = accounts(&portal.root) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let account = entries
        .into_iter()
        .find(|e| e.enabled && e.username == name);
    let hash = account
        .as_ref()
        .map(|e| e.password_hash.clone())
        .unwrap_or_else(|| portal.dummy_hash.clone());
    let valid = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        verify_password(&hash, &input.password)
    })
    .await
    .unwrap_or(false);
    if !valid {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(account) = account else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Ok((token, _)) = portal.issue_session(&account).await else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    (
        [(header::SET_COOKIE, portal.settings.cookie(&token, false))],
        Json(json!({"ok":true})),
    )
        .into_response()
}
async fn session(State(portal): State<Arc<Portal>>, headers: HeaderMap) -> Response {
    match portal.authenticate(&headers, false).await {
        Ok((account, session)) => Json(json!({"username":account.username,"csrf_token":session.csrf,
            "current_profile":account.current_profile,"profile":profile_dir(&portal.root,&account)})).into_response(),
        Err(status) => status.into_response(),
    }
}
async fn logout(State(portal): State<Arc<Portal>>, headers: HeaderMap) -> Response {
    if let Err(status) = portal.authenticate(&headers, true).await {
        return status.into_response();
    }
    if let Some(key) = portal.session_key(&headers) {
        portal.sessions.lock().await.remove(&key);
    }
    (
        [(header::SET_COOKIE, portal.settings.cookie("", true))],
        Json(json!({"ok":true})),
    )
        .into_response()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordChange {
    current_password: String,
    new_password: String,
}
async fn password(
    State(portal): State<Arc<Portal>>,
    headers: HeaderMap,
    Json(input): Json<PasswordChange>,
) -> Response {
    let (account, _) = match portal.authenticate(&headers, true).await {
        Ok(value) => value,
        Err(status) => return status.into_response(),
    };
    if !portal.login_limit(&account.username).await {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(permit) = portal.hashing.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let hash = account.password_hash.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        ensure!(
            verify_password(&hash, &input.current_password),
            "invalid password"
        );
        hash_password(&input.new_password)
    })
    .await;
    let Ok(Ok(hash)) = result else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let updated = edit_accounts(&portal.root, |entries| {
        let entry = entries
            .iter_mut()
            .find(|e| e.id == account.id && e.version == account.version)
            .context("account changed")?;
        entry.password_hash = hash;
        entry.version = uuid::Uuid::new_v4().to_string();
        Ok(entry.clone())
    });
    let Ok(account) = updated else {
        return StatusCode::CONFLICT.into_response();
    };
    portal
        .sessions
        .lock()
        .await
        .retain(|_, session| session.account_id != account.id);
    let Ok((token, _)) = portal.issue_session(&account).await else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    (
        [(header::SET_COOKIE, portal.settings.cookie(&token, false))],
        Json(json!({"ok":true})),
    )
        .into_response()
}
async fn command(
    State(portal): State<Arc<Portal>>,
    headers: HeaderMap,
    Json(mut request): Json<control::Request>,
) -> Response {
    let (account, _) = match portal.authenticate(&headers, true).await {
        Ok(value) => value,
        Err(status) => return status.into_response(),
    };
    let Ok(_permit) = portal.requests.try_acquire() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let _identity = if matches!(
        request.method.as_str(),
        "save_settings" | "import_existing" | "finish_microsoft" | "start_assistant" | "restart"
    ) {
        Some(portal.identities.lock().await)
    } else {
        None
    };
    if let Err(error) = portal.check_request(&account, &request) {
        let message = match error.to_string().as_str() {
            message
                if message.starts_with("The portal operator")
                    || message.starts_with("Repository paths")
                    || message.starts_with("Import files")
                    || message.starts_with("This Teams account")
                    || message.starts_with("Operation is unavailable") =>
            {
                error.to_string()
            }
            _ => "Invalid request for this profile.".into(),
        };
        return Json(control::Reply::error("invalid_input", 2, &message)).into_response();
    }
    // These actions refer to the requesting browser, not a window/browser on the server.
    if request.method == "activity_open" {
        return Json(control::Reply::success(json!({"id":request.args["id"]}))).into_response();
    }
    if request.method == "open_github_login" {
        return Json(control::Reply::success(
            json!({"url":"https://github.com/login/device"}),
        ))
        .into_response();
    }
    if request.method == "connect_microsoft" {
        request.args["open"] = json!(false);
    }
    let dir = match portal.worker(&account).await {
        Ok(dir) => dir,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if matches!(request.method.as_str(), "start_assistant" | "restart")
        && let Ok(config) = read_config(&dir.join("config.toml"))
        && portal.unique_identity(&account, &config).is_err()
    {
        return Json(control::Reply::error(
            "state_conflict",
            6,
            "This Teams account already belongs to another profile.",
        ))
        .into_response();
    }
    let method = request.method.clone();
    let mut reply = match control::client_at(request, &dir).await {
        Ok(reply) => reply,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    if reply.ok
        && method == "finish_microsoft"
        && reply.data["connected"] == true
        && let Ok(config) = read_config(&dir.join("config.toml"))
        && portal.unique_identity(&account, &config).is_err()
    {
        let _ = control::client_at(control::Request::new("microsoft_logout"), &dir).await;
        reply = control::Reply::error(
            "state_conflict",
            6,
            "This Teams account already belongs to another profile. Connect a different account.",
        );
    }
    if reply.ok
        && matches!(
            method.as_str(),
            "start_assistant" | "restart" | "stop_assistant" | "microsoft_logout"
        )
    {
        let _ = portal.resume(
            &account,
            matches!(method.as_str(), "start_assistant" | "restart"),
        );
    }
    Json(reply).into_response()
}
async fn webhook(
    State(portal): State<Arc<Portal>>,
    WebPath((id, kind)): WebPath<(String, String)>,
    uri: Uri,
    body: Bytes,
) -> Response {
    if !matches!(kind.as_str(), "notifications" | "lifecycle") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(account) = accounts(&portal.root).ok().and_then(|entries| {
        entries
            .into_iter()
            .find(|e| e.enabled && !e.current_profile && e.id == id)
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let dir = profile_dir(&portal.root, &account);
    if control::existing_host_at(&dir).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Ok(config) = read_config(&dir.join("config.toml")) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(bind) = config.server.bind.parse::<std::net::SocketAddr>() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !bind.ip().is_loopback() || config.server.webhook_prefix != format!("/webhooks/{id}") {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let mut target = url::Url::parse(&format!("http://{bind}/graph/{kind}")).unwrap();
    target.set_query(uri.query());
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
    else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(response) = client
        .post(target)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
    else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let status = response.status();
    if response.content_length().is_some_and(|size| size > 4096) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let Ok(bytes) = response.bytes().await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    if bytes.len() > 4096 {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        bytes,
    )
        .into_response()
}

pub(super) fn run() -> Result<()> {
    let root = control::profile_dir()?.join("web");
    security::private_dir(&root)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("portal.lock"))?;
    security::protect_file(&root.join("portal.lock"))?;
    lock.try_lock_exclusive()?;
    let portal = Arc::new(Portal::new(root, std::env::current_exe()?)?);
    tokio::runtime::Runtime::new()?.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&portal.settings.bind).await?;
        eprintln!(
            "Web portal listening at {} (public URL: {}).",
            portal.settings.bind, portal.settings.public_url
        );
        let entries = accounts(&portal.root)?;
        // Accept callbacks before restoring assistants: Graph validates new
        // subscriptions immediately, even while other profile hosts are starting.
        let (shutdown, stopped) = oneshot::channel::<()>();
        let mut serving = tokio::spawn(axum::serve(listener, router(portal.clone()))
            .with_graceful_shutdown(async { let _ = stopped.await; }).into_future());
        let warm = { let portal = portal.clone(); tokio::spawn(async move {
            for account in entries.into_iter().filter(|entry| entry.enabled) {
                if let Ok(dir) = portal.worker(&account).await
                    && fs::read_to_string(portal.root.join("resume").join(&account.id)).is_ok_and(|value| value == "1") {
                    let _guard = portal.identities.lock().await;
                    if let Ok(config) = read_config(&dir.join("config.toml"))
                        && portal.unique_identity(&account, &config).is_ok() {
                        let _ = control::client_at(control::Request::new("start_assistant"), &dir).await;
                    }
                }
            }
        }) };
        let background = {
            let portal = portal.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    portal.reconcile().await;
                }
            })
        };
        let mut served = false;
        let result = tokio::select! {
            result = &mut serving => { served = true; result.map_err(anyhow::Error::from)?.map_err(anyhow::Error::from) }
            _ = super::headless::shutdown_signal() => Ok(()),
        };
        warm.abort(); let _ = warm.await;
        background.abort();
        let _ = background.await;
        let _ = shutdown.send(());
        let workers = std::mem::take(&mut *portal.workers.lock().await);
        let mut stopping = tokio::task::JoinSet::new();
        for (_, worker) in workers { stopping.spawn(worker.shutdown()); }
        while stopping.join_next().await.is_some() {}
        if !served && tokio::time::timeout(Duration::from_secs(5), &mut serving).await.is_err() {
            serving.abort();
        }
        drop(lock);
        result
    })
}

#[cfg(test)]
mod tests;
