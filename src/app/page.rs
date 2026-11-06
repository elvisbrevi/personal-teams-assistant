//! The owner's GUI in the system browser, on loopback, with the window's authority.
//!
//! Every host of the installation's own profile serves `desktop/ui` and the control contract on
//! `127.0.0.1` at a port derived from the profile, for the person at this computer. Only the
//! host opens it: `app_open` hands the system browser (through `Shell::open_url`) a single-use
//! code that expires within a minute, and the page exchanges it for an HttpOnly, SameSite=Strict
//! browser-session cookie. The code never appears in a reply, a log or a diagnostic, so the
//! control endpoint's local token, which agents share, never reaches the window's authority.
//! Calls from the page run as `Transport::Window`; nothing else widens.
use super::*;
use axum::{
    Json,
    extract::DefaultBodyLimit,
    http::{HeaderMap, Method, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::post,
};
use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{Mutex as Lock, OnceLock},
    time::{Duration, Instant},
};

/// How long the code the host hands the browser stays valid.
const CODE_LIFETIME: Duration = Duration::from_secs(60);
/// The longest a page session lasts; closing the browser, a restart or `app quit` end it sooner.
const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 3600);
/// The shortest time between two browser openings, so a loop cannot flood the desktop.
const OPEN_INTERVAL: Duration = Duration::from_secs(2);
const MAX_CODES: usize = 4;
const MAX_SESSIONS: usize = 16;
/// Below every operating system's ephemeral range, so the derived port is usually free.
const PORTS: std::ops::Range<u16> = 21_000..30_000;
const CANDIDATES: usize = 16;

pub(super) const NO_BROWSER: &str = "This host has no browser session to open its page in (no display, or a service session). Run pta app open in a desktop session on this computer, or start the application there.";
const UNAVAILABLE: &str = "This profile's local page is not available: it serves only the installation owner's own profile, and its port could not be opened. Restart the host with pta app quit and pta start.";
const TOO_SOON: &str =
    "The page was opened a moment ago. Wait a few seconds before opening it again.";

/// The page's listener and sign-ins, owned by the host; a restart starts empty.
#[derive(Default)]
pub(super) struct Page {
    address: OnceLock<SocketAddr>,
    cookie: OnceLock<String>,
    codes: Lock<Vec<(String, Instant)>>,
    sessions: Lock<HashMap<String, Session>>,
    opened: Lock<Option<Instant>>,
}

#[derive(Clone)]
struct Session {
    csrf: String,
    expires: Instant,
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

/// A URL- and cookie-safe random value (256 bits).
fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn profile_digest(profile: &Path) -> [u8; 32] {
    Sha256::digest(profile.to_string_lossy().as_bytes()).into()
}

/// The ports this profile tries, in order: the same list on every start, different per profile.
pub(super) fn candidates(profile: &Path) -> Vec<u16> {
    let digest = profile_digest(profile);
    let span = PORTS.end - PORTS.start;
    (0..CANDIDATES)
        .map(|i| PORTS.start + u16::from_be_bytes([digest[2 * i], digest[2 * i + 1]]) % span)
        .collect()
}

/// Cookies ignore ports, so each profile's page has its own cookie name on `127.0.0.1`.
pub(super) fn cookie_name(profile: &Path) -> String {
    let digest = profile_digest(profile);
    format!(
        "pta-local-{}",
        digest[..6]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

/// Whether this process can show a browser to a person: a desktop session, not a service.
pub(super) fn browser_session() -> bool {
    if cfg!(windows) {
        // Interactive logons (console or remote desktop) name their session; services do not.
        std::env::var_os("SESSIONNAME").is_some_and(|v| !v.is_empty())
    } else if cfg!(target_os = "macos") {
        std::env::var_os("SSH_CONNECTION").is_none()
    } else {
        ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()))
            && std::env::var_os("PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|dir| dir.join("xdg-open").is_file())
            })
    }
}

