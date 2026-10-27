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
            authorized: false,
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
async fn legacy_non_owner_stays_closed_without_explicit_local_admission() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    store::edit_accounts(&portal.root, |entries| {
        entries[1].authorized = false;
        Ok(())
    })
    .unwrap();
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
    let reply = store::authorize_machine(&portal.root, &machine, &file, None).unwrap();
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
    assert!(store::authorize_machine(&portal.root, &machine, &file, None).is_err());
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
    store::authorize_machine(&portal.root, &machine, &file, None).unwrap();
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
        store::authorize_machine(&portal.root, &machine, &file, None).unwrap();
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
    let (dir, portal, mut entries) = fixture();
    for entry in &mut entries {
        entry.authorized = true;
    }
    write_private(
        &portal.root.join("accounts.json"),
        &serde_json::to_string(&entries).unwrap(),
    )
    .unwrap();
    write_private(
        &portal.root.join("settings.json"),
        &serde_json::to_string(&Settings {
            owner_github_id: Some(1001),
            public_url: "https://localhost:38656".into(),
            access: Some(access::tests::config()),
            ..Settings::default()
        })
        .unwrap(),
    )
    .unwrap();
    let mut portal = Arc::new(Portal::new(portal.root.clone(), portal.binary.clone()).unwrap());
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
    let mut config = read_config(&dir.join("config.toml")).unwrap();
    config.server.public_url = portal.settings.public_url.clone();
    write_private(&dir.join("config.toml"), &toml::to_string(&config).unwrap()).unwrap();
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
        "wiki_support":true,"activity_registration_support":true,"web_support":true,"web_access_support":true,"implementation_support":true,"machine_pairing_support":true,
        "machine_login_support":true,"shared_accounts_support":true,"account_authority_support":true,"account_binding":{"id":account.id,"version":account.version},
        "browser_token":"synthetic-browser-token","machine_token":"synthetic-machine-token"})
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
                Some("https://localhost:38656"),
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
    for (entry, host) in entries.iter().zip([&_alice, &_bob]) {
        let (cookie, csrf) = session_cookie(&portal, entry).await;
        for (token, origin) in [
            (None, Some("https://localhost:38656")),
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
                Some("https://localhost:38656"),
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
                Some("https://localhost:38656"),
                forged,
            )
            .await,
        )
        .await;
        assert_eq!(data["ok"], false);
        assert_eq!(data["code"], "invalid_input");
        let delivered = host.received_requests().await.unwrap();
        assert_eq!(delivered.len(), 1);
        assert_eq!(
            delivered[0].headers["authorization"],
            "Bearer synthetic-browser-token"
        );
        if entry.id == entries[0].id {
            assert!(_bob.received_requests().await.unwrap().is_empty());
        }
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
            Some("https://localhost:38656"),
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
        Some("https://localhost:38656"),
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
        (super::super::implementation::handles(tool.name)
            && !tool.name.starts_with("machine_login"))
            || matches!(tool.name, "azure_cli_status" | "github_cli_status")
            || tool.name.ends_with("azure_cli_login")
    }) {
        assert!(
            portal
                .check_request(&entries[0], &control::Request::new(tool.name))
                .is_ok(),
            "{}",
            tool.name
        );
    }
    // The portal profile is the coordinator: a browser approves other computers, never joins.
    for method in [
        "implementation_launch",
        "implementation_local",
        "machine_register",
        "machine_protocol",
        "machine_login",
        "machine_login_status",
        "machine_login_finish",
        "machine_login_cancel",
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
async fn account_authority_is_mandatory_and_scoped_forwarding_cannot_select_another_account() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let (host, _lock) = fake_host(&portal, &entries[0]).await;
    let dir = profile_dir(&portal.root, &entries[0]);
    assert!(
        control::client_scoped(
            control::Request::new("snapshot"),
            &dir,
            &entries[1].id,
            &entries[1].version,
            authority::Transport::Browser,
        )
        .await
        .is_err()
    );
    let path = dir.join("control.json");
    let mut descriptor: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    descriptor
        .as_object_mut()
        .unwrap()
        .remove("account_authority_support");
    write_private(&path, &descriptor.to_string()).unwrap();
    let before = fs::read(dir.join("config.toml")).unwrap();
    assert!(portal.worker(&entries[0]).await.is_err());
    assert!(
        control::client_at(control::Request::new("snapshot"), &dir)
            .await
            .is_err()
    );
    assert_eq!(fs::read(dir.join("config.toml")).unwrap(), before);
    assert!(host.received_requests().await.unwrap().is_empty());
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

#[tokio::test]
async fn admitted_accounts_sign_in_privately_and_rebinding_revokes_only_their_authority() {
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let (_owner, _owner_lock) = fake_host(&portal, &entries[0]).await;
    let (_other, _other_lock) = fake_host(&portal, &entries[1]).await;
    let (owner, _) = session_cookie(&portal, &entries[0]).await;
    let mut other = TestSession {
        cookie: String::new(),
        assertion: assertion(&entries[1]),
    };
    let login = send(
        &portal,
        "GET",
        "/login",
        Some(&other),
        None,
        None,
        Value::Null,
    )
    .await;
    assert_eq!(login.status(), StatusCode::OK);
    other.cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into();
    let session = json_body(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&other),
            None,
            None,
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(session["username"], "bob");
    assert_eq!(
        session["profile"],
        profile_dir(&portal.root, &entries[1]).to_str().unwrap()
    );
    assert!(!session.to_string().contains(&entries[0].id));
    // A cookie from one account is unusable with the other identity's verified assertion.
    let crossed = TestSession {
        cookie: owner.cookie.clone(),
        assertion: other.assertion.clone(),
    };
    assert_eq!(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&crossed),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let machine = uuid::Uuid::new_v4().to_string();
    let token = security::random_secret();
    write_private(&profile_dir(&portal.root, &entries[1]).join("implementation/registry.json"),
        &json!({"machines":[{"id":machine,"account_version":entries[1].version,"secret_hash":format!("{:x}",Sha256::digest(token.as_bytes()))}]}).to_string()).unwrap();
    store::edit_accounts(&portal.root, |entries| {
        entries[1].version = uuid::Uuid::new_v4().to_string();
        entries[1].access_binding.as_mut().unwrap().provider_user_id = "1003".into();
        Ok(())
    })
    .unwrap();
    assert_eq!(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&other),
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
            "/api/session",
            Some(&owner),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::OK
    );
    let machine_response = router(portal.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/machines")
                .header(header::HOST, "localhost:38656")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"id":machine,"action":"poll"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(machine_response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        send(
            &portal,
            "POST",
            &format!("/webhooks/{}/graph/notifications", entries[1].id),
            None,
            None,
            None,
            json!({})
        )
        .await
        .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn additional_accounts_require_new_hosts_before_any_request_is_sent() {
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let (host, _lock) = fake_host(&portal, &entries[1]).await;
    let dir = profile_dir(&portal.root, &entries[1]);
    let file = dir.join("control.json");
    let mut descriptor: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    descriptor
        .as_object_mut()
        .unwrap()
        .remove("shared_accounts_support");
    write_private(&file, &descriptor.to_string()).unwrap();
    assert!(portal.worker(&entries[1]).await.is_err());
    assert!(
        control::client_scoped(
            control::Request::new("snapshot"),
            &dir,
            &entries[1].id,
            &entries[1].version,
            authority::Transport::Browser
        )
        .await
        .is_err()
    );
    assert!(host.received_requests().await.unwrap().is_empty());
}

#[test]
fn pairing_for_an_additional_account_is_explicit_and_version_bound() {
    let (dir, portal, mut entries) = fixture();
    entries[1].authorized = true;
    write_private(
        &portal.root.join("accounts.json"),
        &serde_json::to_string(&entries).unwrap(),
    )
    .unwrap();
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
    let output = dir.path().join("second-account-pairing.json");
    store::authorize_machine(&portal.root, &machine, &output, Some("bob")).unwrap();
    let payload: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    let account = store::consume_pairing(
        &portal.root,
        &config,
        payload["pairing_token"].as_str().unwrap(),
        &machine,
    )
    .unwrap()
    .unwrap();
    assert_eq!(account.id, entries[1].id);
    let output = dir.path().join("revoked-pairing.json");
    store::authorize_machine(&portal.root, &machine, &output, Some("bob")).unwrap();
    let payload: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    store::edit_accounts(&portal.root, |entries| {
        entries[1].version = uuid::Uuid::new_v4().to_string();
        Ok(())
    })
    .unwrap();
    assert!(
        store::consume_pairing(
            &portal.root,
            &config,
            payload["pairing_token"].as_str().unwrap(),
            &machine
        )
        .unwrap()
        .is_none()
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn reconciliation_stops_only_the_changed_app_managed_account_host() {
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let (replacement, _replacement_lock) = fake_host(&portal, &entries[1]).await;
    let mut pids = Vec::new();
    for account in &entries {
        let child = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        pids.push(child.id().unwrap());
        portal.workers.lock().await.insert(
            account.id.clone(),
            Worker {
                child,
                profile: profile_dir(&portal.root, account),
                version: account.version.clone(),
            },
        );
    }
    let mut unrelated = tokio::process::Command::new("/bin/sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    store::edit_accounts(&portal.root, |entries| {
        entries[1].version = uuid::Uuid::new_v4().to_string();
        Ok(())
    })
    .unwrap();
    portal.reconcile().await;
    assert!(replacement.received_requests().await.unwrap().is_empty());
    let mut workers = portal.workers.lock().await;
    assert!(
        workers
            .get_mut(&entries[0].id)
            .unwrap()
            .child
            .try_wait()
            .unwrap()
            .is_none()
    );
    assert!(!workers.contains_key(&entries[1].id));
    assert!(!Path::new(&format!("/proc/{}", pids[1])).exists());
    assert!(unrelated.try_wait().unwrap().is_none());
    let owner = workers.remove(&entries[0].id).unwrap();
    drop(workers);
    owner.shutdown().await;
    unrelated.kill().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn an_exited_managed_child_cannot_quit_a_replacement_host_for_the_same_profile() {
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let (replacement, _lock) = fake_host(&portal, &entries[0]).await;
    let mut child = tokio::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    child.wait().await.unwrap();
    Worker {
        child,
        profile: profile_dir(&portal.root, &entries[0]),
        version: entries[0].version.clone(),
    }
    .shutdown()
    .await;
    assert!(replacement.received_requests().await.unwrap().is_empty());
}

// Browser-assisted machine sign-in: synthetic accounts, installations and verifiers only.
fn login_verifier() -> (String, String) {
    let verifier = security::random_secret();
    let challenge = format!("{:x}", Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}
async fn machine_post(portal: &Arc<Portal>, body: Value, bearer: Option<&str>) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/machines")
        .header(header::HOST, "localhost:38656")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    router(portal.clone())
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}
async fn start_login(
    portal: &Arc<Portal>,
    machine: &str,
    challenge: &str,
    expected: Value,
) -> String {
    let response = machine_post(
        portal,
        json!({"action":"login_start","id":machine,"name":"Synthetic laptop","os":"windows",
            "installation":"a".repeat(64),"challenge":challenge,"expected_account":expected}),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let reply = json_body(response).await;
    assert_eq!(reply["ok"], true, "{reply}");
    reply["data"]["transaction"].as_str().unwrap().to_owned()
}
async fn login_poll(
    portal: &Arc<Portal>,
    action: &str,
    machine: &str,
    transaction: &str,
    verifier: &str,
) -> Response {
    machine_post(
        portal,
        json!({"action":action,"id":machine,"transaction":transaction,"verifier":verifier}),
        None,
    )
    .await
}
async fn registration_mock(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/control"))
        .and(wiremock::matchers::body_partial_json(
            json!({"method":"machine_register"}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(control::Reply::success(
                json!({"token":"synthetic-machine-credential-0000000000000"}),
            )),
        )
        .with_priority(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn machine_login_needs_browser_trust_and_the_initiating_installations_verifier() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let (host, _lock) = fake_host(&portal, &entries[0]).await;
    registration_mock(&host).await;
    let machine = uuid::Uuid::new_v4().to_string();
    let (verifier, challenge) = login_verifier();
    let start = json!({"action":"login_start","id":machine,"name":"Synthetic laptop","os":"windows",
        "installation":"a".repeat(64),"challenge":challenge});
    // Sign-in never accepts bearer credentials, forged authority or malformed installations.
    for (body, bearer, status) in [
        (
            start.clone(),
            Some("ghp_synthetic_github_token"),
            StatusCode::FORBIDDEN,
        ),
        (
            json!({"action":"login_start","id":machine,"name":"Laptop","os":"windows","installation":"a".repeat(64),"challenge":challenge,"account":"bob"}),
            None,
            StatusCode::FORBIDDEN,
        ),
        (
            json!({"action":"login_start","id":"not-a-machine","name":"Laptop","os":"windows","installation":"a".repeat(64),"challenge":challenge}),
            None,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(machine_post(&portal, body, bearer).await.status(), status);
    }
    let transaction = start_login(&portal, &machine, &challenge, Value::Null).await;
    // Only the verifier and machine that started it can read or finish it.
    let (other_verifier, _) = login_verifier();
    for (id, secret) in [
        (machine.as_str(), other_verifier.as_str()),
        ("11111111-1111-4111-8111-111111111111", verifier.as_str()),
    ] {
        assert_eq!(
            login_poll(&portal, "login_finish", id, &transaction, secret)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let pending =
        json_body(login_poll(&portal, "login_finish", &machine, &transaction, &verifier).await)
            .await;
    assert_eq!(pending["data"]["state"], "pending");

    // The approval link survives Access and local-session redirects.
    let (alice, csrf) = session_cookie(&portal, &entries[0]).await;
    let location = |response: Response| {
        response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_owned()
    };
    let enroll = format!("?enroll={transaction}");
    assert_eq!(
        location(
            send(
                &portal,
                "GET",
                &format!("/login{enroll}"),
                Some(&alice),
                None,
                None,
                Value::Null
            )
            .await
        ),
        format!("/{enroll}")
    );
    let unsigned = TestSession {
        cookie: String::new(),
        assertion: alice.assertion.clone(),
    };
    assert_eq!(
        location(
            send(
                &portal,
                "GET",
                &format!("/{enroll}"),
                Some(&unsigned),
                None,
                None,
                Value::Null
            )
            .await
        ),
        format!("/login{enroll}")
    );
    assert_eq!(
        location(
            send(
                &portal,
                "GET",
                "/login?enroll=../../evil",
                Some(&alice),
                None,
                None,
                Value::Null
            )
            .await
        ),
        "/"
    );

    // Browser decisions need the session's CSRF token and same-origin request.
    let origin = portal.settings.origin();
    let inspect = json!({"action":"inspect","transaction":transaction});
    assert_eq!(
        send(
            &portal,
            "POST",
            "/api/enrollment",
            Some(&alice),
            None,
            Some(&origin),
            inspect.clone()
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let shown = json_body(
        send(
            &portal,
            "POST",
            "/api/enrollment",
            Some(&alice),
            Some(&csrf),
            Some(&origin),
            inspect,
        )
        .await,
    )
    .await;
    assert_eq!(shown["ok"], true, "{shown}");
    assert_eq!(shown["data"]["machine"]["name"], "Synthetic laptop");
    assert_eq!(shown["data"]["machine"]["os"], "windows");
    assert_eq!(shown["data"]["username"], "alice");
    assert!(!shown.to_string().contains(&verifier));
    // Opening and inspecting in a browser registers nothing.
    assert!(host.received_requests().await.unwrap().is_empty());
    let trusted = json_body(
        send(
            &portal,
            "POST",
            "/api/enrollment",
            Some(&alice),
            Some(&csrf),
            Some(&origin),
            json!({"action":"trust","transaction":transaction}),
        )
        .await,
    )
    .await;
    assert_eq!(trusted["ok"], true, "{trusted}");
    // One decision only: another account cannot take over an approved request.
    let (bob, bob_csrf) = session_cookie(&portal, &entries[1]).await;
    let taken = json_body(
        send(
            &portal,
            "POST",
            "/api/enrollment",
            Some(&bob),
            Some(&bob_csrf),
            Some(&origin),
            json!({"action":"trust","transaction":transaction}),
        )
        .await,
    )
    .await;
    assert_eq!(taken["ok"], false);
    let approved =
        json_body(login_poll(&portal, "login_status", &machine, &transaction, &verifier).await)
            .await;
    assert_eq!(approved["data"]["state"], "approved");
    assert!(host.received_requests().await.unwrap().is_empty());

    let finished =
        json_body(login_poll(&portal, "login_finish", &machine, &transaction, &verifier).await)
            .await;
    assert_eq!(finished["ok"], true, "{finished}");
    assert_eq!(finished["data"]["account"], 1001);
    assert_eq!(finished["data"]["username"], "alice");
    let requests = host.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(request["method"], "machine_register");
    assert_eq!(request["args"]["id"], machine.as_str());
    assert_eq!(request["args"]["enrollment"], "login");
    assert_eq!(request["args"]["installation"], "a".repeat(64));
    let forwarded = request.to_string();
    assert!(!forwarded.contains(&verifier) && !forwarded.contains(&transaction));
    // Single use: a replayed finish issues nothing and does not reach the host again.
    let replay =
        json_body(login_poll(&portal, "login_finish", &machine, &transaction, &verifier).await)
            .await;
    assert_eq!(replay["data"]["state"], "expired");
    assert_eq!(host.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn machine_login_denied_mismatched_revoked_expired_and_cancelled_requests_issue_nothing() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let (host, _lock) = fake_host(&portal, &entries[1]).await;
    registration_mock(&host).await;
    let machine = uuid::Uuid::new_v4().to_string();
    let finish = |transaction: String, verifier: String| {
        let portal = portal.clone();
        let machine = machine.clone();
        async move {
            json_body(login_poll(&portal, "login_finish", &machine, &transaction, &verifier).await)
                .await["data"]["state"]
                .clone()
        }
    };
    // A computer already in Alice's account cannot be trusted by Bob.
    let (verifier, challenge) = login_verifier();
    let owned = start_login(&portal, &machine, &challenge, json!(1001)).await;
    assert!(enrollment::inspect(&portal.root, &entries[1], &owned).is_err());
    assert!(enrollment::decide(&portal.root, &entries[1], &owned, true).is_err());
    enrollment::decide(&portal.root, &entries[0], &owned, false).unwrap();
    assert_eq!(finish(owned, verifier).await, "denied");

    // Approval is bound to the account version: revocation before finishing issues nothing.
    let (verifier, challenge) = login_verifier();
    let revoked = start_login(&portal, &machine, &challenge, Value::Null).await;
    enrollment::decide(&portal.root, &entries[1], &revoked, true).unwrap();
    store::edit_accounts(&portal.root, |accounts| {
        accounts[1].version = uuid::Uuid::new_v4().to_string();
        Ok(())
    })
    .unwrap();
    assert_eq!(
        finish(revoked.clone(), verifier.clone()).await,
        "unavailable"
    );
    assert_eq!(finish(revoked, verifier).await, "expired");

    // Expired requests disappear.
    let (verifier, challenge) = login_verifier();
    let expired = start_login(&portal, &machine, &challenge, Value::Null).await;
    let path = portal.root.join("machine-logins.json");
    let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for entry in stored.as_object_mut().unwrap().values_mut() {
        entry["expires_at"] = json!(1);
    }
    write_private(&path, &stored.to_string()).unwrap();
    assert!(enrollment::inspect(&portal.root, &entries[0], &expired).is_err());
    assert_eq!(finish(expired, verifier).await, "expired");

    // Only the initiating installation can cancel; a cancelled request cannot be trusted.
    let (verifier, challenge) = login_verifier();
    let cancelled = start_login(&portal, &machine, &challenge, Value::Null).await;
    let (other, _) = login_verifier();
    assert_eq!(
        login_poll(&portal, "login_cancel", &machine, &cancelled, &other)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        json_body(login_poll(&portal, "login_cancel", &machine, &cancelled, &verifier).await).await
            ["ok"],
        true
    );
    assert!(enrollment::decide(&portal.root, &entries[0], &cancelled, true).is_err());
    assert_eq!(finish(cancelled, verifier).await, "expired");
    assert!(host.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn machine_login_is_rate_limited_and_never_reaches_an_older_host() {
    let (_dir, portal, entries, _server) = configured_fixture().await;
    let machine = uuid::Uuid::new_v4().to_string();
    let (_, challenge) = login_verifier();
    for _ in 0..5 {
        start_login(&portal, &machine, &challenge, Value::Null).await;
    }
    // Restarting from the same installation replaces its own pending request.
    let stored: Value =
        serde_json::from_slice(&fs::read(portal.root.join("machine-logins.json")).unwrap())
            .unwrap();
    assert_eq!(stored.as_object().unwrap().len(), 1);
    let limited = machine_post(
        &portal,
        json!({"action":"login_start","id":machine,"name":"Laptop","os":"linux","installation":"a".repeat(64),"challenge":challenge}),
        None,
    )
    .await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);

    // A host without the installation-proof check never receives a sign-in registration.
    let (host, _lock) = fake_host(&portal, &entries[0]).await;
    let file = profile_dir(&portal.root, &entries[0]).join("control.json");
    let mut descriptor: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    descriptor
        .as_object_mut()
        .unwrap()
        .remove("machine_login_support");
    write_private(&file, &descriptor.to_string()).unwrap();
    let other = uuid::Uuid::new_v4().to_string();
    let (verifier, challenge) = login_verifier();
    let transaction = start_login(&portal, &other, &challenge, Value::Null).await;
    enrollment::decide(&portal.root, &entries[0], &transaction, true).unwrap();
    assert_eq!(
        login_poll(&portal, "login_finish", &other, &transaction, &verifier)
            .await
            .status(),
        StatusCode::BAD_GATEWAY
    );
    assert!(host.received_requests().await.unwrap().is_empty());
}

struct NativeShell;
impl Shell for NativeShell {
    fn headless(&self) -> bool {
        true
    }
    fn show(&self) -> Result<()> {
        Ok(())
    }
    fn hide(&self) -> Result<()> {
        Ok(())
    }
    fn open_url(&self, _: &str) -> Result<()> {
        panic!("A headless installation returns the approval URL instead of opening it.")
    }
    fn exit(&self) {}
}

#[tokio::test]
async fn machine_login_native_installation_finishes_only_after_trust_and_keeps_prior_state() {
    let (dir, portal, entries, _server) = configured_fixture().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("https://{address}");
    write_private(
        &portal.root.join("settings.json"),
        &serde_json::to_string(&Settings {
            public_url: origin.clone(),
            ..portal.settings.clone()
        })
        .unwrap(),
    )
    .unwrap();
    let served = Arc::new(Portal::new(portal.root.clone(), portal.binary.clone()).unwrap());
    let (account_host, _lock) = fake_host(&served, &entries[0]).await;
    registration_mock(&account_host).await;
    tokio::spawn(axum::serve(listener, router(served.clone())).into_future());
    // The coordinator protocol is exercised over loopback HTTP; production requires HTTPS.
    let endpoint = url::Url::parse(&format!("http://{address}/api/machines")).unwrap();
    let native = Arc::new(Host {
        state: init_state(&dir.path().join("native"), &dir.path().join("native-data")).unwrap(),
        shell: Box::new(NativeShell),
    });
    use super::implementation::login::{begin_at, cancel, finish_with, status};

    let begun = begin_at(&native, &origin, endpoint.clone(), None, true)
        .await
        .unwrap();
    let mut keys: Vec<_> = begun.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "approval_url",
            "code",
            "coordinator",
            "expires_at",
            "machine",
            "opened",
            "state"
        ]
    );
    assert_eq!(begun["opened"], false);
    assert_eq!(begun["machine"]["os"], std::env::consts::OS);
    let approval = begun["approval_url"].as_str().unwrap();
    let transaction = approval
        .strip_prefix(&format!("{origin}/login?enroll="))
        .unwrap()
        .to_owned();
    // Reusing the pending request keeps the same approval.
    let again = begin_at(&native, &origin, endpoint.clone(), None, false)
        .await
        .unwrap();
    assert_eq!(again["approval_url"], approval);

    let mut stored = None;
    let pending = finish_with(&native, |token| {
        stored = Some(token.to_owned());
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(pending["pending"], true);
    assert!(stored.is_none());
    enrollment::decide(&served.root, &entries[0], &transaction, true).unwrap();
    assert_eq!(status(&native).await.unwrap()["state"], "approved");
    let connected = finish_with(&native, |token| {
        stored = Some(token.to_owned());
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(connected["connected"], true);
    assert_eq!(connected["username"], "alice");
    assert_eq!(
        stored.as_deref(),
        Some("synthetic-machine-credential-0000000000000")
    );
    let connection_path = dir.path().join("native/implementation/connection.json");
    let connection: Value = serde_json::from_slice(&fs::read(&connection_path).unwrap()).unwrap();
    assert_eq!(connection["url"], origin.as_str());
    assert_eq!(connection["account"], 1001);
    assert!(
        !fs::read_to_string(&connection_path)
            .unwrap()
            .contains("synthetic-machine-credential")
    );
    let none = status(&native).await.unwrap();
    assert_eq!(none["state"], "none");
    assert_eq!(none["connected"], true);
    assert!(finish_with(&native, |_| Ok(())).await.is_err());

    // A denied re-sign-in trusts nothing and leaves the previous connection in place.
    let before = fs::read(&connection_path).unwrap();
    let denied = begin_at(&native, &origin, endpoint.clone(), Some(1001), false)
        .await
        .unwrap();
    let denied_transaction = denied["approval_url"]
        .as_str()
        .unwrap()
        .rsplit('=')
        .next()
        .unwrap()
        .to_owned();
    enrollment::decide(&served.root, &entries[0], &denied_transaction, false).unwrap();
    let mut called = false;
    let error = finish_with(&native, |_| {
        called = true;
        Ok(())
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("denied"), "{error}");
    assert!(!called);
    assert_eq!(fs::read(&connection_path).unwrap(), before);

    // Another account cannot approve a computer bound to a different account; cancel ends it.
    let foreign = begin_at(&native, &origin, endpoint, Some(1002), false)
        .await
        .unwrap();
    let foreign_transaction = foreign["approval_url"]
        .as_str()
        .unwrap()
        .rsplit('=')
        .next()
        .unwrap()
        .to_owned();
    assert!(enrollment::decide(&served.root, &entries[0], &foreign_transaction, true).is_err());
    cancel(&native).await.unwrap();
    assert!(enrollment::decide(&served.root, &entries[1], &foreign_transaction, true).is_err());
    assert!(finish_with(&native, |_| Ok(())).await.is_err());
    assert_eq!(fs::read(&connection_path).unwrap(), before);
    assert_eq!(account_host.received_requests().await.unwrap().len(), 1);
}
