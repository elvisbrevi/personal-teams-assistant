use crate::{
    adapters::{graph::Graph, oauth::AccessToken},
    config::Config,
    decision::Jev,
    knowledge::KnowledgeMap,
    llm::DeepSeek,
    pipeline::Pipeline,
    security::{self, Redactor},
    simulation::{self, SimulationRequest, SimulationResult},
    state::Store,
    tools::Tools,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use std::{path::Path, sync::Arc, time::Duration};

struct NoTeamsToken;

#[async_trait]
impl AccessToken for NoTeamsToken {
    async fn access_token(&self) -> Result<String> {
        anyhow::bail!("Teams account is not connected to local chat")
    }
}

/// Run the real decision and answer pipeline with a local-only adapter.
/// No Teams credential, public URL or Graph subscription is needed.
pub async fn chat(config_path: &Path, input: SimulationRequest) -> Result<SimulationResult> {
    simulation::validate_request(&input)?;
    ensure!(!input.sources.is_empty(), "select at least one source");
    let config = Config::load_desktop(
        config_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid path"))?,
    )?;
    let knowledge = KnowledgeMap::load(&config.knowledge_map)?;
    let repositories = knowledge.repositories.clone();
    ensure!(
        input.sources.iter().all(|id| knowledge
            .resources
            .iter()
            .any(|r| { r.id == *id && r.enabled && r.external_processing })),
        "selected source is unavailable"
    );
    let jev_key = security::secret("TYPESAFE_API_KEY")?;
    let llm_key = security::secret("DEEPSEEK_API_KEY")?;
    let mut exact_secrets = vec![jev_key.clone(), llm_key.clone()];
    for name in config.secrets.values() {
        exact_secrets.push(security::secret(name)?);
    }
    let redactor = Arc::new(Redactor::new(
        &config.policy.sensitive_patterns,
        exact_secrets,
    )?);
    security::private_dir(&config.server.data_dir)?;
    let store = Arc::new(Store::open(
        &config.server.data_dir.join("desktop-chat.db"),
    )?);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut config = config;
    config.policy.allowed_senders.clear(); // The local user selects sources explicitly.
    let config = Arc::new(config);
    let graph = Arc::new(Graph {
        client: client.clone(),
        token: Arc::new(NoTeamsToken),
        base_url: "https://graph.microsoft.com/v1.0".into(),
        config: config.clone(),
        store: store.clone(),
        client_state: String::new(),
    });
    let pipeline = Pipeline {
        config: config.clone(),
        store: store.clone(),
        adapter: Arc::new(NoLocalAdapter),
        gate: Arc::new(Jev {
            client: client.clone(),
            endpoint: "https://api.typesafe.ai/v1/systemone".into(),
            api_key: jev_key,
            model: config.jev.model.clone(),
        }),
        knowledge,
        llm: Arc::new(DeepSeek::new(
            &llm_key,
            &config.llm.model,
            &config.llm.style,
            "https://api.deepseek.com",
        )?),
        tools: Arc::new(Tools {
            bindings: config.secrets.clone(),
            repositories,
            client,
            store,
            graph,
        }),
        redactor,
    };
    let selected = input.sources.clone();
    simulation::run(&pipeline, input, Some(&selected)).await
}

struct NoLocalAdapter;

#[async_trait]
impl crate::adapters::MessageAdapter for NoLocalAdapter {
    async fn fetch(&self, _: &str) -> Result<crate::adapters::teams::IncomingMessage> {
        anyhow::bail!("local adapter is created per simulation")
    }
    async fn send(&self, _: &crate::adapters::teams::IncomingMessage, _: &str) -> Result<String> {
        anyhow::bail!("local chat cannot send to Teams")
    }
}