/// Hands a URL to the system browser. The URL never enters an error, a log or a diagnostic.
pub(super) fn system_browser(url: &str) -> Result<()> {
    let mut command = if cfg!(windows) {
        let mut command = security::program("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else if cfg!(target_os = "macos") {
        security::program("open")
    } else {
        security::program("xdg-open")
    };
    let mut child = command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| SetupError(NO_BROWSER))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Binds the page for the installation owner's own profile; never for a portal account's host.
pub(super) fn launch(host: Arc<Host>) -> Option<impl std::future::Future<Output = ()> + Send> {
    let scope = &host.state.authority;
    if !scope.owner || scope.shared {
        return None;
    }
    let listener = candidates(&scope.profile)
        .into_iter()
        .find_map(|port| std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).ok());
    let Some(listener) = listener.filter(|l| l.set_nonblocking(true).is_ok()) else {
        tracing::warn!(event = "local_page_unavailable");
        return None;
    };
    let address = listener.local_addr().ok()?;
    let page = &host.state.page;
    page.address.set(address).ok()?;
    page.cookie.set(cookie_name(&scope.profile)).ok()?;
    tracing::info!(event = "local_page_ready", port = address.port());
    let router = router(host.clone());
    Some(async move {
        if let Ok(listener) = tokio::net::TcpListener::from_std(listener) {
            let _ = axum::serve(listener, router).await;
        }
    })
}

/// `app_open`: the host opens its own page in the system browser with a fresh code. The reply
/// says only that it opened; the address with the code exists only in the browser's hands.
pub(super) fn open(host: &Host) -> Result<Value> {
    let page = &host.state.page;
    let Some(address) = page.address.get() else {
        return Err(SetupError(UNAVAILABLE).into());
    };
    if !host.shell.browser() {
        return Err(SetupError(NO_BROWSER).into());
    }
    let now = Instant::now();
    {
        let mut opened = page.opened.lock().unwrap_or_else(|e| e.into_inner());
        if opened.is_some_and(|last| now.duration_since(last) < OPEN_INTERVAL) {
            return Err(Rejected::Conflict(TOO_SOON).into());
        }
        *opened = Some(now);
    }
    let code = random();
    {
        let mut codes = page.codes.lock().unwrap_or_else(|e| e.into_inner());
        codes.retain(|(_, expires)| *expires > now);
        codes.push((digest(&code), now + CODE_LIFETIME));
        let excess = codes.len().saturating_sub(MAX_CODES);
        codes.drain(..excess);
    }
    host.shell
        .open_url(&format!("http://{address}/open?code={code}"))
        .map_err(|_| SetupError(NO_BROWSER))?;
    Ok(json!({"opened": true, "target": "browser"}))
}

impl Page {
    pub(super) fn available(&self) -> bool {
        self.address.get().is_some()
    }
    /// Consumes a code: valid once, before it expires; an expired or reused one opens nothing.
    fn redeem(&self, code: &str) -> bool {
        if code.len() != 64 || !code.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
        let key = digest(code);
        let now = Instant::now();
        let mut codes = self.codes.lock().unwrap_or_else(|e| e.into_inner());
        let found = codes.iter().position(|(stored, _)| *stored == key);
        let valid = found.is_some_and(|i| codes[i].1 > now);
        if let Some(i) = found {
            codes.remove(i);
        }
        codes.retain(|(_, expires)| *expires > now);
        valid
    }
    fn issue(&self) -> String {
        let token = random();
        let now = Instant::now();
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|_, s| s.expires > now);
        while sessions.len() >= MAX_SESSIONS {
            let oldest = sessions
                .iter()
                .min_by_key(|(_, s)| s.expires)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(key) => sessions.remove(&key),
                None => break,
            };
        }
        sessions.insert(
            digest(&token),
            Session {
                csrf: security::random_secret(),
                expires: now + SESSION_LIFETIME,
            },
        );
        token
    }
    fn session(&self, headers: &HeaderMap) -> Option<Session> {
        let name = self.cookie.get()?;
        let mut tokens = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .flat_map(|h| h.split(';'))
            .filter_map(|cookie| cookie.trim().split_once('='))
            .filter(|(key, _)| key == name)
            .map(|(_, token)| token);
        let token = tokens.next()?;
        if tokens.next().is_some() || token.len() > 128 {
            return None;
        }
        let key = digest(token);
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match sessions.get(&key) {
            Some(session) if session.expires > Instant::now() => Some(session.clone()),
            Some(_) => {
                sessions.remove(&key);
                None
            }
            None => None,
        }
    }
    fn origin(&self) -> Option<String> {
        self.address
            .get()
            .map(|address| format!("http://{address}"))
    }
}

fn router(host: Arc<Host>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/open", get(exchange))
        .route(
            "/style.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_str!("../../desktop/ui/style.css"),
                )
            }),
        )
        .route(
            "/i18n.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../../desktop/ui/i18n.js"),
                )
            }),
        )
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
        .route("/transport.js", get(transport_js))
        .route("/api/session", get(session))
        .route("/api/control", post(command))
        .layer(DefaultBodyLimit::max(1_000_000))
        .layer(middleware::from_fn_with_state(host.clone(), guard))
        .with_state(host)
}

