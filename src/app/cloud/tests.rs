//! The account store link: how the Worker's answers are read, what the profile keeps and who may
//! connect. The Worker's own contract (enrollment, cross-account refusals) is tested in `cloud/`.
use super::*;
use crate::config::Config;
use authority::Transport::{Local, Machine};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct TestShell;
impl Shell for TestShell {
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
        Ok(())
    }
    fn exit(&self) {}
}

pub(super) fn host(root: &Path) -> Arc<Host> {
    Arc::new(Host {
        state: init_state(&root.join("profile"), &root.join("data")).unwrap(),
        shell: Box::new(TestShell),
    })
}

async fn call(
    host: &Arc<Host>,
    transport: authority::Transport,
    method: &str,
    args: Value,
) -> Reply {
    let mut request = control::Request::new(method);
    request.args = args;
    authority::within(
        authority::Invocation {
            scope: host.state.authority.clone(),
            transport,
        },
        control::dispatch(host, request),
    )
    .await
}

fn credentials() -> Credentials {
    Credentials {
        client_id: "synthetic.access".into(),
        client_secret: "synthetic-client-secret".into(),
        token: Some(format!("ptac_{}", "A".repeat(43))),
    }
}

async fn answer(template: ResponseTemplate) -> std::result::Result<Value, Failure> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/whoami"))
        .respond_with(template)
        .mount(&server)
        .await;
    exchange(
        &server.uri(),
        reqwest::Method::GET,
        "/v1/whoami",
        Some(&credentials()),
        None,
    )
    .await
}

#[tokio::test]
async fn worker_answers_map_to_standing_without_following_redirects() {
    let ok = answer(ResponseTemplate::new(200).set_body_json(json!({"installation":{"id":"x"}})));
    assert_eq!(ok.await.unwrap()["installation"]["id"], "x");
    // Access refuses with its own page; the Worker with JSON. Both mean the credentials.
    let edge = ResponseTemplate::new(401).set_body_string("<html>Forbidden</html>");
    assert_eq!(answer(edge).await, Err(Failure::Revoked));
    let worker = ResponseTemplate::new(401).set_body_json(json!({"error":{"code":"unauthorized"}}));
    assert_eq!(answer(worker).await, Err(Failure::Revoked));
    let quota =
        ResponseTemplate::new(429).set_body_json(json!({"error":{"code":"quota_exhausted"}}));
    assert_eq!(answer(quota).await, Err(Failure::Quota));
    let unconfigured =
        ResponseTemplate::new(503).set_body_json(json!({"error":{"code":"not_configured"}}));
    assert_eq!(answer(unconfigured).await, Err(Failure::Unconfigured));
    let refused = ResponseTemplate::new(409).set_body_json(json!({"error":{"code":"conflict"}}));
    assert_eq!(
        answer(refused).await,
        Err(Failure::Refused("conflict".into()))
    );
    assert_eq!(
        answer(ResponseTemplate::new(500)).await,
        Err(Failure::Offline)
    );
    // A login redirect is never followed with the credentials.
    let redirect =
        ResponseTemplate::new(302).insert_header("location", "https://login.example.com/");
    assert_eq!(answer(redirect).await, Err(Failure::Offline));
    // A success that is not JSON is not an answer.
    assert_eq!(
        answer(ResponseTemplate::new(200).set_body_string("<html></html>")).await,
        Err(Failure::Offline)
    );
}

