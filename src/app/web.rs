//! Authenticated browser transport for the existing GUI. Each account owns one host;
//! the desktop's private IPC remains loopback-only and rejects browser requests.
use super::*;
use axum::{
    Json,
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, Path as WebPath},
    http::{HeaderMap, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::post,
};
use fs2::FileExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

mod access;
mod profiles;
mod store;
use profiles::{Worker, create_profile, profile_dir};
use store::{Account, accounts};
pub(super) fn admin(args: &[String]) -> Result<control::Reply> {
    store::admin(args)
}
pub(super) fn profile_command(binary: &Path, dir: &Path) -> std::process::Command {
    profiles::worker_command(binary, dir).into_std()
}

/// Resolve only operator-owned on-disk bindings. No request field participates.
pub(super) fn account_scope(profile: &Path) -> Result<authority::Scope> {
    let profile = profile.canonicalize()?;
    let isolated = profile
        .parent()
        .filter(|p| p.file_name().is_some_and(|n| n == "profiles"))
        .and_then(Path::parent)
        .filter(|p| p.file_name().is_some_and(|n| n == "web"));
    let root = isolated
        .map(Path::to_path_buf)
        .unwrap_or_else(|| profile.join("web"));
    let entries = accounts(&root)?;
    let entry = entries
        .iter()
        .find(|a| profile_dir(&root, a).canonicalize().ok().as_ref() == Some(&profile));

    let shared = isolated.is_some();
    let owner = !shared || entry.is_some_and(|a| settings(&root).is_ok_and(|s| s.owns(a)));
    Ok(authority::Scope {
        profile,
        account: entry.map(|a| authority::AccountBinding {
            id: a.id.clone(),
            version: a.version.clone(),
        }),
        // The standard profile is always the local operator's installation. Browser
        // sessions still require the enabled owner association at both ends of IPC.
        owner,
        shared,
    })
}

