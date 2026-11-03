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

fn host(root: &Path) -> Arc<Host> {
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
    assert!(authority::check_shared_operation(&control::Request::new("cloud_connect")).is_err());
    assert!(authority::check_shared_operation(&control::Request::new("cloud_status")).is_ok());
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
