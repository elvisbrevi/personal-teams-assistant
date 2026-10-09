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

const PASSWORD: &str = "synthetic-test-password";

fn fixture() -> (tempfile::TempDir, Arc<Portal>, Vec<Account>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("profile/web");
    security::private_dir(&root).unwrap();
    let hash = hash_password(PASSWORD).unwrap();
    let accounts: Vec<_> = ["alice", "bob"]
        .into_iter()
        .map(|name| Account {
            id: uuid::Uuid::new_v4().to_string(),
            username: name.into(),
            password_hash: hash.clone(),
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
    let portal = Arc::new(Portal::new(root, PathBuf::from("/unused-test-host")).unwrap());
    (dir, portal, accounts)
}
async fn send(
    portal: &Arc<Portal>,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    origin: Option<&str>,
    input: Value,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "localhost:38656");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
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
async fn session_cookie(portal: &Arc<Portal>, account: &Account) -> (String, String) {
    let (token, session) = portal.issue_session(account).await.unwrap();
    (
        format!("{}={token}", portal.settings.cookie_name()),
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
        "wiki_support":true,"activity_registration_support":true,"web_support":true})
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
async fn login_requires_same_origin_and_never_exposes_profile_without_a_session() {
    let (_dir, portal, _) = fixture();
    let root = send(&portal, "GET", "/", None, None, None, Value::Null).await;
    assert_eq!(root.status(), StatusCode::SEE_OTHER);
    assert_eq!(root.headers()[header::LOCATION], "/login");
    assert_eq!(root.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        root.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    for uri in ["/api/session", "/app.js", "/transport.js"] {
        assert_eq!(
            send(&portal, "GET", uri, None, None, None, Value::Null)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let input = json!({"username":"Alice","password":PASSWORD});
    assert_eq!(
        send(
            &portal,
            "POST",
            "/api/login",
            None,
            None,
            Some("https://other.example"),
            input.clone()
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let response = send(
        &portal,
        "POST",
        "/api/login",
        None,
        None,
        Some("http://localhost:38656"),
        input,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let data = json_body(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(cookie.split(';').next().unwrap()),
            None,
            None,
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(data["username"], "alice");
    assert!(data.get("password_hash").is_none());
    for name in ["bob", "unknown"] {
        assert_eq!(
            send(
                &portal,
                "POST",
                "/api/login",
                None,
                None,
                Some("http://localhost:38656"),
                json!({"username":name,"password":"wrong-password"})
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn commands_are_session_scoped_and_require_csrf_even_for_reads() {
    let (_dir, portal, entries) = fixture();
    let (_alice, _alice_lock) = fake_host(&portal, &entries[0]).await;
    let (_bob, _bob_lock) = fake_host(&portal, &entries[1]).await;
    let request = serde_json::to_value(control::Request::new("snapshot")).unwrap();
    for entry in &entries {
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
}

#[tokio::test]
async fn changing_a_password_revokes_all_old_sessions_and_logout_revokes_the_new_one() {
    let (_dir, portal, entries) = fixture();
    let (cookie, csrf) = session_cookie(&portal, &entries[0]).await;
    let (other, _) = session_cookie(&portal, &entries[0]).await;
    let response = send(
        &portal,
        "POST",
        "/api/password",
        Some(&cookie),
        Some(&csrf),
        Some("http://localhost:38656"),
        json!({"current_password":PASSWORD,"new_password":"new-synthetic-password"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let new = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    for old in [&cookie, &other] {
        assert_eq!(
            send(
                &portal,
                "GET",
                "/api/session",
                Some(old),
                None,
                None,
                Value::Null
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let session = json_body(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&new),
            None,
            None,
            Value::Null,
        )
        .await,
    )
    .await;
    let updated = accounts(&portal.root).unwrap();
    assert!(verify_password(
        &updated[0].password_hash,
        "new-synthetic-password"
    ));
    assert!(!verify_password(&updated[0].password_hash, PASSWORD));
    let saved = fs::read_to_string(portal.root.join("accounts.json")).unwrap();
    assert!(!saved.contains(PASSWORD) && !saved.contains("new-synthetic-password"));
    assert_eq!(
        send(
            &portal,
            "POST",
            "/api/logout",
            Some(&new),
            session["csrf_token"].as_str(),
            Some("http://localhost:38656"),
            json!({})
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        send(
            &portal,
            "GET",
            "/api/session",
            Some(&new),
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
async fn disabling_an_account_and_expiration_are_checked_on_every_request() {
    let (_dir, portal, entries) = fixture();
    let (cookie, _) = session_cookie(&portal, &entries[0]).await;
    edit_accounts(&portal.root, |entries| {
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
    let (cookie, _) = session_cookie(&portal, &entries[1]).await;
    portal
        .sessions
        .lock()
        .await
        .values_mut()
        .for_each(|session| session.expires = Instant::now() - Duration::from_secs(1));
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
    let (_dir, portal, entries) = fixture();
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
    edit_accounts(&portal.root, |entries| {
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
            public_url: "http://assistant.elvisbrevi.cl".into(),
            ..Settings::default()
        },
        Settings {
            bind: "0.0.0.0:38656".into(),
            ..Settings::default()
        },
        Settings {
            public_url: "https://assistant.elvisbrevi.cl/path".into(),
            ..Settings::default()
        },
    ] {
        assert!(settings.validate().is_err());
    }
    let config = Settings {
        public_url: "https://assistant.elvisbrevi.cl".into(),
        ..Settings::default()
    };
    config.validate().unwrap();
    let cookie = config.cookie("synthetic", false);
    assert!(
        cookie.starts_with("__Host-pta-session=")
            && cookie.contains("; Secure")
            && !cookie.contains("Domain=")
    );
    let upper = Settings {
        public_url: "HTTPS://assistant.elvisbrevi.cl".into(),
        ..Settings::default()
    };
    upper.validate().unwrap();
    assert!(upper.cookie("synthetic", false).contains("; Secure"));
}
