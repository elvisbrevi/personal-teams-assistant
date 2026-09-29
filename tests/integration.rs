mod support;
use anyhow::Result;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use personal_teams_assistant::{
    adapters::{
        MessageAdapter,
        graph::Graph,
        oauth::OAuth,
        webhook::{self, WebState},
    },
    decision::{DecisionGate, Jev, Stage},
    knowledge::{Access, KnowledgeMap, Resource},
    llm::{DeepSeek, GenerationInput, LlmProvider},
    pipeline::Pipeline,
    security::{Redactor, Vault},
    state::Subscription,
    tools::{ReadOnlyTool, ToolSpec},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, method, path},
};

struct NoTools;
#[async_trait]
impl ReadOnlyTool for NoTools {
    async fn execute(&self, _: &ToolSpec, _: &str, _: &str) -> Result<String> {
        panic!("unexpected tool invocation")
    }
}
struct NoLlm;
#[async_trait]
impl LlmProvider for NoLlm {
    fn name(&self) -> &str {
        "none"
    }
    async fn generate(&self, _: GenerationInput<'_>) -> Result<String> {
        panic!("unexpected generation")
    }
}
struct NoGate;
#[async_trait]
impl DecisionGate for NoGate {
    async fn evaluate(
        &self,
        _: Stage,
        _: serde_json::Value,
        _: BTreeMap<String, String>,
    ) -> Result<personal_teams_assistant::decision::Verdict> {
        panic!("greetings must not consume Jev")
    }
}
fn knowledge(dir: &tempfile::TempDir) -> KnowledgeMap {
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(
        dir.path().join("hours.md"),
        "El soporte atiende de lunes a viernes de 09:00 a 18:00.",
    )
    .unwrap();
    KnowledgeMap {
        repositories: BTreeMap::from([("private".into(), dir.path().into())]),
        resources: vec![Resource {
            id: "hours".into(),
            description: "Horarios de soporte".into(),
            topics: vec!["soporte".into()],
            enabled: true,
            external_processing: true,
            allowed_conversations: vec!["chats/chat1".into()],
            allowed_senders: vec![],
            access: Access::File {
                repository: "private".into(),
                path: "hours.md".into(),
            },
        }],
    }
}
fn redactor() -> Arc<Redactor> {
    Arc::new(Redactor::new(&[], vec![]).unwrap())
}
#[tokio::test]
async fn deterministic_greeting_sends_without_jev_or_llm() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let graph = support::graph(&server, store.clone());
    support::mock_message(&server, "¡Hola!").await;
    Mock::given(method("POST"))
        .and(path("/chats/chat1/messages"))
        .and(body_partial_json(
            json!({"body":{"contentType":"text","content":"¡Hola!"}}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":"sent1"})))
        .expect(1)
        .mount(&server)
        .await;
    let pipeline = Pipeline {
        config: graph.config.clone(),
        store: store.clone(),
        adapter: graph,
        gate: Arc::new(NoGate),
        knowledge: knowledge(&dir),
        llm: Arc::new(NoLlm),
        tools: Arc::new(NoTools),
        redactor: redactor(),
    };
    store.enqueue("chats/chat1/messages/123").unwrap();
    let job = store.next_job().unwrap().unwrap();
    pipeline.process(&job.resource).await.unwrap();
    assert_eq!(store.audit(&job.resource).unwrap().unwrap().status, "sent");
    assert!(!store.enqueue(&job.resource).unwrap());
    assert!(store.next_job().unwrap().is_none());
}
#[tokio::test]
async fn graph_jev_deepseek_end_to_end_and_final_gate() {
    for allow in [true, false] {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = support::store(&dir);
        let graph = support::graph(&server, store.clone());
        support::mock_message(&server, "¿Cuál es el horario del soporte?").await;
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        Mock::given(method("POST")).and(path("/v1/systemone")).respond_with(move |_req:&wiremock::Request| {
            let n=counter.fetch_add(1,Ordering::SeqCst);
            let answers = if n == 0 {
                json!({"decision":support::choice(&["hours","ignore"],"hours",0.99),"safety":{"type":"noul","noul":0.99}})
            } else if n == 1 {
                json!({"relevant":{"type":"noul","noul":0.99},"qualified":{"type":"noul","noul":0.99},"safe":{"type":"noul","noul":0.99}})
            } else {
                json!({"supported":{"type":"noul","noul":if allow {0.99} else {0.01}},"no_new_promise":{"type":"noul","noul":0.99},"privacy":{"type":"noul","noul":0.99},"relevant":{"type":"noul","noul":0.99}})
            };
            ResponseTemplate::new(200).set_body_json(json!({"model":"jev-test","answers":answers}))
        }).expect(3).mount(&server).await;
        Mock::given(method("POST")).and(path("/chat/completions")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"gen1","object":"chat.completion","created":1,"model":"deepseek-test","choices":[{"index":0,"message":{"role":"assistant","content":"El soporte atiende de lunes a viernes de 09:00 a 18:00."},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":10,"total_tokens":20,"prompt_cache_hit_tokens":0,"prompt_cache_miss_tokens":10}}))).expect(1).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/chats/chat1/messages"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":"sent1"})))
            .expect(if allow { 1 } else { 0 })
            .mount(&server)
            .await;
        let gate = Arc::new(Jev {
            client: reqwest::Client::new(),
            endpoint: format!("{}/v1/systemone", server.uri()),
            api_key: "test-typesafe-key".into(),
            model: "jev-test".into(),
        });
        let llm = Arc::new(
            DeepSeek::new(
                "test-deepseek-key",
                "deepseek-test",
                "Brief Spanish",
                &server.uri(),
            )
            .unwrap(),
        );
        let pipeline = Pipeline {
            config: graph.config.clone(),
            store: store.clone(),
            adapter: graph,
            gate,
            knowledge: knowledge(&dir),
            llm,
            tools: Arc::new(NoTools),
            redactor: redactor(),
        };
        store.enqueue("chats/chat1/messages/123").unwrap();
        let job = store.next_job().unwrap().unwrap();
        pipeline.process(&job.resource).await.unwrap();
        let audit = store.audit(&job.resource).unwrap().unwrap();
        assert_eq!(audit.status, if allow { "sent" } else { "ignored" });
        assert_eq!(audit.confidences.len(), 3);
        let requests = server.received_requests().await.unwrap();
        for r in requests
            .iter()
            .filter(|r| r.url.path() == "/v1/systemone" || r.url.path() == "/chat/completions")
        {
            let body = String::from_utf8_lossy(&r.body);
            assert!(!body.contains("test-typesafe-key"));
            assert!(!body.contains("test-deepseek-key"));
            assert!(!body.contains("test-access-token"));
        }
    }
}
#[tokio::test]
async fn jev_follow_up_uses_yes_probability_and_checks_fail_closed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "answers":{"continuation":{"type":"noul","noul":0.03}}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let gate = Jev {
        client: reqwest::Client::new(),
        endpoint: format!("{}/v1/systemone", server.uri()),
        api_key: "test-key".into(),
        model: "jev-test".into(),
    };
    let verdict = gate
        .evaluate(
            Stage::FollowUp,
            json!({"current":"otra pregunta","previous_question":"estado","previous_answer":"respuesta"}),
            BTreeMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(verdict.selected, "new_topic");
    assert!(verdict.allows(0.9));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "answers":{"relevant":{"type":"noul","noul":0.99},"qualified":{"type":"noul","noul":0.99},"safe":{"type":"noul","noul":0.12}}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let gate = Jev {
        client: reqwest::Client::new(),
        endpoint: format!("{}/v1/systemone", server.uri()),
        api_key: "test-key".into(),
        model: "jev-test".into(),
    };
    let verdict = gate
        .evaluate(
            Stage::Evidence,
            json!({"question":"status","evidence":"sample"}),
            BTreeMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(verdict.selected, "ignore");
    assert!(!verdict.allows(0.7));
}
#[tokio::test]
async fn send_error_is_not_retried() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let graph = support::graph(&server, store.clone());
    support::mock_message(&server, "hola").await;
    Mock::given(method("POST"))
        .and(path("/chats/chat1/messages"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    let pipeline = Pipeline {
        config: graph.config.clone(),
        store: store.clone(),
        adapter: graph,
        gate: Arc::new(NoGate),
        knowledge: knowledge(&dir),
        llm: Arc::new(NoLlm),
        tools: Arc::new(NoTools),
        redactor: redactor(),
    };
    store.enqueue("chats/chat1/messages/123").unwrap();
    let job = store.next_job().unwrap().unwrap();
    pipeline.process(&job.resource).await.unwrap();
    store.retry(&job).unwrap();
    assert_eq!(
        store.status(&job.resource).unwrap().as_deref(),
        Some("uncertain")
    );
    assert!(store.next_job().unwrap().is_none());
}
#[tokio::test]
async fn webhook_validates_source_tenant_subscription_and_resource() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let graph = support::graph(&server, store.clone());
    store
        .save_subscription(&Subscription {
            id: "sub1".into(),
            resource: "chats/chat1/messages".into(),
            expires_at: chrono::Utc::now().timestamp() + 300,
        })
        .unwrap();
    let vault = Vault::new(&base64::engine::general_purpose::STANDARD.encode([1; 32])).unwrap();
    let oauth = Arc::new(
        OAuth::new(
            graph.config.clone(),
            reqwest::Client::new(),
            "test-client-secret".into(),
            store.clone(),
            vault,
        )
        .unwrap(),
    );
    let app = webhook::router(Arc::new(WebState {
        graph: graph.clone(),
        oauth,
        admin_key: "test-admin".into(),
        pipeline: None,
    }));
    let response = app
        .clone()
        .oneshot(
            Request::post("/graph/notifications?validationToken=a%2Bb")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    for (state, tenant, resource, status) in [
        (
            "wrong",
            graph.config.graph.tenant_id.as_str(),
            "chats/chat1/messages/123",
            StatusCode::FORBIDDEN,
        ),
        (
            graph.client_state.as_str(),
            "wrong-tenant",
            "chats/chat1/messages/123",
            StatusCode::FORBIDDEN,
        ),
        (
            graph.client_state.as_str(),
            graph.config.graph.tenant_id.as_str(),
            "chats/other/messages/123",
            StatusCode::FORBIDDEN,
        ),
        (
            graph.client_state.as_str(),
            graph.config.graph.tenant_id.as_str(),
            "chats('chat1')/messages('123')",
            StatusCode::ACCEPTED,
        ),
    ] {
        let body=json!({"value":[{"subscriptionId":"sub1","clientState":state,"tenantId":tenant,"resource":resource,"changeType":"created"}]}).to_string();
        let r = app
            .clone()
            .oneshot(
                Request::post("/graph/notifications")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), status);
    }
    assert!(store.next_job().unwrap().is_some());
    assert!(store.next_job().unwrap().is_none());
}
#[tokio::test]
async fn simulation_requires_admin_key_and_never_sends_to_graph() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let graph = support::graph(&server, store.clone());
    let pipeline = Arc::new(Pipeline {
        config: graph.config.clone(),
        store: store.clone(),
        adapter: graph.clone(),
        gate: Arc::new(NoGate),
        knowledge: knowledge(&dir),
        llm: Arc::new(NoLlm),
        tools: Arc::new(NoTools),
        redactor: redactor(),
    });
    let vault = Vault::new(&base64::engine::general_purpose::STANDARD.encode([3; 32])).unwrap();
    let oauth = Arc::new(
        OAuth::new(
            graph.config.clone(),
            reqwest::Client::new(),
            "secret".into(),
            store,
            vault,
        )
        .unwrap(),
    );
    let app = webhook::router(Arc::new(WebState {
        graph,
        oauth,
        admin_key: "test-admin".into(),
        pipeline: Some(pipeline),
    }));
    let body = json!({"session":"same-chat","text":"hola"}).to_string();
    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/test/chat")
                .header("content-type", "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let ignored_group = app
        .clone()
        .oneshot(
            Request::post("/test/chat")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-admin")
                .body(Body::from(
                    json!({"session":"group-test","text":"hola","group":true,"mentioned":false})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let ignored_bytes = axum::body::to_bytes(ignored_group.into_body(), 4000)
        .await
        .unwrap();
    let ignored: serde_json::Value = serde_json::from_slice(&ignored_bytes).unwrap();
    assert_eq!(ignored["status"], "ignored");
    assert!(ignored["answer"].is_null());
    let response = app
        .oneshot(
            Request::post("/test/chat")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-admin")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 4000)
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["answer"], "¡Hola!");
    assert!(server.received_requests().await.unwrap().is_empty());
}
#[test]
fn map_requires_conversation_authorization_and_no_path_escape() {
    let dir = tempfile::tempdir().unwrap();
    let map = knowledge(&dir);
    assert!(map.available("chats/other", "sender").is_empty());
    assert_eq!(map.available("chats/chat1", "sender").len(), 1);
    let text = include_str!("../knowledge-map.example.toml");
    assert!(KnowledgeMap::parse(text).is_ok());
    assert!(
        KnowledgeMap::parse(&text.replace(
            "path = \"temas/asistente/operacion.md\"",
            "path = \"../escape.md\""
        ))
        .is_err()
    );
}
#[tokio::test]
async fn jev_rejects_missing_and_invalid_decisions() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"answers":{"decision":{"type":"choice","choice":"allow","confidence":0.999}}}),
        ))
        .mount(&server)
        .await;
    let gate = Jev {
        client: reqwest::Client::new(),
        endpoint: server.uri(),
        api_key: "test-key".into(),
        model: "test".into(),
    };
    assert!(
        gate.evaluate(Stage::Final, json!({}), BTreeMap::new())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn graph_refreshes_and_renews_subscriptions() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let graph = support::graph(&server, store.clone());
    Mock::given(method("GET"))
        .and(path("/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[]})))
        .mount(&server)
        .await;
    Mock::given(method("POST")).and(path("/subscriptions")).and(body_partial_json(json!({"resource":"chats/chat1/messages","includeResourceData":false}))).respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":"sub1","expirationDateTime":(chrono::Utc::now()+chrono::Duration::minutes(50)).to_rfc3339()}))).expect(1).mount(&server).await;
    Mock::given(method("PATCH")).and(path("/subscriptions/sub1")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"sub1","expirationDateTime":(chrono::Utc::now()+chrono::Duration::minutes(50)).to_rfc3339()}))).expect(1).mount(&server).await;
    graph.reconcile_subscriptions().await.unwrap();
    assert_eq!(store.subscriptions().unwrap().len(), 1);
    store.expire_subscription("sub1").unwrap();
    graph.reconcile_subscriptions().await.unwrap();
}
#[tokio::test]
async fn all_chats_use_one_subscription_and_accept_chat_notifications() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let mut cfg = support::config();
    cfg.graph.discover_all_chats = true;
    let graph = Arc::new(Graph {
        client: reqwest::Client::new(),
        token: Arc::new(support::Token),
        base_url: server.uri(),
        config: Arc::new(cfg),
        store: store.clone(),
        client_state: "test-webhook-shared-secret-32-chars".into(),
    });
    Mock::given(method("GET"))
        .and(path("/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/subscriptions"))
        .and(body_partial_json(json!({"resource":graph.user_messages_resource()})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":"all","expirationDateTime":(chrono::Utc::now()+chrono::Duration::minutes(50)).to_rfc3339()})))
        .expect(1)
        .mount(&server)
        .await;
    graph.reconcile_subscriptions().await.unwrap();
    assert_eq!(store.subscriptions().unwrap().len(), 1);
    let vault = Vault::new(&base64::engine::general_purpose::STANDARD.encode([1; 32])).unwrap();
    let oauth = Arc::new(
        OAuth::new(
            graph.config.clone(),
            reqwest::Client::new(),
            "secret".into(),
            store.clone(),
            vault,
        )
        .unwrap(),
    );
    let app = webhook::router(Arc::new(WebState {
        graph: graph.clone(),
        oauth,
        admin_key: "test-admin".into(),
        pipeline: None,
    }));
    let body = json!({"value":[{"subscriptionId":"all","clientState":graph.client_state,"tenantId":graph.config.graph.tenant_id,"resource":"chats('chat2')/messages('123')","changeType":"created"}]}).to_string();
    let response = app
        .oneshot(
            Request::post("/graph/notifications")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        store.next_job().unwrap().unwrap().resource,
        "chats/chat2/messages/123"
    );
}
#[tokio::test]
async fn graph_fetch_rejects_unapproved_destination() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let graph = support::graph(&server, support::store(&dir));
    assert!(graph.fetch("chats/other/messages/123").await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn changed_tunnel_recreates_only_owned_subscriptions() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let graph = support::graph(&server, store.clone());
    let expiration = (chrono::Utc::now() + chrono::Duration::minutes(50)).to_rfc3339();
    store
        .save_subscription(&Subscription {
            id: "old".into(),
            resource: "chats/chat1/messages".into(),
            expires_at: chrono::Utc::now().timestamp() + 3000,
        })
        .unwrap();
    Mock::given(method("GET")).and(path("/subscriptions")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[{"id":"old","resource":"chats/chat1/messages","clientState":null,"notificationUrl":"https://old.example.com/graph/notifications","expirationDateTime":expiration},{"id":"unrelated","resource":"chats/chat1/messages","clientState":"someone-else","notificationUrl":"https://other.example.com"}]}))).mount(&server).await;
    Mock::given(method("DELETE"))
        .and(path("/subscriptions/old"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/subscriptions/unrelated"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/subscriptions"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"id":"new","expirationDateTime":expiration})),
        )
        .expect(1)
        .mount(&server)
        .await;
    graph.reconcile_subscriptions().await.unwrap();
    assert_eq!(store.subscriptions().unwrap()[0].id, "new");
}

