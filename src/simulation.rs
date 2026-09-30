use crate::{
    adapters::{MessageAdapter, teams},
    pipeline::Pipeline,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Deserialize, Serialize)]
pub struct SimulationRequest {
    pub session: String,
    pub text: String,
    #[serde(default)]
    pub group: bool,
    #[serde(default)]
    pub mentioned: bool,
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Serialize)]
pub struct SimulationResult {
    pub status: String,
    pub reason: String,
    pub answer: Option<String>,
}

pub fn validate_request(input: &SimulationRequest) -> Result<()> {
    ensure!(
        !input.session.is_empty()
            && input.session.len() <= 64
            && input
                .session
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
            && !input.text.trim().is_empty()
            && input.text.chars().count() <= 2000,
        "invalid simulation request"
    );
    Ok(())
}

struct TestAdapter(teams::IncomingMessage);

#[async_trait]
impl MessageAdapter for TestAdapter {
    async fn fetch(&self, _: &str) -> Result<teams::IncomingMessage> {
        Ok(self.0.clone())
    }
    async fn send(&self, _: &teams::IncomingMessage, _: &str) -> Result<String> {
        Ok("simulation-only".into())
    }
}

pub async fn run(
    base: &Pipeline,
    input: SimulationRequest,
    local_sources: Option<&[String]>,
) -> Result<SimulationResult> {
    validate_request(&input)?;
    let resource = format!("simulation:{}:{}", input.session, uuid::Uuid::new_v4());
    let message = teams::IncomingMessage {
        resource: resource.clone(),
        conversation: format!("chats/simulation-{}", input.session),
        sender: "simulated-user".into(),
        kind: if input.group {
            teams::ConversationKind::Group
        } else {
            teams::ConversationKind::Direct
        },
        mentions: if input.mentioned {
            vec![base.config.graph.user_id.clone()]
        } else {
            vec![]
        },
        text: input.text,
        created_at: chrono::Utc::now().timestamp(),
        is_user_message: true,
    };
    base.store.begin_simulation(&resource)?;
    let pipeline = Pipeline {
        config: base.config.clone(),
        store: base.store.clone(),
        adapter: Arc::new(TestAdapter(message)),
        gate: base.gate.clone(),
        knowledge: base.knowledge.clone(),
        llm: base.llm.clone(),
        tools: base.tools.clone(),
        redactor: base.redactor.clone(),
    };
    pipeline
        .process_with_sources(&resource, local_sources)
        .await?;
    let audit = base
        .store
        .audit(&resource)?
        .ok_or_else(|| anyhow::anyhow!("missing audit"))?;
    let answer = if audit.status == "sent" {
        audit.sent
    } else if audit.status == "dry_run" {
        audit.proposed
    } else {
        None
    };
    Ok(SimulationResult {
        status: audit.status,
        reason: audit.reason,
        answer,
    })
}