#[tokio::test]
async fn both_credentials_travel_as_headers_never_in_the_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/store"))
        .and(header("CF-Access-Client-Id", "synthetic.access"))
        .and(header("CF-Access-Client-Secret", "synthetic-client-secret"))
        .and(header(
            "authorization",
            format!("Bearer ptac_{}", "A".repeat(43)).as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"schema":1})))
        .expect(1)
        .mount(&server)
        .await;
    let store = exchange(
        &server.uri(),
        reqwest::Method::GET,
        "/v1/store",
        Some(&credentials()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(store["schema"], 1);
}

#[tokio::test]
async fn health_requires_a_configured_worker_that_speaks_this_protocol() {
    for (body, expected) in [
        (
            json!({"configured":false,"protocols":[1]}),
            Some(Failure::Unconfigured),
        ),
        (
            json!({"configured":true,"protocols":[2]}),
            Some(Failure::Refused("incompatible_protocol".into())),
        ),
        (json!({"configured":true,"protocols":[1,2]}), None),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        assert_eq!(health(&server.uri()).await.err(), expected);
    }
}

#[test]
fn the_cloud_section_is_omitted_until_connected_and_checked() {
    let mut config = Config::desktop_template().unwrap();
    assert!(!toml::to_string(&config).unwrap().contains("[cloud]"));
    config.cloud = Cloud {
        endpoint: "https://store.example.com".into(),
        installation: uuid::Uuid::new_v4().to_string(),
        mode: CloudMode::Off,
        token_expires_at: 0,
    };
    config.cloud.validate().unwrap();
    let text = toml::to_string(&config).unwrap();
    assert!(text.contains("[cloud]"));
    let back: Config = toml::from_str(&text).unwrap();
    assert_eq!(back.cloud, config.cloud);
    for endpoint in [
        "http://store.example.com",
        "https://store.example.com/v1",
        "https://user@store.example.com",
        "https://store.example.com?x=1",
        "https://localhost",
        "https://store.example.com:8443",
    ] {
        assert!(Cloud::check_endpoint(endpoint).is_err(), "{endpoint}");
    }
    let orphan = Cloud {
        installation: uuid::Uuid::new_v4().to_string(),
        ..Cloud::default()
    };
    assert!(orphan.validate().is_err());
    let premature = Cloud {
        endpoint: "https://store.example.com".into(),
        mode: CloudMode::Mirror,
        ..Cloud::default()
    };
    assert!(premature.validate().is_err());
}

#[tokio::test]
async fn status_reports_a_disconnected_installation_without_network() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let reply = call(&host, Local, "cloud_status", json!({})).await;
    assert!(reply.ok, "{:?}", reply.message);
    assert_eq!(reply.data["standing"], "disconnected");
    assert_eq!(reply.data["mode"], "off");
    assert_eq!(reply.data["enrolled"], false);
    let invalid = call(&host, Local, "cloud_status", json!({"endpoint":"x"})).await;
    assert_eq!(invalid.code, "invalid_input");
}

#[tokio::test]
async fn connect_checks_its_arguments_before_any_exchange() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    for args in [
        json!({"endpoint":"http://store.example.com","client_id":"a.access","client_secret":"synthetic-client-secret"}),
        json!({"endpoint":"https://store.example.com","client_id":"a.access"}),
        json!({"endpoint":"https://store.example.com","client_id":"bad id","client_secret":"synthetic-client-secret"}),
        json!({"endpoint":"https://store.example.com","client_id":"a.access","client_secret":"short"}),
        json!({"endpoint":"https://store.example.com","label":"x".repeat(81),"client_id":"a.access","client_secret":"synthetic-client-secret"}),
        json!({"endpoint":"https://store.example.com","account":"someone-else"}),
    ] {
        let reply = call(&host, Local, "cloud_connect", args.clone()).await;
        // An account argument is refused by the host's authority check before the operation.
        assert!(
            ["invalid_input", "authority_denied"].contains(&reply.code.as_str()),
            "{args}: {}",
            reply.code
        );
        assert!(
            !serde_json::to_string(&reply)
                .unwrap()
                .contains("synthetic-client-secret")
        );
    }
    assert!(host.state.settings.config().unwrap().cloud.is_default());
}

#[tokio::test]
async fn machines_and_shared_accounts_cannot_connect() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let args = json!({"endpoint":"https://store.example.com"});
    let reply = call(&host, Machine, "cloud_connect", args).await;
    assert!(!reply.ok);
    assert_eq!(reply.code, "authority_denied");
    // Portal accounts reach the operations; the host checks the operator's grant (`web::tests`).
    assert!(authority::check_shared_operation(&control::Request::new("cloud_connect")).is_ok());
}

#[tokio::test]
async fn installations_need_the_owner_and_a_link_and_never_revoke_this_one() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    for (method, args) in [
        ("cloud_installations", json!({})),
        (
            "cloud_revoke",
            json!({"installation": uuid::Uuid::new_v4().to_string()}),
        ),
    ] {
        let machine = call(&host, Machine, method, args.clone()).await;
        assert_eq!(machine.code, "authority_denied", "{method}");
        // Not connected: nothing to ask.
        let local = call(&host, Local, method, args).await;
        assert_eq!(local.code, "not_ready", "{method}");
    }
    for args in [
        json!({"installation": "not-an-id"}),
        json!({}),
        json!({"installation": uuid::Uuid::new_v4().to_string(), "account": "x"}),
    ] {
        let reply = call(&host, Local, "cloud_revoke", args.clone()).await;
        assert!(
            ["invalid_input", "authority_denied"].contains(&reply.code.as_str()),
            "{args}: {}",
            reply.code
        );
    }
    assert!(
        !call(&host, Local, "cloud_installations", json!({"all": true}))
            .await
            .ok
    );
}

