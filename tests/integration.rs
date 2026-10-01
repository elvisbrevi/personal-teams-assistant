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
use std::{collections::BTreeMap, sync::Arc};
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
        Mock::given(method("POST")).and(path("/v1/systemone")).respond_with(move |_req:&wiremock::Request| {
            let answers = json!({"references":{"type":"noul","noul":0.99},"attribution":{"type":"noul","noul":0.99},"supported":{"type":"noul","noul":if allow {0.99} else {0.01}},"no_new_promise":{"type":"noul","noul":0.99},"privacy":{"type":"noul","noul":0.99},"relevant":{"type":"noul","noul":0.99}});
            ResponseTemplate::new(200).set_body_json(json!({"model":"jev-test","answers":answers}))
        }).expect(1).mount(&server).await;
        Mock::given(method("POST")).and(path("/chat/completions")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"gen1","object":"chat.completion","created":1,"model":"deepseek-test","choices":[{"index":0,"message":{"role":"assistant","content":"{\"answer\":\"El soporte atiende de lunes a viernes de 09:00 a 18:00.\",\"detailed\":false}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":10,"total_tokens":20,"prompt_cache_hit_tokens":0,"prompt_cache_miss_tokens":10}}))).expect(1).mount(&server).await;
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
        assert_eq!(audit.confidences.len(), 1);
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
struct FinalOnlyGate;
#[async_trait]
impl DecisionGate for FinalOnlyGate {
    async fn evaluate(
        &self,
        stage: Stage,
        _: serde_json::Value,
        _: BTreeMap<String, String>,
    ) -> Result<personal_teams_assistant::decision::Verdict> {
        assert!(
            matches!(stage, Stage::Final),
            "a semantic prefilter discarded an answerable question: {stage:?}"
        );
        Ok(personal_teams_assistant::decision::Verdict {
            selected: "allow".into(),
            confidence: 0.99,
        })
    }
}
struct CompoundAnswer;
#[async_trait]
impl LlmProvider for CompoundAnswer {
    fn name(&self) -> &str {
        "synthetic"
    }
    async fn generate(&self, input: GenerationInput<'_>) -> Result<String> {
        assert!(input.evidence.contains("09:00 a 18:00"));
        assert!(input.evidence.contains("domingo de 02:00 a 03:00"));
        assert!(!input.evidence.contains("UNAUTHORIZED"));
        assert!(input.evidence.chars().count() <= 16000);
        Ok(
            "Soporte: lunes a viernes de 09:00 a 18:00. Mantenimiento: domingo de 02:00 a 03:00."
                .into(),
        )
    }
}
#[tokio::test]
async fn compound_and_new_topic_questions_read_authorized_sources_without_semantic_prefilters() {
    for with_history in [false, true] {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = support::store(&dir);
        let graph = support::graph(&server, store.clone());
        support::mock_message(
            &server,
            "¿Cuál es el horario de soporte y cuándo es el mantenimiento?",
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/chats/chat1/messages"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":"sent1"})))
            .expect(1)
            .mount(&server)
            .await;
        let mut map = knowledge(&dir);
        std::fs::write(
            dir.path().join("maintenance.md"),
            "El mantenimiento es el domingo de 02:00 a 03:00.",
        )
        .unwrap();
        let mut maintenance = map.resources[0].clone();
        maintenance.id = "maintenance".into();
        maintenance.description = "Ventana de mantenimiento".into();
        maintenance.access = Access::File {
            repository: "private".into(),
            path: "maintenance.md".into(),
        };
        map.resources.push(maintenance.clone());
        std::fs::write(dir.path().join("private.md"), "UNAUTHORIZED").unwrap();
        for (id, enabled, external, conversation, sender) in [
            ("wrong-chat", true, true, "chats/other", ""),
            ("disabled", false, true, "chats/chat1", ""),
            ("local-only", true, false, "chats/chat1", ""),
            ("wrong-sender", true, true, "chats/chat1", "other"),
        ] {
            let mut blocked = maintenance.clone();
            blocked.id = id.into();
            blocked.enabled = enabled;
            blocked.external_processing = external;
            blocked.allowed_conversations = vec![conversation.into()];
            blocked.allowed_senders = if sender.is_empty() {
                vec![]
            } else {
                vec![sender.into()]
            };
            blocked.access = Access::File {
                repository: "private".into(),
                path: "private.md".into(),
            };
            map.resources.push(blocked);
        }
        if with_history {
            store
                .save_context(
                    "chats/chat1",
                    "¿Cómo está el proyecto anterior?",
                    "Requiere confirmación.",
                )
                .unwrap();
        }
        let pipeline = Pipeline {
            config: graph.config.clone(),
            store: store.clone(),
            adapter: graph,
            gate: Arc::new(FinalOnlyGate),
            knowledge: map,
            llm: Arc::new(CompoundAnswer),
            tools: Arc::new(NoTools),
            redactor: redactor(),
        };
        store.enqueue("chats/chat1/messages/123").unwrap();
        pipeline.process("chats/chat1/messages/123").await.unwrap();
        let audit = store.audit("chats/chat1/messages/123").unwrap().unwrap();
        assert_eq!(audit.status, "sent");
        assert_eq!(audit.source.as_deref(), Some("hours,maintenance"));
        assert_eq!(
            store.context("chats/chat1").unwrap().unwrap().question,
            "¿Cuál es el horario de soporte y cuándo es el mantenimiento?"
        );
    }
}
struct UncertainToolGate;
#[async_trait]
impl DecisionGate for UncertainToolGate {
    async fn evaluate(
        &self,
        stage: Stage,
        _: serde_json::Value,
        _: BTreeMap<String, String>,
    ) -> Result<personal_teams_assistant::decision::Verdict> {
        Ok(match stage {
            Stage::Routing => personal_teams_assistant::decision::Verdict {
                selected: "status-tool".into(),
                confidence: 0.48,
            },
            Stage::Final => personal_teams_assistant::decision::Verdict {
                selected: "allow".into(),
                confidence: 0.99,
            },
            _ => panic!("unexpected semantic prefilter"),
        })
    }
}
struct AnswerOrClarify(bool);
#[async_trait]
impl LlmProvider for AnswerOrClarify {
    fn name(&self) -> &str {
        "synthetic"
    }
    async fn generate(&self, input: GenerationInput<'_>) -> Result<String> {
        if self.0 {
            assert!(input.evidence.contains("09:00 a 18:00"));
            Ok("Soporte: lunes a viernes de 09:00 a 18:00.".into())
        } else {
            assert!(
                input
                    .evidence
                    .contains("No se recuperaron hechos verificables")
            );
            Ok("No pude verificar el horario. ¿Qué fuente de soporte debo consultar?".into())
        }
    }
}
#[tokio::test]
async fn uncertain_tool_routing_answers_from_documents_or_requests_clarification_without_running_tools()
 {
    for with_document in [true, false] {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = support::store(&dir);
        let graph = support::graph(&server, store.clone());
        support::mock_message(&server, "¿Cuál es el horario de soporte?").await;
        Mock::given(method("POST"))
            .and(path("/chats/chat1/messages"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":"sent1"})))
            .expect(1)
            .mount(&server)
            .await;
        let mut map = knowledge(&dir);
        let mut tool = map.resources[0].clone();
        tool.id = "status-tool".into();
        tool.access = Access::Tool {
            tool: ToolSpec::Http {
                url: "https://example.com/status".into(),
                secret_ref: None,
            },
        };
        if !with_document {
            map.resources.clear();
        }
        map.resources.push(tool);
        let pipeline = Pipeline {
            config: graph.config.clone(),
            store: store.clone(),
            adapter: graph,
            gate: Arc::new(UncertainToolGate),
            knowledge: map,
            llm: Arc::new(AnswerOrClarify(with_document)),
            tools: Arc::new(NoTools),
            redactor: redactor(),
        };
        store.enqueue("chats/chat1/messages/123").unwrap();
        pipeline.process("chats/chat1/messages/123").await.unwrap();
        let audit = store.audit("chats/chat1/messages/123").unwrap().unwrap();
        assert_eq!(audit.status, "sent");
        assert!(audit.tools.is_empty());
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
    let state = Arc::new(WebState {
        graph,
        oauth,
        admin_key: "test-admin".into(),
        pipeline: Some(pipeline),
    });
    let public = webhook::public_router(state.clone());
    for path in ["/test", "/test/chat", "/oauth/login", "/oauth/callback"] {
        let response = public
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    let app = webhook::router(state);
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
async fn recovers_subscription_when_graph_redacts_client_state() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let graph = support::graph(&server, store.clone());
    let cfg = &graph.config;
    Mock::given(method("GET"))
        .and(path("/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[{
            "id":"existing",
            "resource":"chats/chat1/messages",
            "clientState":null,
            "applicationId":cfg.graph.client_id,
            "creatorId":cfg.graph.user_id,
            "notificationUrl":"https://assistant.example.com/graph/notifications",
            "expirationDateTime":(chrono::Utc::now()+chrono::Duration::minutes(50)).to_rfc3339()
        }]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/subscriptions"))
        .respond_with(ResponseTemplate::new(409))
        .expect(0)
        .mount(&server)
        .await;
    graph.reconcile_subscriptions().await.unwrap();
    assert_eq!(store.subscriptions().unwrap()[0].id, "existing");
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

#[tokio::test]
async fn self_chat_is_scoped_durable_and_does_not_confuse_equal_human_text() {
    use personal_teams_assistant::{adapters::teams::IncomingMessage, config::SelfChat};
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let mut config = support::config();
    let user = config.graph.user_id.clone();
    config.graph.self_chat = Some(SelfChat {
        id: "chat1".into(),
        user_id: user.clone(),
        enabled_at: chrono::Utc::now().timestamp_millis() - 60_000,
    });
    let graph = Arc::new(Graph {
        config: Arc::new(config),
        ..(*support::graph(&server, store.clone())).clone()
    });
    Mock::given(method("GET"))
        .and(path("/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":user})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/chats/chat1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chatType":"oneOnOne"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/chats/chat1/members"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[{"userId":user}]})))
        .mount(&server)
        .await;
    for id in ["123", "human-equal", "old"] {
        Mock::given(method("GET")).and(path(format!("/chats/chat1/messages/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":id,"messageType":"message","createdDateTime":if id=="old" {(chrono::Utc::now()-chrono::Duration::seconds(120)).to_rfc3339()}else {chrono::Utc::now().to_rfc3339()},"deletedDateTime":null,"from":{"user":{"id":user}},"body":{"contentType":"text","content":"¡Hola!"},"mentions":[]}))).mount(&server).await;
    }
    Mock::given(method("POST"))
        .and(path("/chats/chat1/messages"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":"output-id"})))
        .expect(2)
        .mount(&server)
        .await;
    let pipeline = Pipeline {
        config: graph.config.clone(),
        store: store.clone(),
        adapter: graph.clone(),
        gate: Arc::new(NoGate),
        knowledge: knowledge(&dir),
        llm: Arc::new(NoLlm),
        tools: Arc::new(NoTools),
        redactor: redactor(),
    };
    for id in ["123", "old", "human-equal"] {
        let resource = format!("chats/chat1/messages/{id}");
        store.enqueue(&resource).unwrap();
        pipeline.process(&resource).await.unwrap();
    }
    assert_eq!(
        store
            .audit("chats/chat1/messages/old")
            .unwrap()
            .unwrap()
            .status,
        "ignored"
    );
    assert_eq!(
        store
            .audit("chats/chat1/messages/human-equal")
            .unwrap()
            .unwrap()
            .status,
        "sent"
    );
    assert!(store.is_output("chats/chat1", "output-id", "").unwrap());
    assert!(
        !store
            .is_output("chats/chat1", "human-other", "¡Hola!")
            .unwrap()
    );
    let message: IncomingMessage = graph.fetch("chats/chat1/messages/123").await.unwrap();
    let mut third_party = message.clone();
    third_party.conversation = "chats/third-party".into();
    assert!(!third_party.eligible_in(&user, &[], 300, graph.config.graph.self_chat.as_ref()));
    let unknown = store.begin_output("chats/chat1").unwrap();
    // If Graph strips the marker and the send result is lost, fail closed.
    assert!(store.has_unresolved_output("chats/chat1").unwrap());
    assert!(
        !graph
            .fetch("chats/chat1/messages/human-equal")
            .await
            .unwrap()
            .is_user_message
    );
    // A webhook can arrive before POST returns, or after an ambiguous send/restart.
    let marker = format!("<a href=\"https://personalteams.invalid/output/{unknown}\">PTA</a>");
    assert!(
        store
            .is_output("chats/chat1", "early-output", &marker)
            .unwrap()
    );
    assert!(!store.has_unresolved_output("chats/chat1").unwrap());
    assert!(
        graph
            .fetch("chats/chat1/messages/human-equal")
            .await
            .unwrap()
            .is_user_message
    );
    let reopened =
        personal_teams_assistant::state::Store::open(&dir.path().join("test.db")).unwrap();
    assert!(
        reopened
            .is_output("chats/chat1", "early-output", "")
            .unwrap()
    );
    assert!(
        !reopened
            .is_output("chats/third-party", "early-output", &marker)
            .unwrap()
    );
    assert!(!reopened.enqueue("chats/chat1/messages/123").unwrap());
}

#[test]
fn diagnostic_queries_leave_processing_jobs_untouched_and_hide_content() {
    use personal_teams_assistant::state::{Audit, Store};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let store = Store::open(&path).unwrap();
    store.enqueue("chats/chat1/messages/123").unwrap();
    store.next_job().unwrap();
    store
        .record(
            "chats/chat1/messages/123",
            &Audit {
                status: "processing".into(),
                proposed: Some("private-content-canary".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let safe = Store::inspect(&path, false, 20, None, false).unwrap();
    assert!(
        !serde_json::to_string(&safe)
            .unwrap()
            .contains("private-content-canary")
    );
    assert_eq!(
        store.status("chats/chat1/messages/123").unwrap().as_deref(),
        Some("processing")
    );
    assert!(
        Store::inspect(&path, false, 20, None, true).unwrap()[0]["audit"]["proposed"].is_string()
    );
    assert!(!Store::has_token(&path).unwrap());
    assert_eq!(
        store.status("chats/chat1/messages/123").unwrap().as_deref(),
        Some("processing")
    );
}

struct LengthChoice {
    detailed: bool,
}
#[async_trait]
impl LlmProvider for LengthChoice {
    fn name(&self) -> &str {
        "length-choice"
    }
    async fn generate(&self, _: GenerationInput<'_>) -> Result<String> {
        unreachable!()
    }
    async fn generate_response(
        &self,
        input: GenerationInput<'_>,
    ) -> Result<personal_teams_assistant::llm::GeneratedAnswer> {
        assert_eq!(input.max_answer_chars, 30);
        assert_eq!(input.max_detailed_answer_chars, 100);
        Ok(personal_teams_assistant::llm::GeneratedAnswer {
            used_sources: Vec::new(),
            answer: "á".repeat(60),
            detailed: self.detailed,
        })
    }
}
#[tokio::test]
async fn agent_selected_character_limits_are_enforced_before_sending() {
    for detailed in [false, true] {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = support::store(&dir);
        let mut config = support::config();
        config.policy.max_answer_chars = 30;
        config.policy.max_detailed_answer_chars = 100;
        config.validate().unwrap();
        let graph = Arc::new(Graph {
            config: Arc::new(config),
            ..(*support::graph(&server, store.clone())).clone()
        });
        support::mock_message(&server, "¿Cuál es el horario de soporte?").await;
        Mock::given(method("POST"))
            .and(path("/chats/chat1/messages"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":"sent-limit"})))
            .expect(if detailed { 1 } else { 0 })
            .mount(&server)
            .await;
        let pipeline = Pipeline {
            config: graph.config.clone(),
            store: store.clone(),
            adapter: graph,
            gate: Arc::new(FinalOnlyGate),
            knowledge: knowledge(&dir),
            llm: Arc::new(LengthChoice { detailed }),
            tools: Arc::new(NoTools),
            redactor: redactor(),
        };
        store.enqueue("chats/chat1/messages/123").unwrap();
        pipeline.process("chats/chat1/messages/123").await.unwrap();
        let audit = store.audit("chats/chat1/messages/123").unwrap().unwrap();
        assert_eq!(audit.detailed, Some(detailed));
        assert_eq!(audit.answer_limit, Some(if detailed { 100 } else { 30 }));
        assert_eq!(audit.status, if detailed { "sent" } else { "ignored" });
    }
}

#[tokio::test]
async fn reserved_notes_require_delegated_account_authorship_without_chat_metadata() {
    use personal_teams_assistant::config::SelfChat;
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let mut config = support::config();
    let user = config.graph.user_id.clone();
    config.graph.self_chat = Some(SelfChat {
        id: "48:notes".into(),
        user_id: user.clone(),
        enabled_at: chrono::Utc::now().timestamp_millis() - 60_000,
    });
    let graph = Graph {
        config: Arc::new(config),
        ..(*support::graph(&server, store.clone())).clone()
    };
    let message = json!({"id":"notes-1","messageType":"message","createdDateTime":chrono::Utc::now().to_rfc3339(),"deletedDateTime":null,"from":{"user":{"id":user}},"body":{"contentType":"text","content":"hola"},"mentions":[]});
    Mock::given(method("GET"))
        .and(path("/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":user})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/me/chats/48:notes/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[message]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/me/chats/48:notes/messages/notes-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message))
        .mount(&server)
        .await;
    assert_eq!(graph.discover_self_chat().await.unwrap(), "48:notes");
    graph.poll_self_chat().await.unwrap();
    assert_eq!(
        store
            .status("chats/48:notes/messages/notes-1")
            .unwrap()
            .as_deref(),
        Some("pending")
    );
    assert!(
        graph
            .fetch("chats/48:notes/messages/notes-1")
            .await
            .unwrap()
            .eligible_in(&user, &[], 300, graph.config.graph.self_chat.as_ref())
    );
    Mock::given(method("GET"))
        .and(path("/me/chats/48:notes/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"value":[{"messageType":"message","from":{"user":{"id":"another-user"}}}]}),
        ))
        .with_priority(1)
        .mount(&server)
        .await;
    assert!(graph.validate_self_chat("48:notes").await.is_err());
    assert!(!server.received_requests().await.unwrap().iter().any(|r| r.url.path()=="/chats/48:notes" || r.url.path()=="/chats/48:notes/members"));
}

struct WikiTools {
    result: serde_json::Value,
}
#[async_trait]
impl ReadOnlyTool for WikiTools {
    async fn execute(&self, spec: &ToolSpec, question: &str, _: &str) -> Result<String> {
        assert!(
            matches!(spec, ToolSpec::AzureDevopsWiki { .. }),
            "explicit Wiki must beat activity routing"
        );
        assert_eq!(question, "Según la wiki, ¿cómo se configura el pipeline?");
        assert!(!question.contains("Contexto anterior"));
        Ok(self.result.to_string())
    }
}
struct WikiLlm {
    used: Vec<String>,
}
#[async_trait]
impl LlmProvider for WikiLlm {
    fn name(&self) -> &str {
        "synthetic-wiki"
    }
    async fn generate(&self, _: GenerationInput<'_>) -> Result<String> {
        unreachable!()
    }
    async fn generate_response(
        &self,
        input: GenerationInput<'_>,
    ) -> Result<personal_teams_assistant::llm::GeneratedAnswer> {
        assert!(input.evidence.contains("Procedimiento verificable"));
        assert!(input.evidence.contains("wiki:test:page1"));
        Ok(personal_teams_assistant::llm::GeneratedAnswer {
            answer: "El procedimiento documentado exige validar la configuración.".into(),
            detailed: false,
            used_sources: self.used.clone(),
        })
    }
}
struct WikiGate;
#[async_trait]
impl DecisionGate for WikiGate {
    async fn evaluate(
        &self,
        stage: Stage,
        state: serde_json::Value,
        _: BTreeMap<String, String>,
    ) -> Result<personal_teams_assistant::decision::Verdict> {
        assert!(matches!(stage, Stage::Final));
        let answer = state["answer"].as_str().unwrap();
        assert!(answer.contains("pagePath=%2FProcedure"));
        assert!(answer.contains("último editor registrado: Ana"));
        assert!(answer.chars().count() <= 3000);
        assert_eq!(state["used_sources"].as_array().unwrap().len(), 2);
        assert_eq!(state["references"].as_array().unwrap().len(), 2);
        Ok(personal_teams_assistant::decision::Verdict {
            selected: "allow".into(),
            confidence: 0.99,
        })
    }
}
fn synthetic_wiki_result() -> serde_json::Value {
    let wiki = json!({"organization":"https://dev.azure.com/test","project":"Project","project_id":"abcdeabc-abcd-abcd-abcd-abcdeabcdea1","id":"abcdeabc-abcd-abcd-abcd-abcdeabcdea2","name":"Wiki","kind":"projectWiki","repository_id":"abcdeabc-abcd-abcd-abcd-abcdeabcdea3","mapped_path":"/","versions":["published"]});
    let pages:Vec<_>=[("page1","edited_by_me",None),("page2","other",Some("Ana"))].into_iter().map(|(id,authority,author)|json!({"wiki":wiki,"title":id,"path":"/Procedure","git_item_path":"/Procedure.md","version":"published","revision":"1111111111111111111111111111111111111111","content":"Procedimiento verificable: validar configuración.","reference":{"id":format!("wiki:test:{id}"),"kind":"wiki","label":format!("Project / Wiki / {id}"),"url":format!("https://dev.azure.com/test/abcdeabc-abcd-abcd-abcd-abcdeabcdea1/_wiki/wikis/abcdeabc-abcd-abcd-abcd-abcdeabcdea2?pagePath=%2FProcedure&anchor={id}"),"organization":"https://dev.azure.com/test","project":"abcdeabc-abcd-abcd-abcd-abcdeabcdea1","aliases":[],"parent":null,"revision":"1111111111111111111111111111111111111111","authority":authority,"author":author,"author_role":"último editor registrado"}})).collect();
    json!({"query":"configuración pipeline","wikis":[],"pages":pages,"candidates":2,"histories_checked":2,"partial":false,"warnings":[]})
}
fn wiki_resource() -> Resource {
    Resource {
        id: "manuals".into(),
        description: "Wiki knowledge".into(),
        topics: vec!["wiki".into()],
        enabled: true,
        external_processing: true,
        allowed_conversations: vec![],
        allowed_senders: vec![],
        access: Access::Tool {
            tool: ToolSpec::AzureDevopsWiki {
                repository: "private".into(),
                path: "catalog.toml".into(),
                secret_ref: "secret://ado/read".into(),
                wiki_ids: vec![],
                author_mode: personal_teams_assistant::ado::wiki::AuthorMode::PreferMine,
            },
        },
    }
}
#[tokio::test]
async fn wiki_priority_current_request_mixed_citations_and_complete_gate_without_graph_send() {
    for ids in [
        vec!["wiki:test:page1".into(), "wiki:test:page2".into()],
        vec![],
        vec!["invented".into()],
    ] {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = support::store(&dir);
        let graph = support::graph(&server, store.clone());
        let mut cfg = (*graph.config).clone();
        cfg.policy.dry_run = true;
        store
            .save_context(
                "chats/simulation-wiki",
                "Pregunta anterior secreta diferente",
                "Respuesta anterior no es evidencia",
            )
            .unwrap();
        let pipeline = Pipeline {
            config: Arc::new(cfg),
            store: store.clone(),
            adapter: graph,
            gate: if ids.len() == 2 {
                Arc::new(WikiGate)
            } else {
                Arc::new(NoGate)
            },
            knowledge: KnowledgeMap {
                repositories: BTreeMap::new(),
                resources: vec![
                    wiki_resource(),
                    Resource {
                        id: "azure-devops-status".into(),
                        description: "Activity".into(),
                        topics: vec![],
                        enabled: true,
                        external_processing: true,
                        allowed_conversations: vec![],
                        allowed_senders: vec![],
                        access: Access::Tool {
                            tool: ToolSpec::AzureDevopsStatus {
                                repository: "private".into(),
                                path: "catalog.toml".into(),
                                secret_ref: "secret://ado/read".into(),
                            },
                        },
                    },
                ],
            },
            llm: Arc::new(WikiLlm { used: ids.clone() }),
            tools: Arc::new(WikiTools {
                result: synthetic_wiki_result(),
            }),
            redactor: redactor(),
        };
        let sources = vec!["manuals".into(), "azure-devops-status".into()];
        let result = personal_teams_assistant::simulation::run(
            &pipeline,
            personal_teams_assistant::simulation::SimulationRequest {
                session: "wiki".into(),
                text: "Según la wiki, ¿cómo se configura el pipeline?".into(),
                group: false,
                mentioned: false,
                sources: sources.clone(),
            },
            Some(&sources),
        )
        .await
        .unwrap();
        if ids.len() == 2 {
            assert_eq!(result.status, "dry_run", "{}", result.reason);
            assert!(
                result
                    .answer
                    .unwrap()
                    .contains("contribución propia verificada")
            );
        } else {
            assert_eq!(result.status, "ignored");
            assert!(result.reason.starts_with("invalid_references"));
        }
        assert!(
            pipeline.knowledge.resources[0]
                .allowed_conversations
                .is_empty()
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
#[tokio::test]
async fn explicit_local_selection_still_requires_enabled_and_external_processing() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let graph = support::graph(&server, store.clone());
    for (enabled, external) in [(false, true), (true, false)] {
        let mut source = wiki_resource();
        source.enabled = enabled;
        source.external_processing = external;
        let pipeline = Pipeline {
            config: graph.config.clone(),
            store: store.clone(),
            adapter: graph.clone(),
            gate: Arc::new(NoGate),
            knowledge: KnowledgeMap {
                repositories: BTreeMap::new(),
                resources: vec![source],
            },
            llm: Arc::new(NoLlm),
            tools: Arc::new(NoTools),
            redactor: redactor(),
        };
        let sources = vec!["manuals".into()];
        let result = personal_teams_assistant::simulation::run(
            &pipeline,
            personal_teams_assistant::simulation::SimulationRequest {
                session: "permissions".into(),
                text: "Según la wiki, ¿cómo se configura el pipeline?".into(),
                group: false,
                mentioned: false,
                sources: sources.clone(),
            },
            Some(&sources),
        )
        .await
        .unwrap();
        assert_eq!(result.reason, "no_authorized_resource");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}
#[tokio::test]
async fn teams_context_preserves_two_interlocutors_own_messages_and_omits_unrelated_members() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let graph = support::graph(&server, store);
    let date = chrono::Utc::now().to_rfc3339();
    let messages:Vec<_>=[("3","Luis","third","Falta validar el pipeline."),("2","Self",graph.config.graph.user_id.as_str(),"Ana, revisé el pipeline del Project."),("1","Ana","second","Revisa el pipeline del Project, por favor.")].into_iter().map(|(id,name,sender,body)|json!({"id":id,"messageType":"message","createdDateTime":date,"deletedDateTime":null,"from":{"user":{"id":sender,"displayName":name}},"body":{"contentType":"text","content":body}})).collect();
    Mock::given(method("GET"))
        .and(path("/chats/chat1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":messages})))
        .expect(1)
        .mount(&server)
        .await;
    let context = graph
        .recent_project_context(
            &["Project".into()],
            "chats/chat1",
            chrono::Utc::now() - chrono::Duration::days(7),
        )
        .await
        .unwrap();
    assert_eq!(context.len(), 3);
    assert_eq!(context[0].name.as_deref(), Some("Ana"));
    assert!(context[0].text.contains("Revisa"));
    assert!(!context[0].mine);
    assert!(context[1].mine);
    assert_eq!(context[1].name.as_deref(), Some("Self"));
    assert_eq!(context[2].name.as_deref(), Some("Luis"));
    assert!(context[2].text.contains("Falta validar"));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().contains("members"))
    );
}

struct TeamsEvidenceTool;
#[async_trait]
impl ReadOnlyTool for TeamsEvidenceTool {
    async fn execute(&self, _: &ToolSpec, _: &str, _: &str) -> Result<String> {
        use personal_teams_assistant::evidence::{Evidence, TeamsMessage};
        let messages = [
            ("a", "Ana", false, "Pidió revisar el pipeline"),
            (
                "self",
                "Self",
                true,
                "Respondí que revisaría la configuración",
            ),
            ("l", "Luis", false, "Indicó que faltaba validar"),
        ]
        .into_iter()
        .map(|(id, name, mine, text)| TeamsMessage {
            conversation: "chat".into(),
            message: id.into(),
            sender: id.into(),
            name: Some(name.into()),
            mine,
            date: "2026-09-30T10:00:00Z".into(),
            text: text.into(),
        })
        .collect();
        Ok(serde_json::to_string(&Evidence {
            text: "Coordinación registrada; no prueba ejecución.".into(),
            teams: messages,
            ..Default::default()
        })?)
    }
}
struct TeamsAttributionLlm(bool);
#[async_trait]
impl LlmProvider for TeamsAttributionLlm {
    fn name(&self) -> &str {
        "synthetic-teams"
    }
    async fn generate(&self, input: GenerationInput<'_>) -> Result<String> {
        assert!(input.evidence.contains("\"name\":\"Ana\""));
        assert!(input.evidence.contains("\"name\":\"Luis\""));
        assert!(input.evidence.contains("\"mine\":true"));
        Ok(if self.0 {
            "Luis pidió revisar y Ana indicó que faltaba validar."
        } else {
            "Ana pidió revisar y Luis indicó que faltaba validar."
        }
        .into())
    }
}
#[tokio::test]
async fn teams_simulation_preserves_attribution_and_final_gate_rejects_swapped_authors() {
    for swapped in [false, true] {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = support::store(&dir);
        let graph = support::graph(&server, store.clone());
        Mock::given(method("POST")).and(path("/jev")).respond_with(move |req:&wiremock::Request| {
            let input=req.body_json::<serde_json::Value>().unwrap();
            assert!(input["state"]["teams_messages"].as_array().unwrap().iter().any(|m|m["name"]=="Ana" && m["text"]=="Pidió revisar el pipeline"));
            let answers:serde_json::Map<String,serde_json::Value>=["references","attribution","supported","privacy","relevant","no_new_promise"].into_iter().map(|name|(name.into(),json!({"type":"noul","noul":if name=="attribution" && swapped {0.01} else {0.99}}))).collect();
            ResponseTemplate::new(200).set_body_json(json!({"answers":answers}))
        }).expect(1).mount(&server).await;
        let mut cfg = (*graph.config).clone();
        cfg.policy.dry_run = true;
        let mut resource = wiki_resource();
        resource.id = "azure-devops-status".into();
        resource.access = Access::Tool {
            tool: ToolSpec::AzureDevopsStatus {
                repository: "private".into(),
                path: "catalog.toml".into(),
                secret_ref: "secret://ado/read".into(),
            },
        };
        let pipeline = Pipeline {
            config: Arc::new(cfg),
            store: store.clone(),
            adapter: graph,
            gate: Arc::new(Jev {
                client: reqwest::Client::new(),
                endpoint: format!("{}/jev", server.uri()),
                api_key: "synthetic".into(),
                model: "synthetic".into(),
            }),
            knowledge: KnowledgeMap {
                repositories: BTreeMap::new(),
                resources: vec![resource],
            },
            llm: Arc::new(TeamsAttributionLlm(swapped)),
            tools: Arc::new(TeamsEvidenceTool),
            redactor: redactor(),
        };
        let sources = vec!["azure-devops-status".into()];
        let result = personal_teams_assistant::simulation::run(
            &pipeline,
            personal_teams_assistant::simulation::SimulationRequest {
                session: "team-attribution".into(),
                text: "¿Qué coordinación hubo sobre el pipeline?".into(),
                group: false,
                mentioned: false,
                sources: sources.clone(),
            },
            Some(&sources),
        )
        .await
        .unwrap();
        assert_eq!(result.status, if swapped { "ignored" } else { "dry_run" });
        if swapped {
            assert_eq!(result.reason, "final_gate");
        }
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.url.path() == "/jev")
        );
    }
}
