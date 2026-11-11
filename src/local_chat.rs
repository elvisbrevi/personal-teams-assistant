use crate::{
    adapters::{graph::Graph, oauth::AccessToken},
    config::Config,
    knowledge::KnowledgeMap,
    pipeline::Pipeline,
    security::{self, Redactor},
    simulation::{self, SimulationRequest, SimulationResult},
    state::Store,
    tools::Tools,
};
use anyhow::Result;
use async_trait::async_trait;
use std::{path::Path, sync::Arc, time::Duration};

/// How a simulation reads the user's own Teams messages.
pub enum Teams {
    /// No source it may read needs Teams.
    Unused,
    /// The host's Microsoft session.
    Session(Arc<dyn AccessToken>),
    /// There is none, and why.
    Missing(&'static str),
}

/// An access token the host obtained for one simulation.
pub struct FixedToken(pub String);

#[async_trait]
impl AccessToken for FixedToken {
    async fn access_token(&self) -> Result<String> {
        Ok(self.0.clone())
    }
}

/// Why a simulation read no Teams message of the user's.
#[derive(Debug)]
pub struct NoTeamsSession(pub &'static str);
impl std::fmt::Display for NoTeamsSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for NoTeamsSession {}

struct NoTeamsToken(&'static str);

#[async_trait]
impl AccessToken for NoTeamsToken {
    async fn access_token(&self) -> Result<String> {
        Err(NoTeamsSession(self.0).into())
    }
}

/// Run the real decision and answer pipeline with a local-only adapter. No public URL or
/// Graph subscription is needed; `teams` is the session the user's own Teams messages are
/// read with, the only Graph read a simulation makes.
pub async fn chat(
    config_path: &Path,
    input: SimulationRequest,
    teams: Teams,
) -> Result<SimulationResult> {
    simulation::validate_request(&input)?;
    let config = Config::load(
        config_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid path"))?,
    )?;
    let knowledge = KnowledgeMap::load(&config.knowledge_map)?;
    let repositories = knowledge.repositories.clone();
    let selected = simulation::sources(&knowledge, &input)?;
    let (llm, llm_secrets) = crate::llm::from_config(&config.llm)?;
    let mut exact_secrets = llm_secrets;
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
        token: match teams {
            Teams::Session(token) => token,
            Teams::Unused => Arc::new(NoTeamsToken("no source of this simulation reads Teams")),
            Teams::Missing(reason) => Arc::new(NoTeamsToken(reason)),
        },
        base_url: "https://graph.microsoft.com/v1.0".into(),
        config: config.clone(),
        store: store.clone(),
        client_state: String::new(),
        config_path: None,
    });
    let pipeline = Pipeline {
        config: config.clone(),
        store: store.clone(),
        adapter: Arc::new(NoLocalAdapter),
        knowledge,
        llm: Arc::new(llm),
        tools: Arc::new(Tools {
            bindings: config.secrets.clone(),
            repositories,
            client,
            store,
            graph,
        }),
        redactor,
    };
    simulation::run(&pipeline, input, selected.as_deref()).await
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