#[tokio::test]
async fn a_handoff_installs_the_key_once_and_never_replaces_another() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let bundle = |content: &crate::mirror::key::Content| {
        serde_json::to_vec(&json!({
            "endpoint": "https://store.example.invalid",
            "client_id": "synthetic.access",
            "client_secret": "synthetic-client-secret",
            "token_expires_at": 0,
            "content_key": content.backup(),
        }))
        .unwrap()
    };
    let kept = std::sync::Mutex::new(Vec::new());
    let receive = |plain: Vec<u8>| {
        let (host, kept) = (&host, &kept);
        async move {
            receive_with(host, &plain, "Synthetic laptop", |name, value| {
                kept.lock()
                    .unwrap()
                    .push((name.to_owned(), value.to_owned()));
                Ok(())
            })
            .await
        }
    };
    let content = crate::mirror::key::Content::generate();
    let installed = receive(bundle(&content)).await;
    assert_eq!(installed["installed"], true, "{installed}");
    assert_eq!(installed["key_written"], true);
    // The store does not answer here: installed, not connected, and nothing uploaded.
    assert_eq!(installed["connected"], false);
    let keys = host.state.settings.profile().join("keys");
    let held = crate::mirror::key::load(&keys).unwrap().unwrap();
    assert_eq!(held.backup(), content.backup());
    assert!(
        !serde_json::to_string(&installed)
            .unwrap()
            .contains("synthetic-client-secret")
    );
    // The same key again changes nothing; another one is refused and the first one stays.
    let again = receive(bundle(&content)).await;
    assert_eq!(again["key_written"], false);
    let other = receive(bundle(&crate::mirror::key::Content::generate())).await;
    assert_eq!(other["installed"], false);
    assert_eq!(
        crate::mirror::key::load(&keys).unwrap().unwrap().backup(),
        content.backup()
    );
    assert_eq!(receive(b"not json".to_vec()).await["installed"], false);
}

#[tokio::test]
async fn settings_editors_keep_the_stored_link() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let mut config = host.state.settings.config().unwrap();
    config.cloud = Cloud {
        endpoint: "https://store.example.com".into(),
        installation: uuid::Uuid::new_v4().to_string(),
        mode: CloudMode::Off,
        token_expires_at: 0,
    };
    host.state.settings.save_config(&config).unwrap();
    let stored = config.cloud.clone();
    let mut edited = config.clone();
    edited.cloud = Cloud::default();
    let map = host.state.settings.map().unwrap();
    // The editor's copy has no link; the saved profile keeps the stored one.
    save_settings(&host, edited, map, String::new())
        .await
        .unwrap();
    assert_eq!(host.state.settings.config().unwrap().cloud, stored);
}

#[test]
fn the_account_store_link_is_a_standing_of_its_own() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let standing = &host.state.standing;
    let cloud = Cloud {
        endpoint: "https://store.example.com".into(),
        installation: uuid::Uuid::new_v4().to_string(),
        ..Cloud::default()
    };
    assert_eq!(
        standing.store_view(&Cloud::default(), false)["state"],
        "disconnected"
    );
    assert_eq!(standing.store_view(&cloud, false)["state"], "not_enrolled");
    assert_eq!(standing.store_view(&cloud, true)["state"], "unknown");
    standing.observe_store(&cloud, None);
    let live = standing.store_view(&cloud, true);
    assert_eq!(
        (live["kind"].as_str(), live["state"].as_str()),
        (Some("account_store"), Some("live"))
    );
    assert!(live["last_live_at"].is_i64());
    // An operation refusal says nothing about the link.
    standing.observe_store(&cloud, Some(&Failure::Refused("invalid_input".into())));
    assert_eq!(standing.store_view(&cloud, true)["state"], "live");
    standing.observe_store(&cloud, Some(&Failure::Quota));
    let offline = standing.store_view(&cloud, true);
    assert_eq!(offline["state"], "offline");
    assert_eq!(offline["last_live_at"], live["last_live_at"]);
    standing.observe_store(&cloud, Some(&Failure::Revoked));
    assert_eq!(standing.store_view(&cloud, true)["state"], "revoked");
    // Another installation's record is never shown for this one.
    let other = Cloud {
        installation: uuid::Uuid::new_v4().to_string(),
        ..cloud.clone()
    };
    assert_eq!(standing.store_view(&other, true)["state"], "unknown");
    assert_ne!(
        standing.store_view(&other, true)["account_key"],
        standing.store_view(&cloud, true)["account_key"]
    );
    assert!(
        !standing
            .store_view(&cloud, true)
            .to_string()
            .contains(&cloud.installation)
    );
    standing.reset_store();
    assert_eq!(standing.store_view(&cloud, true)["state"], "unknown");
    // The coordinator's standing names its kind too.
    assert_eq!(standing.view(None)["kind"], "coordinator");
}