pub(super) fn check_bound_request(
    scope: &authority::Scope,
    request: &control::Request,
) -> Result<()> {
    let root = if scope.shared {
        scope
            .profile
            .parent()
            .and_then(Path::parent)
            .context("Invalid account profile.")?
            .to_path_buf()
    } else {
        scope.profile.join("web")
    };
    let account = accounts(&root)?
        .into_iter()
        .find(|a| profile_dir(&root, a).canonicalize().ok().as_ref() == Some(&scope.profile))
        .context("Unknown browser account.")?;
    Portal::new(root, PathBuf::new())?.check_request(&account, request)
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
    access: Option<access::Config>,
    owner_github_id: Option<u64>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:38656".into(),
            public_url: "http://localhost:38656".into(),
            session_hours: 12,
            access: None,
            owner_github_id: None,
        }
    }
}
impl Settings {
    fn validate(&self) -> Result<()> {
        ensure!(self.owner_github_id != Some(0), "invalid GitHub owner ID");
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
        if let Some(config) = &self.access {
            config.validate()?;
            ensure!(
                url.scheme() == "https",
                "Access requires an HTTPS public origin"
            );
        }
        Ok(())
    }
    fn owns(&self, account: &Account) -> bool {
        account.enabled
            && account.access_binding.as_ref().is_some_and(|binding| {
                self.owner_github_id
                    .is_some_and(|id| binding.provider_user_id == id.to_string())
            })
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
    fn cookie(&self, token: &str, max_age: u64) -> String {
        format!(
            "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
            self.cookie_name(),
            max_age.min(u64::from(self.session_hours) * 3600),
            if self.https() { "; Secure" } else { "" }
        )
    }
}
fn settings(root: &Path) -> Result<Settings> {
    let path = root.join("settings.json");
    let mut config: Settings = if path.exists() {
        serde_json::from_slice(&fs::read(path)?)?
    } else {
        Settings::default()
    };
    config.validate()?;
    // Existing local association is explicit operator authorization. Migrate only an
    // unambiguous sole owner; new installs and legacy multi-user portals stay closed.
    if config.owner_github_id.is_none() {
        let entries = accounts(root)?;
        let bindings: Vec<_> = entries
            .iter()
            .filter_map(|a| a.access_binding.as_ref())
            .collect();
        if let [binding] = bindings.as_slice() {
            config.owner_github_id = binding.provider_user_id.parse().ok().filter(|id| *id > 0);
        }
    }
    Ok(config)
}

#[derive(Clone)]
struct Session {
    account_id: String,
    version: String,
    csrf: String,
    expires: Instant,
    identity: access::Verified,
}
struct Portal {
    root: PathBuf,
    settings: Settings,
    binary: PathBuf,
    sessions: Mutex<HashMap<String, Session>>,
    attempts: Mutex<HashMap<String, VecDeque<Instant>>>,
    requests: Semaphore,
    access: Option<access::Verifier>,
    workers: Mutex<HashMap<String, Worker>>,
    identities: Mutex<()>,
}
impl Portal {
    fn new(root: PathBuf, binary: PathBuf) -> Result<Self> {
        security::private_dir(&root)?;
        let settings = settings(&root)?;
        let access = settings
            .access
            .clone()
            .map(access::Verifier::new)
            .transpose()?;
        Ok(Self {
            settings,
            root,
            binary,
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            requests: Semaphore::new(32),
            access,
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
        identity: &access::Verified,
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
        if session.expires <= Instant::now() || session.identity != *identity {
            self.sessions.lock().await.remove(&key);
            return Err(StatusCode::UNAUTHORIZED);
        }
        let entry = accounts(&self.root)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
            .into_iter()
            .find(|e| {
                self.settings.owns(e)
                    && e.id == session.account_id
                    && e.version == session.version
                    && e.access_binding.as_ref() == Some(&identity.binding)
                    && identity.issued_at >= e.access_valid_after
            })
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
        identity: access::Verified,
    ) -> std::result::Result<(String, Session), StatusCode> {
        let remaining = identity.expires_at.saturating_sub(access::now());
        if remaining == 0 {
            return Err(StatusCode::UNAUTHORIZED);
        }
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
                + Duration::from_secs(
                    (u64::from(self.settings.session_hours) * 3600).min(remaining),
                ),
            identity,
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

/// Static files served without Access: WebKit fetches a page's icons without cookies, so behind
/// Access an iPhone gets the login page instead of the home-screen icon. Each path has its own
/// Access Bypass app (`scripts/configure-web-access.py`).
const PUBLIC_ASSETS: [&str; 5] = [
    "/favicon.png",
    "/manifest.webmanifest",
    "/apple-touch-icon.png",
    "/icon-192.png",
    "/icon-512.png",
];

fn router(portal: Arc<Portal>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/login", get(login))
        .route(
            "/style.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_str!("../../desktop/ui/style.css"),
                )
            }),
        )
        // The tab icon, the iOS home-screen icon and the PWA manifest, public (`PUBLIC_ASSETS`).
        .route(
            "/favicon.png",
            get(|| async { png(include_bytes!("../../desktop/ui/favicon.png")) }),
        )
        .route(
            "/manifest.webmanifest",
            get(|| async {
                asset(
                    "application/manifest+json",
                    include_str!("../../desktop/ui/manifest.webmanifest"),
                )
            }),
        )
        .route(
            "/apple-touch-icon.png",
            get(|| async { png(include_bytes!("../../desktop/ui/apple-touch-icon.png")) }),
        )
        .route(
            "/icon-192.png",
            get(|| async { png(include_bytes!("../../desktop/ui/icon-192.png")) }),
        )
        .route(
            "/icon-512.png",
            get(|| async { png(include_bytes!("../../desktop/ui/icon-512.png")) }),
        )
        .route("/app.js", get(app_js))
        .route("/i18n.js", get(i18n_js))
        .route("/transport.js", get(transport_js))
        .route("/api/session", get(session))
        .route("/api/logout", post(logout))
        .route("/api/control", post(command))
        .route("/api/machines", post(machine_command))
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
fn png(content: &'static [u8]) -> Response {
    ([(header::CONTENT_TYPE, "image/png")], content).into_response()
}
async fn secure_headers(
    State(portal): State<Arc<Portal>>,
    mut request: axum::extract::Request,
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
    } else if (request.method() == axum::http::Method::POST
        && (store::callback_path(request.uri().path()) || request.uri().path() == "/api/machines"))
        || (request.method() == axum::http::Method::GET
            && PUBLIC_ASSETS.contains(&request.uri().path()))
    {
        next.run(request).await
    } else if let Some(verifier) = &portal.access {
        match verifier.verify(request.headers()).await {
            Ok(identity) => match store::access_revoked(&portal.root, &identity) {
                Ok(false) => {
                    request.extensions_mut().insert(identity);
                    next.run(request).await
                }
                Ok(true) => StatusCode::UNAUTHORIZED.into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            },
            Err(status) => status.into_response(),
        }
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Cloudflare Access is not configured. Contact the portal operator.",
        )
            .into_response()
    };
    let headers = response.headers_mut();
    if portal.settings.https() {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            "max-age=31536000".parse().unwrap(),
        );
    }
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    headers.insert(header::X_FRAME_OPTIONS, "DENY".parse().unwrap());
    response
}
async fn index(
    State(portal): State<Arc<Portal>>,
    Extension(identity): Extension<access::Verified>,
    headers: HeaderMap,
) -> Response {
    if portal
        .authenticate(&headers, &identity, false)
        .await
        .is_err()
    {
        return Redirect::to("/login").into_response();
    }
    asset(
        "text/html; charset=utf-8",
        include_str!("../../desktop/ui/index.html"),
    )
}
async fn app_js(
    State(portal): State<Arc<Portal>>,
    Extension(identity): Extension<access::Verified>,
    headers: HeaderMap,
) -> Response {
    if let Err(status) = portal.authenticate(&headers, &identity, false).await {
        return status.into_response();
    }
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../desktop/ui/app.js"),
    )
}
async fn i18n_js(
    State(portal): State<Arc<Portal>>,
    Extension(identity): Extension<access::Verified>,
    headers: HeaderMap,
) -> Response {
    if let Err(status) = portal.authenticate(&headers, &identity, false).await {
        return status.into_response();
    }
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../desktop/ui/i18n.js"),
    )
}
async fn transport_js(
    State(portal): State<Arc<Portal>>,
    Extension(identity): Extension<access::Verified>,
    headers: HeaderMap,
) -> Response {
    if let Err(status) = portal.authenticate(&headers, &identity, false).await {
        return status.into_response();
    }
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../desktop/ui/transport.js"),
    )
}
// Access handles the interactive login. This endpoint only creates a CSRF-bound
// local session for a verified, explicitly associated GitHub identity.
async fn login(
    State(portal): State<Arc<Portal>>,
    Extension(identity): Extension<access::Verified>,
    headers: HeaderMap,
) -> Response {
    let entry = match accounts(&portal.root) {
        Ok(entries) => entries.into_iter().find(|e| {
            portal.settings.owns(e)
                && e.access_binding.as_ref() == Some(&identity.binding)
                && identity.issued_at >= e.access_valid_after
        }),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(account) = entry else {
        return (
            StatusCode::FORBIDDEN,
            "Your GitHub identity has no enabled local account. Contact the portal operator.",
        )
            .into_response();
    };
    if portal
        .authenticate(&headers, &identity, false)
        .await
        .is_ok()
    {
        return Redirect::to("/").into_response();
    }
    if !portal.login_limit(&account.id).await {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let (token, session) = match portal.issue_session(&account, identity).await {
        Ok(value) => value,
        Err(status) => return status.into_response(),
    };
    // End the cross-site OAuth navigation so Strict cookies accompany asset/API requests.
    let mut response = asset(
        "text/html; charset=utf-8",
        include_str!("../../desktop/ui/index.html"),
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        portal
            .settings
            .cookie(
                &token,
                session
                    .expires
                    .saturating_duration_since(Instant::now())
                    .as_secs(),
            )
            .parse()
            .unwrap(),
    );
    response
}
async fn session(
    State(portal): State<Arc<Portal>>,
    Extension(identity): Extension<access::Verified>,
    headers: HeaderMap,
) -> Response {
    match portal.authenticate(&headers, &identity, false).await {
        Ok((account, session)) => Json(json!({"username":account.username,"csrf_token":session.csrf,
            "current_profile":account.current_profile,"profile":profile_dir(&portal.root,&account),"auth_provider":"github"})).into_response(),
        Err(status) => status.into_response(),
    }
}
async fn logout(
    State(portal): State<Arc<Portal>>,
    Extension(identity): Extension<access::Verified>,
    headers: HeaderMap,
) -> Response {
    if let Err(status) = portal.authenticate(&headers, &identity, true).await {
        return status.into_response();
    }
    if store::revoke_access(&portal.root, &identity).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if let Some(key) = portal.session_key(&headers) {
        portal.sessions.lock().await.remove(&key);
    }
    (
        [(header::SET_COOKIE, portal.settings.cookie("", 0))],
        Json(json!({"ok":true,"logout_url":"/cdn-cgi/access/logout"})),
    )
        .into_response()
}
async fn command(
    State(portal): State<Arc<Portal>>,
    Extension(identity): Extension<access::Verified>,
    headers: HeaderMap,
    Json(mut request): Json<control::Request>,
) -> Response {
    let (account, _) = match portal.authenticate(&headers, &identity, true).await {
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
    if request.method == "open_azure_cli_login" {
        return Json(control::Reply::success(
            json!({"url":"https://microsoft.com/devicelogin"}),
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
    let mut reply = match control::client_scoped(
        request,
        &dir,
        &account.id,
        &account.version,
        authority::Transport::Browser,
    )
    .await
    {
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

/// Machine-to-machine transport uses its own account-bound credentials, never browser cookies.
async fn machine_command(
    State(portal): State<Arc<Portal>>,
    headers: HeaderMap,
    Json(mut args): Json<serde_json::Value>,
) -> Response {
    if headers.contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(_permit) = portal.requests.try_acquire() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if authority::reject_authority_arguments(&args).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let registration = args["action"] == "register";
    let entries = match accounts(&portal.root) {
        Ok(entries) => entries,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let account = if registration {
        match store::consume_pairing(
            &portal.root,
            &portal.settings,
            token,
            args["id"].as_str().unwrap_or_default(),
        ) {
            Ok(account) => account,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    } else {
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        let id = args["id"].as_str().unwrap_or_default();
        entries
            .into_iter()
            .filter(|a| portal.settings.owns(a))
            .find(|a| {
                fs::read(profile_dir(&portal.root, a).join("implementation/registry.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .is_some_and(|r| {
                        r["machines"].as_array().is_some_and(|ms| {
                            ms.iter().any(|m| {
                                m["id"] == id
                                    && m["account_version"] == a.version
                                    && m["secret_hash"]
                                        .as_str()
                                        .is_some_and(|h| security::constant_eq(h, &hash))
                            })
                        })
                    })
            })
    };
    let Some(account) = account else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let dir = match portal.worker(&account).await {
        Ok(dir) => dir,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let mut request = control::Request::new(if registration {
        "machine_register"
    } else {
        "machine_protocol"
    });

    if !registration {
        args["token"] = json!(token);
    }
    request.args = args;
    let mut reply = match control::client_scoped(
        request,
        &dir,
        &account.id,
        &account.version,
        authority::Transport::Machine,
    )
    .await
    {
        Ok(reply) => reply,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    if registration && reply.ok {
        reply.data["account"] = json!(
            account
                .access_binding
                .as_ref()
                .and_then(|b| b.provider_user_id.parse::<u64>().ok())
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
            .find(|e| portal.settings.owns(e) && !e.current_profile && e.id == id)
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
    authority::require_operator(&control::profile_dir()?)?;
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
    ensure!(
        portal.access.is_some() && portal.settings.owner_github_id.is_some(),
        "Configure your Cloudflare Access and owner_github_id locally before starting your private web portal."
    );
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
            for account in entries.into_iter().filter(|entry| portal.settings.owns(entry)) {
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