fn asset(content_type: &'static str, content: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], content).into_response()
}
fn png(content: &'static [u8]) -> Response {
    ([(header::CONTENT_TYPE, "image/png")], content).into_response()
}

/// DNS rebinding and cross-origin requests stop here: the `Host` must be this exact loopback
/// address, an `Origin` must be this page, and a fetch from another site (another port on
/// `127.0.0.1` is the same site) is refused before any handler runs.
async fn guard(
    State(host): State<Arc<Host>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let page = &host.state.page;
    let expected = page.address.get().map(ToString::to_string);
    let headers = request.headers();
    let host_ok = expected.is_some()
        && headers.get_all(header::HOST).iter().count() == 1
        && headers.get(header::HOST).and_then(|h| h.to_str().ok()) == expected.as_deref();
    let origin_ok = match headers.get(header::ORIGIN) {
        None => request.method() != Method::POST,
        Some(origin) => origin.to_str().ok() == page.origin().as_deref(),
    };
    let site_ok = headers
        .get("sec-fetch-site")
        .and_then(|h| h.to_str().ok())
        .is_none_or(|site| matches!(site, "same-origin" | "none"));
    let mut response = if !host_ok {
        StatusCode::MISDIRECTED_REQUEST.into_response()
    } else if !origin_ok || !site_ok {
        StatusCode::FORBIDDEN.into_response()
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

/// The shared GUI document, marked so `transport.js` picks this page's transport.
fn document() -> &'static str {
    static DOCUMENT: OnceLock<String> = OnceLock::new();
    DOCUMENT.get_or_init(|| {
        include_str!("../../desktop/ui/index.html").replacen(
            "<head>",
            "<head>\n  <meta name=\"pta-transport\" content=\"local\">",
            1,
        )
    })
}

/// Shown without a session: a used, expired or missing sign-in. It names the next step and
/// never offers a link that signs in.
const SIGNED_OUT: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Personal Teams Assistant</title><link rel=\"stylesheet\" href=\"/style.css\"><script src=\"/i18n.js\" defer></script></head><body><main class=\"panel\"><h1>Open the assistant again</h1><p role=\"alert\">This page's sign-in ended or was already used. Open it again from this computer with the command below, or by starting the application.</p><p><code>pta app open</code></p></main></body></html>";

fn signed_out() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        asset("text/html; charset=utf-8", SIGNED_OUT),
    )
        .into_response()
}

async fn index(State(host): State<Arc<Host>>, headers: HeaderMap) -> Response {
    if host.state.page.session(&headers).is_none() {
        return signed_out();
    }
    asset("text/html; charset=utf-8", document())
}

/// Exchanges the host's code for a browser-session cookie (no `Max-Age`: closing the browser
/// ends it) and leaves the code out of the address bar and history.
async fn exchange(State(host): State<Arc<Host>>, uri: Uri) -> Response {
    let page = &host.state.page;
    let code = uri
        .query()
        .and_then(|query| query.strip_prefix("code="))
        .unwrap_or_default();
    let Some(name) = page.cookie.get() else {
        return signed_out();
    };
    if !page.redeem(code) {
        return signed_out();
    }
    let token = page.issue();
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!("{name}={token}; Path=/; HttpOnly; SameSite=Strict")
            .parse()
            .unwrap(),
    );
    response
}

async fn app_js(State(host): State<Arc<Host>>, headers: HeaderMap) -> Response {
    if host.state.page.session(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../desktop/ui/app.js"),
    )
}

async fn transport_js(State(host): State<Arc<Host>>, headers: HeaderMap) -> Response {
    if host.state.page.session(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../desktop/ui/transport.js"),
    )
}

async fn session(State(host): State<Arc<Host>>, headers: HeaderMap) -> Response {
    match host.state.page.session(&headers) {
        Some(session) => Json(json!({"csrf_token": session.csrf, "local": true})).into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// The page's calls run as the window: the same operations, checks and authority.
async fn command(
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
    Json(request): Json<control::Request>,
) -> Response {
    let Some(session) = host.state.page.session(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let csrf = headers.get("x-pta-csrf").and_then(|h| h.to_str().ok());
    if !csrf.is_some_and(|csrf| security::constant_eq(csrf, &session.csrf)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    Json(authority::from_window(&host.state, Box::pin(control::dispatch(&host, request))).await)
        .into_response()
}

#[cfg(test)]
mod tests;
