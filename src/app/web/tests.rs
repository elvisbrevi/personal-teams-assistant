use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use serde_json::Value;
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn fixture() -> (tempfile::TempDir, Arc<Portal>, Vec<Account>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("profile/web");
    security::private_dir(&root).unwrap();
    let accounts: Vec<_> = ["alice", "bob"]
        .into_iter()
        .map(|name| Account {
            id: uuid::Uuid::new_v4().to_string(),
            username: name.into(),
            password_hash: "retired-synthetic-hash".into(),
            access_binding: Some(access::Binding {
                issuer: access::tests::config().issuer,
                idp_id: access::tests::config().github_idp_id,
                provider_user_id: if name == "alice" { "1001" } else { "1002" }.into(),
            }),
            access_valid_after: 0,
            version: uuid::Uuid::new_v4().to_string(),
            enabled: true,
            current_profile: false,
        })
        .collect();
    for entry in &accounts {
        create_profile(&root, entry).unwrap();
    }
    write_private(
        &root.join("accounts.json"),
        &serde_json::to_string(&accounts).unwrap(),
    )
    .unwrap();
    write_private(
        &root.join("settings.json"),
        &serde_json::to_string(&Settings {
            owner_github_id: Some(1001),
            ..Settings::default()
        })
        .unwrap(),
    )
    .unwrap();
    let portal = Arc::new(Portal::new(root, PathBuf::from("/unused-test-host")).unwrap());
    (dir, portal, accounts)
}