#[test]
fn the_service_token_expiry_is_a_date_and_warns_ahead() {
    assert_eq!(expiry("2027-11-03"), Some(1_825_200_000));
    for bad in ["2027-13-01", "03-11-2027", "1825286400", "1999-01-01", ""] {
        assert_eq!(expiry(bad), None, "{bad}");
    }
    let cloud = Cloud {
        token_expires_at: 1_825_200_000,
        ..Cloud::default()
    };
    let far = token_view(&cloud, 1_825_200_000 - 90 * 86_400).unwrap();
    assert_eq!(
        (far["days_left"].as_i64(), far["renew"].as_bool()),
        (Some(90), Some(false))
    );
    let near = token_view(&cloud, 1_825_200_000 - 10 * 86_400).unwrap();
    assert_eq!(near["renew"], true);
    assert!(token_view(&Cloud::default(), 0).is_none());
    let text = toml::to_string(&Config {
        cloud: Cloud {
            endpoint: "https://store.example.com".into(),
            ..cloud.clone()
        },
        ..Config::desktop_template().unwrap()
    })
    .unwrap();
    assert!(text.contains("token_expires_at = 1825200000"));
    assert!(
        !toml::to_string(&Cloud {
            endpoint: "https://store.example.com".into(),
            ..Cloud::default()
        })
        .unwrap()
        .contains("token_expires_at")
    );
    assert!(
        Cloud {
            token_expires_at: 1_825_200_000_000,
            ..Cloud::default()
        }
        .validate()
        .is_err()
    );
}

#[tokio::test]
async fn disconnect_is_local_safe_to_repeat_and_keeps_the_copy() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    for name in ["cloud_disconnect"] {
        assert_eq!(
            call(&host, Machine, name, json!({})).await.code,
            "authority_denied"
        );
    }
    let mut config = host.state.settings.config().unwrap();
    config.cloud = Cloud {
        endpoint: "https://store.example.com".into(),
        installation: uuid::Uuid::new_v4().to_string(),
        mode: CloudMode::Mirror,
        token_expires_at: 1_825_200_000,
    };
    host.state.settings.save_config(&config).unwrap();
    let key = crate::mirror::key::create(&host.state.settings.profile().join("keys")).unwrap();
    // No stored credential: nothing to tell the Worker, and the link is still removed here.
    let reply = call(&host, Local, "cloud_disconnect", json!({})).await;
    assert!(reply.ok, "{:?}", reply.message);
    assert_eq!(reply.data["changed"], true);
    assert_eq!(reply.data["store_notified"], Value::Null);
    let cloud = host.state.settings.config().unwrap().cloud;
    assert_eq!(cloud.mode, CloudMode::Off);
    assert!(cloud.installation.is_empty());
    assert_eq!(cloud.endpoint, "https://store.example.com");
    assert_eq!(cloud.token_expires_at, 1_825_200_000);
    assert_eq!(
        crate::mirror::key::load(&host.state.settings.profile().join("keys"))
            .unwrap()
            .unwrap()
            .backup(),
        key.backup(),
        "the content key stays"
    );
    let again = call(&host, Local, "cloud_disconnect", json!({})).await;
    assert!(again.ok);
    assert_eq!(again.data["changed"], false);
    let forget = call(
        &host,
        Local,
        "cloud_disconnect",
        json!({"forget_service_token": true}),
    )
    .await;
    assert!(forget.ok);
    assert!(host.state.settings.config().unwrap().cloud.is_default());
    let bad = call(
        &host,
        Local,
        "cloud_disconnect",
        json!({"account": "other"}),
    )
    .await;
    assert!(!bad.ok);
}

#[tokio::test]
async fn setup_status_reports_the_account_store_step() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let reply = call(&host, Local, "setup_readiness", json!({})).await;
    assert!(reply.ok, "{:?}", reply.message);
    let step = reply.data["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "account_store")
        .cloned()
        .unwrap();
    assert_eq!(
        (
            step["state"].as_str(),
            step["scope"].as_str(),
            step["required"].as_bool()
        ),
        (Some("not_applicable"), Some("this_machine"), Some(false))
    );
    assert_eq!(step["facts"]["standing"], "disconnected");
}
