#![allow(dead_code)]
use anyhow::Result;
use async_trait::async_trait;
use personal_teams_assistant::{
    adapters::{graph::Graph, oauth::AccessToken},
    config::Config,
    state::Store,
};
use serde_json::json;
use std::sync::Arc;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};
pub struct Token;
#[async_trait]
impl AccessToken for Token {
    async fn access_token(&self) -> Result<String> {
        Ok("test-access-token".into())
    }
}
pub fn config() -> Config {
    let mut cfg: Config = toml::from_str(include_str!("../../config.example.toml")).unwrap();
    cfg.graph.allowed_chats = vec!["chat1".into()];
    cfg.policy.dry_run = false;
    cfg
}
pub fn store(dir: &tempfile::TempDir) -> Arc<Store> {
    Arc::new(Store::open(&dir.path().join("test.db")).unwrap())
}
pub fn graph(server: &MockServer, store: Arc<Store>) -> Arc<Graph> {
    Arc::new(Graph {
        client: reqwest::Client::new(),
        token: Arc::new(Token),
        base_url: server.uri(),
        config: Arc::new(config()),
        store,
        client_state: "test-webhook-shared-secret-32-chars".into(),
    })
}
pub async fn mock_message(server: &MockServer, text: &str) {
    Mock::given(method("GET"))
        .and(path("/chats/chat1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chatType":"oneOnOne"})))
        .mount(server)
        .await;
    Mock::given(method("GET")).and(path("/chats/chat1/messages/123")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"123","messageType":"message","createdDateTime":chrono::Utc::now().to_rfc3339(),"deletedDateTime":null,"from":{"user":{"id":"sender"}},"body":{"contentType":"text","content":text},"mentions":[]}))).mount(server).await;
}
pub fn choice(options: &[&str], selected: &str, confidence: f64) -> serde_json::Value {
    let probabilities: serde_json::Map<_, _> = options
        .iter()
        .map(|o| {
            (
                (*o).to_owned(),
                json!(if *o == selected { 1.0 } else { 0.0 }),
            )
        })
        .collect();
    json!({"type":"choice","choice":selected,"confidence":confidence,"probabilities":probabilities})
}