#[tokio::test]
async fn machine_transport_rejects_browser_origins_cookies_and_missing_credentials() {
    let (_dir, portal, _entries, _server) = configured_fixture().await;
    assert_eq!(
        send(
            &portal,
            "POST",
            "/api/machines",
            None,
            None,
            None,
            json!({"action":"poll"})
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &portal,
            "POST",
            "/api/machines",
            None,
            None,
            Some("https://evil.example"),
            json!({"action":"register"})
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    for path in ["/api/machines/extra", "/api/machines/register"] {
        assert_eq!(
            send(&portal, "POST", path, None, None, None, json!({}))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        send(
            &portal,
            "GET",
            "/api/machines",
            None,
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn only_the_local_owner_can_login_even_if_another_account_is_enabled_and_bound() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let other = TestSession {
        cookie: String::new(),
        assertion: assertion(&entries[1]),
    };
    assert_eq!(
        send(
            &portal,
            "GET",
            "/login",
            Some(&other),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let (forged, _) = session_cookie(&portal, &entries[1]).await;
    assert_eq!(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&forged),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(portal.worker(&entries[1]).await.is_err());
    assert!(
        portal
            .check_request(&entries[1], &control::Request::new("snapshot"))
            .is_err()
    );
}

#[test]
fn pairing_is_single_use_machine_bound_private_and_local_to_an_installation() {
    let (dir, portal, entries) = fixture();
    let config = Settings {
        owner_github_id: Some(1001),
        public_url: "https://assistant.example.com".into(),
        access: Some(access::tests::config()),
        ..Settings::default()
    };
    write_private(
        &portal.root.join("settings.json"),
        &serde_json::to_string(&config).unwrap(),
    )
    .unwrap();
    let machine = uuid::Uuid::new_v4().to_string();
    let file = dir.path().join("pairing.json");
    let reply = store::authorize_machine(&portal.root, &machine, &file).unwrap();
    let payload: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let token = payload["pairing_token"].as_str().unwrap();
    assert!(!reply.to_string().contains(token));
    assert!(
        !fs::read_to_string(portal.root.join("machine-pairings.json"))
            .unwrap()
            .contains(token)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(store::authorize_machine(&portal.root, &machine, &file).is_err());
    assert!(
        store::consume_pairing(
            &portal.root,
            &config,
            token,
            &uuid::Uuid::new_v4().to_string()
        )
        .unwrap()
        .is_none()
    );
    let (_other_dir, other_portal, _) = fixture();
    assert!(
        store::consume_pairing(&other_portal.root, &config, token, &machine)
            .unwrap()
            .is_none()
    );
    let account = store::consume_pairing(&portal.root, &config, token, &machine)
        .unwrap()
        .unwrap();
    assert_eq!(account.id, entries[0].id);
    assert!(
        store::consume_pairing(&portal.root, &config, token, &machine)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn machine_registration_accepts_only_a_locally_approved_pairing_and_never_github_tokens() {
    let (dir, portal, entries, _server) = configured_fixture().await;
    let (host, _lock) = fake_host(&portal, &entries[0]).await;
    let settings = Settings {
        public_url: "https://assistant.example.com".into(),
        access: Some(access::tests::config()),
        ..portal.settings.clone()
    };
    write_private(
        &portal.root.join("settings.json"),
        &serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    let machine = uuid::Uuid::new_v4().to_string();
    let file = dir.path().join("pairing.json");
    store::authorize_machine(&portal.root, &machine, &file).unwrap();
    let payload: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    let token = payload["pairing_token"].as_str().unwrap();
    for (credential, status) in [
        ("ghp_synthetic_github_token", StatusCode::FORBIDDEN),
        (token, StatusCode::OK),
        (token, StatusCode::FORBIDDEN),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri("/api/machines")
            .header(header::HOST, "localhost:38656")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {credential}"))
            .body(Body::from(
                json!({"action":"register","id":machine,"name":"Synthetic","os":"linux"})
                    .to_string(),
            ))
            .unwrap();
        assert_eq!(
            router(portal.clone())
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            status
        );
    }
    let requests = host.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(request["method"], "machine_register");
    assert!(!request.to_string().contains(token));
}

#[test]
fn default_and_ambiguous_legacy_settings_have_no_owner_or_vendor_url() {
    let config = Settings::default();
    assert!(config.owner_github_id.is_none());
    assert_eq!(config.public_url, "http://localhost:38656");
    let (_dir, portal, _) = fixture();
    write_private(&portal.root.join("settings.json"), "{}").unwrap();
    assert!(settings(&portal.root).unwrap().owner_github_id.is_none());
    store::edit_accounts(&portal.root, |entries| {
        entries[1].access_binding = None;
        Ok(())
    })
    .unwrap();
    assert_eq!(settings(&portal.root).unwrap().owner_github_id, Some(1001));
}

#[test]
fn expired_revoked_disabled_and_wrong_owner_pairings_are_rejected() {
    for rejection in ["expired", "revoked", "disabled", "owner"] {
        let (dir, portal, _) = fixture();
        let mut config = Settings {
            public_url: "https://assistant.example.com".into(),
            access: Some(access::tests::config()),
            ..portal.settings.clone()
        };
        write_private(
            &portal.root.join("settings.json"),
            &serde_json::to_string(&config).unwrap(),
        )
        .unwrap();
        let machine = uuid::Uuid::new_v4().to_string();
        let file = dir.path().join("pairing.json");
        store::authorize_machine(&portal.root, &machine, &file).unwrap();
        let payload: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        let token = payload["pairing_token"].as_str().unwrap();
        match rejection {
            "expired" => {
                let path = portal.root.join("machine-pairings.json");
                let mut entries: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                for value in entries.as_object_mut().unwrap().values_mut() {
                    value["expires_at"] = json!(access::now());
                }
                write_private(&path, &entries.to_string()).unwrap();
            }
            "revoked" => store::edit_accounts(&portal.root, |entries| {
                entries[0].version = uuid::Uuid::new_v4().to_string();
                Ok(())
            })
            .unwrap(),
            "disabled" => store::edit_accounts(&portal.root, |entries| {
                entries[0].enabled = false;
                Ok(())
            })
            .unwrap(),
            "owner" => config.owner_github_id = Some(1002),
            _ => unreachable!(),
        }
        assert!(
            store::consume_pairing(&portal.root, &config, token, &machine)
                .unwrap()
                .is_none(),
            "{rejection}"
        );
    }
}

#[tokio::test]
async fn https_responses_include_hostname_only_hsts() {
    let (_dir, mut portal, _, _) = configured_fixture().await;
    Arc::get_mut(&mut portal).unwrap().settings.public_url = "https://localhost:38656".into();
    let response = send(
        &portal,
        "GET",
        "/favicon.png",
        None,
        None,
        None,
        Value::Null,
    )
    .await;
    assert_eq!(
        response.headers()[header::STRICT_TRANSPORT_SECURITY],
        "max-age=31536000"
    );
}
async fn send(
    portal: &Arc<Portal>,
    method: &str,
    uri: &str,
    cookie: Option<&TestSession>,
    csrf: Option<&str>,
    origin: Option<&str>,
    input: Value,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "localhost:38656");
    if let Some(auth) = cookie {
        request = request.header("cf-access-jwt-assertion", &auth.assertion);
        if !auth.cookie.is_empty() {
            request = request.header(header::COOKIE, &auth.cookie);
        }
    }
    if let Some(csrf) = csrf {
        request = request.header("x-pta-csrf", csrf);
    }
    if let Some(origin) = origin {
        request = request.header(header::ORIGIN, origin);
    }
    let body = if input.is_null() {
        Body::empty()
    } else {
        request = request.header(header::CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&input).unwrap())
    };
    router(portal.clone())
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap()
}
async fn json_body(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 2_000_000).await.unwrap()).unwrap()
}
struct TestSession {
    cookie: String,
    assertion: String,
}
async fn configured_fixture() -> (tempfile::TempDir, Arc<Portal>, Vec<Account>, MockServer) {
    let (dir, mut portal, entries) = fixture();
    let (verifier, server) = access::tests::mock_verifier().await;
    let bindings: HashMap<_, _> = entries
        .iter()
        .map(|e| {
            (
                e.id.clone(),
                e.access_binding.as_ref().unwrap().provider_user_id.clone(),
            )
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/cdn-cgi/access/get-identity"))
        .respond_with(move |request: &wiremock::Request| {
            use base64::Engine;
            let token = request.headers["cookie"]
                .to_str()
                .unwrap()
                .strip_prefix("CF_Authorization=")
                .unwrap();
            let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(token.split('.').nth(1).unwrap())
                .unwrap();
            let claims: Value = serde_json::from_slice(&payload).unwrap();
            let subject = claims["sub"].as_str().unwrap();
            ResponseTemplate::new(200).set_body_json(access::tests::identity(
                subject,
                bindings
                    .get(subject)
                    .map(String::as_str)
                    .unwrap_or("unassociated"),
            ))
        })
        .mount(&server)
        .await;
    Arc::get_mut(&mut portal).unwrap().access = Some(verifier);
    (dir, portal, entries, server)
}
fn assertion(account: &Account) -> String {
    access::tests::token(&access::tests::claims(&account.id), "synthetic-key")
}
async fn session_cookie(portal: &Arc<Portal>, account: &Account) -> (TestSession, String) {
    let assertion = assertion(account);
    let identity = portal
        .access
        .as_ref()
        .unwrap()
        .verify(&access::tests::headers(&assertion))
        .await
        .unwrap();
    let (token, session) = portal.issue_session(account, identity).await.unwrap();
    (
        TestSession {
            cookie: format!("{}={token}", portal.settings.cookie_name()),
            assertion,
        },
        session.csrf,
    )
}
async fn fake_host(portal: &Portal, account: &Account) -> (MockServer, fs::File) {
    let server = MockServer::start().await;
    let dir = profile_dir(&portal.root, account);
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("control.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let port = url::Url::parse(&server.uri()).unwrap().port().unwrap();
    write_private(
        &dir.join("control.json"),
        &json!({"contract":1,"port":port,"token":"synthetic-ipc-token",
        "wiki_support":true,"activity_registration_support":true,"web_support":true,"web_access_support":true,"implementation_support":true,"machine_pairing_support":true})
        .to_string(),
    )
    .unwrap();
    Mock::given(method("POST"))
        .and(path("/control"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(control::Reply::success(json!({"owner":account.username}))),
        )
        .mount(&server)
        .await;
    (server, lock)
}

#[tokio::test]
async fn access_bootstraps_the_existing_profile_without_a_password_form() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    for uri in [
        "/",
        "/login",
        "/api/session",
        "/app.js",
        "/i18n.js",
        "/transport.js",
        "/style.css",
    ] {
        assert_eq!(
            send(&portal, "GET", uri, None, None, None, Value::Null)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let mut auth = TestSession {
        cookie: String::new(),
        assertion: assertion(&entries[0]),
    };
    let response = send(
        &portal,
        "GET",
        "/login",
        Some(&auth),
        None,
        None,
        Value::Null,
    )
    .await;
    // Finish the cross-site OAuth navigation before SameSite=Strict is needed.
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::LOCATION).is_none());
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    auth.cookie = cookie.split(';').next().unwrap().into();
    let body = to_bytes(response.into_body(), 2_000_000).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("transport.js"));
    assert!(!String::from_utf8_lossy(&body).contains("web-password-form"));
    let data = json_body(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&auth),
            None,
            None,
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(data["username"], "alice");
    assert_eq!(data["auth_provider"], "github");
    assert!(data.get("password_hash").is_none());
    let page = send(&portal, "GET", "/", Some(&auth), None, None, Value::Null).await;
    let body = to_bytes(page.into_body(), 2_000_000).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("web-password-form"));
    for uri in ["/api/login", "/api/password"] {
        assert_eq!(
            send(
                &portal,
                "POST",
                uri,
                Some(&auth),
                None,
                Some("http://localhost:38656"),
                json!({"password":"synthetic"})
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
    }
    let unknown = Account {
        id: uuid::Uuid::new_v4().to_string(),
        ..entries[0].clone()
    };
    auth.assertion = assertion(&unknown);
    assert_eq!(
        send(
            &portal,
            "GET",
            "/login",
            Some(&auth),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    // Same email in both JWTs does not let a different provider subject use the profile.
    assert_eq!(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&auth),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_pwa_manifest_and_its_icons_are_served_without_access() {
    let (_dir, portal, _entries, _server) = configured_fixture().await;
    // WebKit fetches home-screen icons without cookies, so they answer without an Access JWT.
    for (uri, kind) in [
        ("/favicon.png", "image/png"),
        ("/manifest.webmanifest", "application/manifest+json"),
        ("/apple-touch-icon.png", "image/png"),
        ("/icon-192.png", "image/png"),
        ("/icon-512.png", "image/png"),
    ] {
        let response = send(&portal, "GET", uri, None, None, None, Value::Null).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        assert_eq!(response.headers()[header::CONTENT_TYPE], kind, "{uri}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
    let manifest = json_body(
        send(
            &portal,
            "GET",
            "/manifest.webmanifest",
            None,
            None,
            None,
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(manifest["start_url"], "/");
    // Every icon the manifest names is served, and only these exact GETs skip Access.
    for icon in manifest["icons"].as_array().unwrap() {
        let uri = format!("/{}", icon["src"].as_str().unwrap());
        let response = send(&portal, "GET", &uri, None, None, None, Value::Null).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
    for (method, uri) in [
        ("POST", "/favicon.png"),
        ("GET", "/favicon.png/extra"),
        ("GET", "/icon-1024.png"),
        ("GET", "/style.css"),
    ] {
        assert_eq!(
            send(&portal, method, uri, None, None, None, Value::Null)
                .await
                .status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri}"
        );
    }
    // The Access helper creates a Bypass app for exactly these paths.
    let helper = include_str!("../../../scripts/configure-web-access.py");
    for path in PUBLIC_ASSETS {
        assert!(helper.contains(&format!("\"{path}\"")), "{path}");
    }
}

#[tokio::test]
async fn commands_are_session_scoped_and_require_csrf_even_for_reads() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let (_alice, _alice_lock) = fake_host(&portal, &entries[0]).await;
    let (_bob, _bob_lock) = fake_host(&portal, &entries[1]).await;
    let request = serde_json::to_value(control::Request::new("snapshot")).unwrap();
    for entry in entries.iter().take(1) {
        let (cookie, csrf) = session_cookie(&portal, entry).await;
        for (token, origin) in [
            (None, Some("http://localhost:38656")),
            (Some(csrf.as_str()), Some("https://evil.example")),
            (Some(csrf.as_str()), None),
        ] {
            assert_eq!(
                send(
                    &portal,
                    "POST",
                    "/api/control",
                    Some(&cookie),
                    token,
                    origin,
                    request.clone()
                )
                .await
                .status(),
                StatusCode::FORBIDDEN
            );
        }
        let data = json_body(
            send(
                &portal,
                "POST",
                "/api/control",
                Some(&cookie),
                Some(&csrf),
                Some("http://localhost:38656"),
                request.clone(),
            )
            .await,
        )
        .await;
        assert_eq!(data["data"]["owner"], entry.username);
        let mut forged = request.clone();
        forged["args"] =
            json!({"user":entries[1].id,"profile":profile_dir(&portal.root,&entries[1])});
        let data = json_body(
            send(
                &portal,
                "POST",
                "/api/control",
                Some(&cookie),
                Some(&csrf),
                Some("http://localhost:38656"),
                forged,
            )
            .await,
        )
        .await;
        assert_eq!(data["data"]["owner"], entry.username);
    }
    assert_ne!(
        profile_dir(&portal.root, &entries[0]),
        profile_dir(&portal.root, &entries[1])
    );
    let (mut alice, _) = session_cookie(&portal, &entries[0]).await;
    alice.assertion = assertion(&entries[1]);
    assert_eq!(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&alice),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn logout_revokes_local_sessions_and_the_access_assertion_even_after_restart() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let (cookie, csrf) = session_cookie(&portal, &entries[0]).await;
    assert_eq!(
        send(
            &portal,
            "POST",
            "/api/logout",
            Some(&cookie),
            None,
            Some("http://localhost:38656"),
            json!({})
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let response = send(
        &portal,
        "POST",
        "/api/logout",
        Some(&cookie),
        Some(&csrf),
        Some("http://localhost:38656"),
        json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        json_body(response).await["logout_url"],
        "/cdn-cgi/access/logout"
    );
    for uri in ["/api/session", "/login"] {
        assert_eq!(
            send(&portal, "GET", uri, Some(&cookie), None, None, Value::Null)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let identity = portal
        .access
        .as_ref()
        .unwrap()
        .verify(&access::tests::headers(&cookie.assertion))
        .await
        .unwrap();
    assert!(store::access_revoked(&portal.root, &identity).unwrap());
    let new_portal = Portal::new(portal.root.clone(), PathBuf::from("/unused")).unwrap();
    assert!(store::access_revoked(&new_portal.root, &identity).unwrap());
    assert!(
        !fs::read_to_string(portal.root.join("access-revocations.json"))
            .unwrap()
            .contains(&cookie.assertion)
    );
}

#[tokio::test]
async fn disabling_expiry_rebinding_and_local_revocation_are_checked_on_every_request() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let (cookie, _) = session_cookie(&portal, &entries[0]).await;
    store::edit_accounts(&portal.root, |entries| {
        entries[0].enabled = false;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&cookie),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &portal,
            "GET",
            "/login",
            Some(&cookie),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let (cookie, _) = session_cookie(&portal, &entries[1]).await;
    portal
        .sessions
        .lock()
        .await
        .values_mut()
        .for_each(|s| s.expires = Instant::now() - Duration::from_secs(1));
    assert_eq!(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&cookie),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let (cookie, _) = session_cookie(&portal, &entries[1]).await;
    store::edit_accounts(&portal.root, |entries| {
        entries[1].access_valid_after = access::now() + 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        send(
            &portal,
            "GET",
            "/login",
            Some(&cookie),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    store::edit_accounts(&portal.root, |entries| {
        entries[1].access_valid_after = 0;
        entries[1].access_binding = None;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&cookie),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..5 {
        assert!(portal.login_limit("limited").await);
    }
    assert!(!portal.login_limit("limited").await);
}

#[tokio::test]
async fn access_expiry_bounds_local_session_lifetime() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let mut value = access::tests::claims(&entries[0].id);
    value["exp"] = json!(access::now() + 10);
    let token = access::tests::token(&value, "synthetic-key");
    let identity = portal
        .access
        .as_ref()
        .unwrap()
        .verify(&access::tests::headers(&token))
        .await
        .unwrap();
    let (_, session) = portal.issue_session(&entries[0], identity).await.unwrap();
    assert!(session.expires.duration_since(Instant::now()) <= Duration::from_secs(10));
    let mut expired = value;
    expired["exp"] = json!(access::now() - 1);
    let auth = TestSession {
        cookie: String::new(),
        assertion: access::tests::token(&expired, "synthetic-key"),
    };
    assert_eq!(
        send(
            &portal,
            "GET",
            "/login",
            Some(&auth),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[test]
fn legacy_accounts_preserve_ids_current_profile_hashes_and_credentials_without_auto_binding() {
    let (dir, portal, entries) = fixture();
    let mut value = json!(entries);
    for e in value.as_array_mut().unwrap() {
        e.as_object_mut().unwrap().remove("access_binding");
        e.as_object_mut().unwrap().remove("access_valid_after");
    }
    value[0]["username"] = json!("elvis");
    value[0]["current_profile"] = json!(true);
    write_private(&portal.root.join("accounts.json"), &value.to_string()).unwrap();
    let loaded = accounts(&portal.root).unwrap();
    assert_eq!(loaded[0].id, entries[0].id);
    assert!(loaded[0].access_binding.is_none());
    assert_eq!(
        profile_dir(&portal.root, &loaded[0]),
        dir.path().join("profile")
    );
    let credential = profile_dir(&portal.root, &loaded[1]).join("data/credentials/SYNTHETIC_KEY");
    security::private_dir(credential.parent().unwrap()).unwrap();
    write_private(&credential, "synthetic-credential").unwrap();
    store::edit_accounts(&portal.root, |entries| {
        entries[0].access_binding = Some(access::Binding {
            issuer: access::tests::config().issuer,
            idp_id: access::tests::config().github_idp_id,
            provider_user_id: "123456".into(),
        });
        Ok(())
    })
    .unwrap();
    assert_eq!(
        accounts(&portal.root).unwrap()[0].password_hash,
        "retired-synthetic-hash"
    );
    assert_eq!(
        fs::read_to_string(credential).unwrap(),
        "synthetic-credential"
    );
    store::edit_accounts(&portal.root, |entries| {
        entries[1].access_binding = entries[0].access_binding.clone();
        Ok(())
    })
    .unwrap();
    assert!(accounts(&portal.root).is_err());
}

#[test]
fn browser_allows_shared_implementation_tools_but_rejects_internal_worker_operations() {
    let (_dir, portal, entries) = fixture();
    for tool in super::super::tools::TOOLS.iter().filter(|tool| {
        super::super::implementation::handles(tool.name)
            || matches!(tool.name, "azure_cli_status" | "github_cli_status")
    }) {
        assert!(
            portal
                .check_request(&entries[0], &control::Request::new(tool.name))
                .is_ok(),
            "{}",
            tool.name
        );
    }
    for method in [
        "implementation_launch",
        "implementation_local",
        "machine_register",
        "machine_protocol",
    ] {
        assert!(
            portal
                .check_request(&entries[0], &control::Request::new(method))
                .is_err(),
            "{method}"
        );
    }
}
#[test]
fn a_profile_cannot_import_or_save_another_users_paths_or_duplicate_teams_identity() {
    let (_dir, portal, entries) = fixture();
    let first = profile_dir(&portal.root, &entries[0]);
    let second = profile_dir(&portal.root, &entries[1]);
    let config = read_config(&first.join("config.toml")).unwrap();
    let map = KnowledgeMap::load(&config.knowledge_map).unwrap();
    let mut request = control::Request::new("save_settings");
    request.args = json!({"config":config,"map":map,"tunnelConfig":""});
    assert!(portal.check_request(&entries[0], &request).is_ok());
    request.args["config"]["server"]["data_dir"] = json!(second.join("data"));
    assert!(portal.check_request(&entries[0], &request).is_err());
    request.args["config"] = json!(config);
    security::private_dir(&first.join("data/repositories")).unwrap();
    security::private_dir(&second.join("data/repositories/private")).unwrap();
    request.args["map"]["repositories"] = json!({"other":second.join("data/repositories/private")});
    assert!(portal.check_request(&entries[0], &request).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            second.join("data/repositories/private"),
            first.join("data/repositories/escape"),
        )
        .unwrap();
        request.args["map"]["repositories"] =
            json!({"escape":first.join("data/repositories/escape")});
        assert!(portal.check_request(&entries[0], &request).is_err());
    }
    request.method = "import_existing".into();
    request.args = json!({"path":second.join("config.toml")});
    assert!(portal.check_request(&entries[0], &request).is_err());
    let mut identity = config.clone();
    identity.graph.user_id = uuid::Uuid::new_v4().to_string();
    let mut other = read_config(&second.join("config.toml")).unwrap();
    other.graph.user_id = identity.graph.user_id.clone();
    write_private(
        &second.join("config.toml"),
        &toml::to_string(&other).unwrap(),
    )
    .unwrap();
    assert!(portal.unique_identity(&entries[0], &identity).is_err());
    request = control::Request::new("app_quit");
    assert!(portal.check_request(&entries[0], &request).is_err());
}

#[test]
fn worker_credentials_and_provider_homes_are_not_inherited_from_the_operator() {
    let command =
        profiles::worker_command(Path::new("unused"), Path::new("/private/synthetic-profile"));
    let values: HashMap<_, _> = command
        .as_std()
        .get_envs()
        .map(|(name, value)| (name.to_owned(), value.map(std::ffi::OsStr::to_owned)))
        .collect();
    for name in [
        "DEEPSEEK_API_KEY",
        "STATE_ENCRYPTION_KEY",
        "GRAPH_WEBHOOK_SECRET",
        "GITHUB_OAUTH_TOKENS",
        "CLOUDFLARE_TUNNEL_TOKEN",
        "CLOUDFLARE_API_TOKEN",
        "PTA_ACCESS_GITHUB_CLIENT_SECRET",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
    ] {
        assert!(!values.contains_key(std::ffi::OsStr::new(name)));
    }
    assert_eq!(
        values[std::ffi::OsStr::new("HOME")].as_deref(),
        Some(std::ffi::OsStr::new("/private/synthetic-profile/home"))
    );
}

#[tokio::test]
async fn callbacks_route_only_to_the_matching_enabled_profile_without_administration_headers() {
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let (first, _first_lock) = fake_host(&portal, &entries[0]).await;
    let (second, _second_lock) = fake_host(&portal, &entries[1]).await;
    for (entry, server) in entries.iter().zip([&first, &second]) {
        let dir = profile_dir(&portal.root, entry);
        let mut config = read_config(&dir.join("config.toml")).unwrap();
        config.server.bind = server.address().to_string();
        write_private(&dir.join("config.toml"), &toml::to_string(&config).unwrap()).unwrap();
        Mock::given(method("POST"))
            .and(path("/graph/notifications"))
            .respond_with(ResponseTemplate::new(200).set_body_string(&entry.username))
            .mount(server)
            .await;
        let url = format!(
            "/webhooks/{}/graph/notifications?validationToken=synthetic",
            entry.id
        );
        let response = send(&portal, "POST", &url, None, None, None, json!({})).await;
        if entry.id == entries[1].id {
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert!(server.received_requests().await.unwrap().is_empty());
            continue;
        }
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert_eq!(body.as_ref(), entry.username.as_bytes());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests.last().unwrap().url.query(),
            Some("validationToken=synthetic")
        );
        assert!(
            !requests
                .last()
                .unwrap()
                .headers
                .contains_key("authorization")
        );
        assert!(!requests.last().unwrap().headers.contains_key("cookie"));
    }
    store::edit_accounts(&portal.root, |entries| {
        entries[0].enabled = false;
        Ok(())
    })
    .unwrap();
    let response = send(
        &portal,
        "POST",
        &format!("/webhooks/{}/graph/notifications", entries[0].id),
        None,
        None,
        None,
        json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[test]
fn remote_http_and_public_bindings_are_rejected_and_secure_cookies_are_host_only() {
    for settings in [
        Settings {
            public_url: "http://assistant.example.com".into(),
            ..Settings::default()
        },
        Settings {
            bind: "0.0.0.0:38656".into(),
            ..Settings::default()
        },
        Settings {
            public_url: "https://assistant.example.com/path".into(),
            ..Settings::default()
        },
    ] {
        assert!(settings.validate().is_err());
    }
    let config = Settings {
        public_url: "https://assistant.example.com".into(),
        ..Settings::default()
    };
    config.validate().unwrap();
    let cookie = config.cookie("synthetic", 43_200);
    assert!(
        cookie.starts_with("__Host-pta-session=")
            && cookie.contains("; Secure")
            && !cookie.contains("Domain=")
    );
    let upper = Settings {
        public_url: "HTTPS://assistant.example.com".into(),
        ..Settings::default()
    };
    upper.validate().unwrap();
    assert!(upper.cookie("synthetic", 43_200).contains("; Secure"));
}

#[tokio::test]
async fn only_exact_graph_posts_bypass_access_and_legacy_settings_fail_closed() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let exact = format!("/webhooks/{}/graph/notifications", entries[0].id);
    for (method, path) in [
        ("GET", exact.clone()),
        ("POST", format!("{exact}/extra")),
        ("POST", format!("/webhooks/{}/api/control", entries[0].id)),
        ("POST", "/webhooks/not-a-profile/graph/notifications".into()),
        ("POST", "/graph/notifications".into()),
    ] {
        assert_eq!(
            send(&portal, method, &path, None, None, None, json!({}))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let (_dir, legacy, _) = fixture();
    assert!(settings(&legacy.root).unwrap().access.is_none());
    assert_eq!(
        send(&legacy, "GET", "/login", None, None, None, Value::Null)
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn the_portal_refuses_a_password_era_host_without_changing_its_profile() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let (_host, _lock) = fake_host(&portal, &entries[0]).await;
    let dir = profile_dir(&portal.root, &entries[0]);
    let path = dir.join("control.json");
    let mut descriptor: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    descriptor
        .as_object_mut()
        .unwrap()
        .remove("web_access_support");
    write_private(&path, &descriptor.to_string()).unwrap();
    let before = fs::read(dir.join("config.toml")).unwrap();
    assert!(portal.worker(&entries[0]).await.is_err());
    assert_eq!(fs::read(dir.join("config.toml")).unwrap(), before);
}

#[tokio::test]
async fn older_hosts_never_receive_a_machine_connection_or_its_credentials() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let (host, _lock) = fake_host(&portal, &entries[0]).await;
    let dir = profile_dir(&portal.root, &entries[0]);
    let path = dir.join("control.json");
    let mut descriptor: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    descriptor
        .as_object_mut()
        .unwrap()
        .remove("machine_pairing_support");
    write_private(&path, &descriptor.to_string()).unwrap();
    assert!(portal.worker(&entries[0]).await.is_err());
    let mut request = control::Request::new("machine_connect");
    request.args = json!({"url":"https://untrusted.example.com","pairing_token":"synthetic"});
    assert!(
        control::client_at(request, &dir)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("pairing")
    );
    assert!(host.received_requests().await.unwrap().is_empty());
}
