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
    pub resource: String,
    pub identified_question: Option<crate::questions::IdentifiedQuestion>,
    pub available_sources: Vec<String>,
    pub selected_sources: Vec<String>,
    pub status: String,
    pub reason: String,
    pub answer: Option<String>,
    pub used_sources: Vec<String>,
    pub references: Vec<crate::evidence::Reference>,
    pub partial: bool,
    pub warnings: Vec<String>,
}

/// A simulation that cannot run as asked; the message says what is missing.
#[derive(Debug)]
pub struct InvalidRequest(pub &'static str);
impl std::fmt::Display for InvalidRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidRequest {}

/// Who writes in a simulated conversation.
pub const SENDER: &str = "simulated-user";
/// The conversation a simulation session runs in.
pub fn conversation(session: &str) -> String {
    format!("chats/simulation-{session}")
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
        InvalidRequest(
            "A simulation needs a session (at most 64 letters, digits, - or _) and a text of at most 2000 characters."
        )
    );
    Ok(())
}

/// The sources a local simulation may read. Named sources must be enabled and allow external
/// processing. Without any, a map with questions offers every such source and the identified
/// question narrows them to its own; a map without questions reads those authorized for the
/// simulated conversation. `None` means the conversation's authorization.
pub fn sources(
    knowledge: &crate::knowledge::KnowledgeMap,
    input: &SimulationRequest,
) -> Result<Option<Vec<String>>> {
    let readable: Vec<String> = knowledge
        .resources
        .iter()
        .filter(|r| r.enabled && r.external_processing)
        .map(|r| r.id.clone())
        .collect();
    if !input.sources.is_empty() {
        ensure!(
            input.sources.iter().all(|id| readable.contains(id)),
            InvalidRequest(
                "A selected source is unknown, disabled or does not allow external processing; see pta sources list."
            )
        );
        return Ok(Some(input.sources.clone()));
    }
    ensure!(
        !readable.is_empty(),
        InvalidRequest(
            "No source can be read in a simulation: enable a source that allows external processing, or name the sources to read in \"sources\"."
        )
    );
    if knowledge.questions.is_some() {
        return Ok(Some(readable));
    }
    ensure!(
        !knowledge
            .available(&conversation(&input.session), SENDER)
            .is_empty(),
        InvalidRequest(
            "No source is authorized for the simulated conversation and the map has no questions: name the sources to read in \"sources\" (enabled, with external processing)."
        )
    );
    Ok(None)
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
        conversation: conversation(&input.session),
        sender: SENDER.into(),
        sender_name: String::new(),
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
        created_at_millis: chrono::Utc::now().timestamp_millis(),
        is_user_message: true,
    };
    base.store.begin_simulation(&resource)?;
    let pipeline = Pipeline {
        config: base.config.clone(),
        store: base.store.clone(),
        adapter: Arc::new(TestAdapter(message)),
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
        resource,
        identified_question: audit.identified_question,
        available_sources: audit.available_sources,
        selected_sources: audit.selected_sources,
        status: audit.status,
        reason: audit.reason,
        answer,
        used_sources: audit.used_sources,
        references: audit.references,
        partial: audit.partial,
        warnings: audit.coverage_warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(questions: bool, audience: &str) -> crate::knowledge::KnowledgeMap {
        let questions = if questions {
            "[[questions]]\nid = \"payment-states\"\ntext = \"What do states 6 and 10 mean?\"\nsources = [\"wiki\"]\n"
        } else {
            ""
        };
        crate::knowledge::KnowledgeMap::parse(&format!(
            r#"
[[resources]]
id = "wiki"
description = "Synthetic Wiki"
topics = []
enabled = true
external_processing = true
allowed_conversations = [{audience}]
allowed_senders = []
kind = "url"
url = "https://example.com/wiki"

[[resources]]
id = "off"
description = "Disabled"
topics = []
enabled = false
external_processing = true
allowed_conversations = ["*"]
allowed_senders = []
kind = "url"
url = "https://example.com/off"
{questions}"#
        ))
        .unwrap()
    }
    fn request(sources: &[&str]) -> SimulationRequest {
        SimulationRequest {
            session: "t".into(),
            text: "What is state 10?".into(),
            group: false,
            mentioned: false,
            sources: sources.iter().map(|s| (*s).to_owned()).collect(),
        }
    }
    fn refusal(result: Result<Option<Vec<String>>>) -> String {
        result
            .unwrap_err()
            .downcast::<InvalidRequest>()
            .expect("a typed invalid request")
            .to_string()
    }

    #[test]
    fn a_simulation_without_sources_uses_the_question_or_the_conversation_authorization() {
        // With questions, every readable source; the identified question keeps its own.
        assert_eq!(
            sources(&map(true, ""), &request(&[])).unwrap(),
            Some(vec!["wiki".to_owned()])
        );
        // Without questions, the sources authorized for the simulated conversation.
        assert_eq!(sources(&map(false, "\"*\""), &request(&[])).unwrap(), None);
        assert!(refusal(sources(&map(false, ""), &request(&[]))).contains("name the sources"));
        assert_eq!(
            sources(&map(false, ""), &request(&["wiki"])).unwrap(),
            Some(vec!["wiki".to_owned()])
        );
        for named in ["off", "unknown"] {
            assert!(
                refusal(sources(&map(true, ""), &request(&[named]))).contains("pta sources list")
            );
        }
        let mut none = map(true, "");
        none.resources.retain(|r| !r.enabled);
        assert!(refusal(sources(&none, &request(&[]))).contains("enable a source"));
        let mut empty = request(&[]);
        empty.text = " ".into();
        assert!(
            validate_request(&empty)
                .unwrap_err()
                .downcast::<InvalidRequest>()
                .is_ok()
        );
    }
}