#[tokio::test]
async fn dry_run_and_sensitive_question_never_send() {
    for text in ["hola", "mi password=supersecret"] {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = support::store(&dir);
        let graph = support::graph(&server, store.clone());
        support::mock_message(&server, text).await;
        let mut cfg = support::config();
        cfg.policy.dry_run = true;
        let pipeline = Pipeline {
            config: Arc::new(cfg),
            store: store.clone(),
            adapter: graph,
            gate: Arc::new(NoGate),
            knowledge: knowledge(&dir),
            llm: Arc::new(NoLlm),
            tools: Arc::new(NoTools),
            redactor: redactor(),
        };
        store.enqueue("chats/chat1/messages/123").unwrap();
        let job = store.next_job().unwrap().unwrap();
        pipeline.process(&job.resource).await.unwrap();
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method.as_str() != "POST")
        );
        let audit = store.audit(&job.resource).unwrap().unwrap();
        assert_eq!(
            audit.status,
            if text == "hola" { "dry_run" } else { "ignored" }
        );
    }
}

#[tokio::test]
async fn oauth_form_allows_microsoft_redirect_and_sets_secure_cookie() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let graph = support::graph(&server, store.clone());
    let oauth = Arc::new(
        OAuth::new(
            graph.config.clone(),
            reqwest::Client::new(),
            "test-client-secret".into(),
            store,
            Vault::new(&base64::engine::general_purpose::STANDARD.encode([2; 32])).unwrap(),
        )
        .unwrap(),
    );
    let app = webhook::router(Arc::new(WebState {
        graph,
        oauth,
        admin_key: "test-admin".into(),
        pipeline: None,
    }));
    let page = app
        .clone()
        .oneshot(Request::get("/oauth/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let csp = page.headers()["content-security-policy"].to_str().unwrap();
    assert!(
        csp.contains("form-action 'self' https://login.microsoftonline.com"),
        "CSP must allow the OAuth POST redirect destination"
    );
    let response = app
        .oneshot(
            Request::post("/oauth/start")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("key=test-admin"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        response.headers()["location"]
            .to_str()
            .unwrap()
            .starts_with("https://login.microsoftonline.com/")
    );
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("HttpOnly; Secure; SameSite=Lax"));
}
