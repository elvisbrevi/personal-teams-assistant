use super::*;
use crate::app::variables::tests::{create, text};
use authority::Transport::{Local, Window};
use axum::body::Body;
use control::Reply;
use tower::ServiceExt;

thread_local! {
    /// What the test shells handed the browser on this test's thread, newest last.
    static OPENED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A desktop shell that records what it hands the browser instead of opening one.
struct Desktop {
    browser: bool,
}
impl Shell for Desktop {
    fn headless(&self) -> bool {
        true
    }
    fn show(&self) -> Result<()> {
        Err(SetupError("no window").into())
    }
    fn hide(&self) -> Result<()> {
        self.show()
    }
    fn open_url(&self, url: &str) -> Result<()> {
        OPENED.with(|opened| opened.borrow_mut().push(url.to_owned()));
        Ok(())
    }
    fn browser(&self) -> bool {
        self.browser
    }
    fn exit(&self) {}
}

fn opened() -> Vec<String> {
    OPENED.with(|opened| opened.borrow().clone())
}

/// A host of the owner's own profile whose page listens on `port` (not bound: requests go
/// straight to the router).
fn host(root: &Path, port: u16, browser: bool) -> Arc<Host> {
    let host = Arc::new(Host {
        state: init_state(&root.join("profile"), &root.join("data")).unwrap(),
        shell: Box::new(Desktop { browser }),
    });
    let page = &host.state.page;
    page.address
        .set(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .unwrap();
    page.cookie
        .set(cookie_name(&host.state.authority.profile))
        .unwrap();
    host
}

async fn send(host: &Arc<Host>, request: axum::http::Request<Body>) -> Response {
    router(host.clone()).oneshot(request).await.unwrap()
}

fn get(host: &Arc<Host>, path: &str) -> axum::http::request::Builder {
    let address = host.state.page.address.get().unwrap();
    axum::http::Request::get(path).header(header::HOST, address.to_string())
}

async fn body(response: Response) -> String {
    String::from_utf8(
        axum::body::to_bytes(response.into_body(), 4_000_000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// Opens the page the way the host does and returns the code it handed the browser.
async fn open_code(host: &Arc<Host>) -> String {
    *host.state.page.opened.lock().unwrap() = None;
    let reply = control::dispatch(host, control::Request::new("app_open")).await;
    assert!(reply.ok, "{:?}", reply.message);
    let url = opened().last().cloned().unwrap();
    let code = url.split_once("/open?code=").unwrap().1.to_owned();
    // The reply names no code, cookie or address with the code.
    assert!(!text(&reply).contains(&code));
    assert!(!text(&reply).contains("/open"));
    code
}

/// Exchanges a code for a session cookie (`name=value`).
async fn sign_in(host: &Arc<Host>) -> String {
    let code = open_code(host).await;
    let response = send(
        host,
        get(host, &format!("/open?code={code}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    cookie.split(';').next().unwrap().to_owned()
}

async fn csrf(host: &Arc<Host>, cookie: &str) -> String {
    let response = send(
        host,
        get(host, "/api/session")
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let session: Value = serde_json::from_str(&body(response).await).unwrap();
    assert_eq!(session["local"], true);
    session["csrf_token"].as_str().unwrap().to_owned()
}

fn control_request(host: &Arc<Host>) -> axum::http::request::Builder {
    let address = host.state.page.address.get().unwrap();
    axum::http::Request::post("/api/control")
        .header(header::HOST, address.to_string())
        .header(header::ORIGIN, format!("http://{address}"))
        .header(header::CONTENT_TYPE, "application/json")
}

async fn page_call(host: &Arc<Host>, cookie: &str, method: &str, args: Value) -> Reply {
    let csrf = csrf(host, cookie).await;
    let response = send(
        host,
        control_request(host)
            .header(header::COOKIE, cookie)
            .header("x-pta-csrf", csrf)
            .body(Body::from(
                json!({"method":method,"args":args,"contract":1}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_str(&body(response).await).unwrap()
}

async fn call(
    host: &Arc<Host>,
    transport: authority::Transport,
    method: &str,
    args: Value,
) -> Reply {
    crate::app::variables::tests::call(host, transport, method, args).await
}

#[tokio::test]
async fn the_host_opens_its_page_with_a_single_use_code_for_an_http_only_strict_session() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), 21_001, true);
    let code = open_code(&host).await;
    assert_eq!(code.len(), 64);
    let url = opened()[0].clone();
    assert_eq!(url, format!("http://127.0.0.1:21001/open?code={code}"));

    let response = send(
        &host,
        get(&host, &format!("/open?code={code}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    // The code leaves the address bar at once.
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/");
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    let name = cookie_name(&host.state.authority.profile);
    assert!(cookie.starts_with(&format!("{name}=")), "{cookie}");
    assert!(cookie.contains("; HttpOnly"));
    assert!(cookie.contains("; SameSite=Strict"));
    assert!(cookie.contains("; Path=/"));
    // A browser-session cookie: closing the browser ends the session.
    assert!(!cookie.contains("Max-Age") && !cookie.contains("Expires"));
    assert!(!cookie.contains(&code));
    let session = cookie.split(';').next().unwrap().to_owned();

    let page = send(
        &host,
        get(&host, "/")
            .header(header::COOKIE, &session)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CACHE_CONTROL], "no-store");
    assert!(page.headers().contains_key(header::CONTENT_SECURITY_POLICY));
    assert!(
        body(page)
            .await
            .contains("<meta name=\"pta-transport\" content=\"local\">")
    );

    // The same code a second time opens nothing.
    let replay = send(
        &host,
        get(&host, &format!("/open?code={code}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    assert!(!replay.headers().contains_key(header::SET_COOKIE));
    let signed_out = body(replay).await;
    assert!(signed_out.contains("pta app open"));
    assert!(!signed_out.contains("/open?code"));
}

#[tokio::test]
async fn forged_expired_and_missing_codes_and_sessions_open_nothing() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), 21_002, true);
    let code = open_code(&host).await;
    let mut forged = code.clone();
    forged.replace_range(..1, if code.starts_with('0') { "1" } else { "0" });
    for query in [
        format!("/open?code={forged}"),
        format!(
            "/open?code={}",
            code.to_uppercase().replace(char::is_numeric, "g")
        ),
        "/open".into(),
        "/open?code=".into(),
        format!("/open?other=1&code={code}"),
    ] {
        let response = send(&host, get(&host, &query).body(Body::empty()).unwrap()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{query}");
        assert!(!response.headers().contains_key(header::SET_COOKIE));
    }
    // An expired code is refused even though it was never used.
    for entry in host.state.page.codes.lock().unwrap().iter_mut() {
        entry.1 = Instant::now() - Duration::from_secs(1);
    }
    let expired = send(
        &host,
        get(&host, &format!("/open?code={code}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);

    // Without a session neither the document, the scripts nor the API answer.
    for path in [
        "/",
        "/index.html",
        "/app.js",
        "/transport.js",
        "/api/session",
    ] {
        let response = send(&host, get(&host, path).body(Body::empty()).unwrap()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    let forged_cookie = format!(
        "{}={}",
        cookie_name(&host.state.authority.profile),
        random()
    );
    let response = send(
        &host,
        get(&host, "/api/session")
            .header(header::COOKIE, forged_cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // An expired session ends.
    let cookie = sign_in(&host).await;
    for session in host.state.page.sessions.lock().unwrap().values_mut() {
        session.expires = Instant::now() - Duration::from_secs(1);
    }
    let response = send(
        &host,
        get(&host, "/api/session")
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn foreign_hosts_origins_and_sites_are_refused_and_writes_need_the_csrf_token() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), 21_003, true);
    let cookie = sign_in(&host).await;
    // DNS rebinding: any Host but this exact loopback address.
    for name in [
        "localhost:21003",
        "evil.example:21003",
        "127.0.0.1:21004",
        "127.0.0.1",
        "[::1]:21003",
    ] {
        let response = send(
            &host,
            axum::http::Request::get("/api/session")
                .header(header::HOST, name)
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST, "{name}");
    }
    // A foreign Origin, or a fetch from another site (another port of 127.0.0.1 is the same
    // site, so cookies would travel), is refused before any handler.
    for (name, value) in [
        ("origin", "http://evil.example"),
        ("origin", "http://127.0.0.1:21004"),
        ("origin", "http://localhost:21003"),
        ("origin", "null"),
        ("sec-fetch-site", "same-site"),
        ("sec-fetch-site", "cross-site"),
    ] {
        let response = send(
            &host,
            get(&host, "/api/session")
                .header(header::COOKIE, &cookie)
                .header(name, value)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{name}: {value}");
    }
    let csrf = csrf(&host, &cookie).await;
    let body_of = || Body::from(json!({"method":"snapshot","args":{},"contract":1}).to_string());
    // A write without Origin, with another Origin, or without the exact CSRF token.
    let address = host.state.page.address.get().unwrap().to_string();
    let without_origin = axum::http::Request::post("/api/control")
        .header(header::HOST, &address)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &cookie)
        .header("x-pta-csrf", &csrf)
        .body(body_of())
        .unwrap();
    assert_eq!(
        send(&host, without_origin).await.status(),
        StatusCode::FORBIDDEN
    );
    for token in [None, Some("wrong"), Some(&csrf[1..])] {
        let mut request = control_request(&host).header(header::COOKIE, &cookie);
        if let Some(token) = token {
            request = request.header("x-pta-csrf", token);
        }
        let response = send(&host, request.body(body_of()).unwrap()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{token:?}");
    }
    let mut request = control_request(&host)
        .header("x-pta-csrf", &csrf)
        .body(body_of())
        .unwrap();
    request.headers_mut().remove(header::COOKIE);
    assert_eq!(
        send(&host, request).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let reply = page_call(&host, &cookie, "snapshot", json!({})).await;
    assert!(reply.ok, "{:?}", reply.message);
}

#[tokio::test]
async fn the_page_has_the_windows_authority_and_the_control_token_still_does_not() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), 21_005, true);
    let body = create("PAGE_REVEALED", "synthetic-page-value-9f8e7d");
    let entry = body["id"].as_str().unwrap().to_owned();
    assert!(call(&host, Local, "variable_create", body).await.ok);
    let cookie = sign_in(&host).await;

    // The person at the page reveals, like the window.
    let reveal = page_call(&host, &cookie, "variable_reveal", json!({"id":entry})).await;
    assert!(reveal.ok, "{:?}", reveal.message);
    assert_eq!(reveal.data["value"], "synthetic-page-value-9f8e7d");
    let window = call(&host, Window, "variable_reveal", json!({"id":entry})).await;
    assert_eq!(window.data["value"], reveal.data["value"]);

    // The control endpoint's local token (the CLI, MCP, tools and agents) never reveals or
    // approves, even while a page session is open.
    let local = call(&host, Local, "variable_reveal", json!({"id":entry})).await;
    assert!(!local.ok);
    assert!(!text(&local).contains("synthetic-page-value"));
    let launcher = json!({"id":id(),"digest":"0".repeat(64)});
    let refused = call(&host, Local, "variable_launcher_approve", launcher.clone()).await;
    assert!(!refused.ok);
    assert!(
        text(&refused).contains("Only the user approves"),
        "{}",
        text(&refused)
    );
    let refused = call(&host, Local, "variable_delivery_approve", launcher.clone()).await;
    assert!(!refused.ok);
    assert!(
        text(&refused).contains("Only the user approves"),
        "{}",
        text(&refused)
    );
    // From the page, approval passes the transport check (and then fails on the unknown id).
    for method in ["variable_launcher_approve", "variable_delivery_approve"] {
        let reply = page_call(&host, &cookie, method, launcher.clone()).await;
        assert!(!reply.ok);
        assert!(
            !text(&reply).contains("Only the user approves"),
            "{method}: {}",
            text(&reply)
        );
    }

    // Opening the page from the local channel gives the caller no way in: the reply, the logs
    // and the diagnostics never carry the code, the cookie or the address with the code.
    *host.state.page.opened.lock().unwrap() = None;
    let reply = call(&host, Local, "app_open", json!({})).await;
    assert!(reply.ok, "{:?}", reply.message);
    let url = opened().last().cloned().unwrap();
    let code = url.split_once("code=").unwrap().1.to_owned();
    let token = cookie.split_once('=').unwrap().1.to_owned();
    for method in ["logs", "audit", "snapshot"] {
        let reply = call(&host, Local, method, json!({})).await;
        let reply = text(&reply);
        for secret in [&code, &token, &url] {
            assert!(!reply.contains(secret.as_str()), "{method}");
        }
    }
    // The control token presented to the page is not a session.
    let address = host.state.page.address.get().unwrap().to_string();
    let bearer = axum::http::Request::post("/api/control")
        .header(header::HOST, &address)
        .header(header::ORIGIN, format!("http://{address}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer synthetic-control-token")
        .body(Body::from(
            json!({"method":"variable_reveal","args":{"id":entry},"contract":1}).to_string(),
        ))
        .unwrap();
    assert_eq!(send(&host, bearer).await.status(), StatusCode::UNAUTHORIZED);
}

fn id() -> String {
    crate::app::variables::tests::id()
}

#[tokio::test]
async fn without_a_browser_session_opening_is_not_ready_and_prints_no_link() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), 21_006, false);
    let reply = call(&host, Local, "app_open", json!({})).await;
    assert!(!reply.ok);
    assert_eq!(reply.code, "not_ready");
    assert_eq!(reply.message.as_deref(), Some(NO_BROWSER));
    assert!(opened().is_empty());
    assert!(host.state.page.codes.lock().unwrap().is_empty());
    assert!(!text(&reply).contains("http://"));

    // A host without its page (a portal account's host, or no free port) says so.
    let other = tempfile::tempdir().unwrap();
    let unbound = Arc::new(Host {
        state: init_state(&other.path().join("profile"), &other.path().join("data")).unwrap(),
        shell: Box::new(Desktop { browser: true }),
    });
    let reply = call(&unbound, Local, "app_open", json!({})).await;
    assert_eq!(reply.code, "not_ready");
    assert!(opened().is_empty());
}

#[tokio::test]
async fn opening_is_rate_limited_and_old_codes_are_dropped() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), 21_007, true);
    let first = open_code(&host).await;
    let again = call(&host, Local, "app_open", json!({})).await;
    assert!(!again.ok);
    assert_eq!(opened().len(), 1);
    for _ in 0..MAX_CODES {
        open_code(&host).await;
    }
    assert_eq!(host.state.page.codes.lock().unwrap().len(), MAX_CODES);
    // The oldest code was dropped and no longer opens a session.
    let response = send(
        &host,
        get(&host, &format!("/open?code={first}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn each_profile_has_its_own_address_cookie_and_sessions() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let a = host(first.path(), 21_008, true);
    let b = host(second.path(), 21_009, true);
    let (pa, pb) = (&a.state.authority.profile, &b.state.authority.profile);
    // Stable: the same profile derives the same candidates and cookie on every start.
    assert_eq!(candidates(pa), candidates(pa));
    assert_eq!(candidates(pa).len(), CANDIDATES);
    assert!(candidates(pa).iter().all(|port| PORTS.contains(port)));
    assert_ne!(candidates(pa), candidates(pb));
    assert_eq!(cookie_name(pa), cookie_name(pa));
    assert_ne!(cookie_name(pa), cookie_name(pb));
    // A session of one profile's page is nothing to the other's, even under its cookie name.
    let cookie = sign_in(&a).await;
    let token = cookie.split_once('=').unwrap().1;
    let foreign = format!("{}={token}", cookie_name(pb));
    let response = send(
        &b,
        get(&b, "/api/session")
            .header(header::COOKIE, foreign)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // A restarted host starts with no sessions.
    let restarted = Arc::new(Host {
        state: init_state(&first.path().join("profile"), &first.path().join("data")).unwrap(),
        shell: Box::new(Desktop { browser: true }),
    });
    assert!(restarted.state.page.sessions.lock().unwrap().is_empty());
    assert!(!restarted.state.page.available());
}

#[tokio::test]
async fn the_page_binds_loopback_only_and_never_for_a_portal_account() {
    let root = tempfile::tempdir().unwrap();
    let owner = Arc::new(Host {
        state: init_state(&root.path().join("profile"), &root.path().join("data")).unwrap(),
        shell: Box::new(Desktop { browser: true }),
    });
    let server = launch(owner.clone()).expect("the owner's profile serves its page");
    let address = *owner.state.page.address.get().unwrap();
    assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
    assert!(candidates(&owner.state.authority.profile).contains(&address.port()));
    let task = tokio::spawn(server);
    // A real request reaches the listener and is refused without a session.
    let response = reqwest::Client::new()
        .get(format!("http://{address}/api/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    task.abort();

    // A portal account's isolated profile (web/profiles/<id>) never gets a page.
    let portal = tempfile::tempdir().unwrap();
    let profile = portal.path().join("web/profiles/synthetic");
    let shared = Arc::new(Host {
        state: init_state(&profile, &profile.join("data")).unwrap(),
        shell: Box::new(Desktop { browser: true }),
    });
    assert!(shared.state.authority.shared);
    assert!(launch(shared.clone()).is_none());
    assert!(!shared.state.page.available());
}
