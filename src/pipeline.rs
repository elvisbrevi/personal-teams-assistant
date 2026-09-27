use crate::{
    adapters::{MessageAdapter, teams::greeting},
    config::Config,
    decision::{DecisionGate, Stage},
    knowledge::{Access, KnowledgeMap},
    llm::{GenerationInput, LlmProvider},
    security::Redactor,
    state::{Audit, Store},
    tools::{ReadOnlyTool, ScopedTool, ToolArgs},
};
use anyhow::Result;
use rig::tool::Tool;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

pub struct Pipeline {
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub adapter: Arc<dyn MessageAdapter>,
    pub gate: Arc<dyn DecisionGate>,
    pub knowledge: KnowledgeMap,
    pub llm: Arc<dyn LlmProvider>,
    pub tools: Arc<dyn ReadOnlyTool>,
    pub redactor: Arc<Redactor>,
}
impl Pipeline {
    pub async fn process(&self, resource: &str) -> Result<()> {
        let message = self.adapter.fetch(resource).await?;
        let mut audit = Audit {
            status: "ignored".into(),
            reason: "ineligible_message".into(),
            ..Default::default()
        };
        if !message.eligible(
            &self.config.graph.user_id,
            &self.config.policy.allowed_senders,
            self.config.policy.max_message_age_seconds,
        ) {
            return self.store.record(resource, &audit);
        }
        let proposal = if greeting(&message.text) {
            audit.reason = "deterministic_greeting".into();
            self.config.policy.greeting.clone()
        } else {
            let question = self.redactor.redact(&message.text);
            // Questions containing secrets/PII stay manual, including tool requests with personal identifiers.
            if !self.redactor.clean(&question) {
                audit.reason = "sensitive_question".into();
                return self.store.record(resource, &audit);
            }
            let available = self
                .knowledge
                .available(&message.conversation, &message.sender);
            if available.is_empty() {
                audit.reason = "no_authorized_resource".into();
                return self.store.record(resource, &audit);
            }
            let candidates: BTreeMap<_, _> = available
                .iter()
                .map(|r| {
                    (
                        r.id.clone(),
                        self.redactor.redact(&format!(
                            "{}; topics: {}",
                            r.description,
                            r.topics.join(", ")
                        )),
                    )
                })
                .collect();
            let routing = self
                .gate
                .evaluate(
                    Stage::Routing,
                    json!({"question":question,"sources":candidates}),
                    candidates.clone(),
                )
                .await?;
            audit.confidences.push(routing.confidence);
            if !routing.allows(self.config.jev.routing_threshold) {
                audit.reason = "routing_gate".into();
                return self.store.record(resource, &audit);
            }
            let Some(source) = available.into_iter().find(|r| r.id == routing.selected) else {
                audit.reason = "unknown_source".into();
                return self.store.record(resource, &audit);
            };
            audit.source = Some(source.id.clone());
            self.checkpoint(resource, &audit, "retrieving")?;
            let raw = match &source.access {
                Access::Tool { tool } => {
                    audit.tools.push(tool.name().into());
                    // Persist invocation before execution; the audit retains this if the process crashes.
                    audit.status = "processing".into();
                    audit.reason = "tool_started".into();
                    self.store.record(resource, &audit)?;
                    ScopedTool {
                        executor: self.tools.clone(),
                        spec: tool.clone(),
                    }
                    .call(ToolArgs {
                        question: question.clone(),
                    })
                    .await?
                }
                _ => {
                    self.knowledge
                        .retrieve(source, &question, self.config.policy.max_context_chars)
                        .await?
                }
            };
            let evidence = crate::knowledge::excerpt(
                &self.redactor.redact(&raw),
                &question,
                self.config.policy.max_context_chars,
            );
            audit.status = "ignored".into();
            if evidence.trim().is_empty() {
                audit.reason = "empty_evidence".into();
                return self.store.record(resource, &audit);
            }
            self.checkpoint(resource, &audit, "evidence_gate")?;
            let evidence_verdict = self
                .gate
                .evaluate(
                    Stage::Evidence,
                    json!({"question":question,"evidence":evidence,"source":source.id}),
                    BTreeMap::new(),
                )
                .await?;
            audit.confidences.push(evidence_verdict.confidence);
            if evidence_verdict.selected != "allow"
                || !evidence_verdict.allows(self.config.jev.evidence_threshold)
            {
                audit.reason = "evidence_gate".into();
                return self.store.record(resource, &audit);
            }
            audit.provider = Some(self.llm.name().into());
            self.checkpoint(resource, &audit, "generating")?;
            let answer = self
                .llm
                .generate(GenerationInput {
                    question: &question,
                    evidence: &evidence,
                })
                .await?;
            audit.proposed = Some(self.redactor.redact(&answer));
            if !self.valid_answer(&answer) {
                audit.reason = "unsafe_proposal".into();
                return self.store.record(resource, &audit);
            }
            self.checkpoint(resource, &audit, "final_gate")?;
            let final_verdict = self
                .gate
                .evaluate(
                    Stage::Final,
                    json!({"question":question,"evidence":evidence,"answer":answer}),
                    BTreeMap::new(),
                )
                .await?;
            audit.confidences.push(final_verdict.confidence);
            if final_verdict.selected != "allow"
                || !final_verdict.allows(self.config.jev.final_threshold)
            {
                audit.reason = "final_gate".into();
                return self.store.record(resource, &audit);
            }
            audit.reason = "supported_answer".into();
            answer
        };
        audit.proposed = Some(self.redactor.redact(&proposal));
        if !self.valid_answer(&proposal) {
            audit.reason = "unsafe_answer".into();
            return self.store.record(resource, &audit);
        }
        if self.config.policy.dry_run {
            audit.status = "dry_run".into();
            return self.store.record(resource, &audit);
        }
        // Re-read immediately before sending to catch edits/deletion and stale questions.
        let latest = self.adapter.fetch(resource).await?;
        if latest.text != message.text
            || !latest.eligible(
                &self.config.graph.user_id,
                &self.config.policy.allowed_senders,
                self.config.policy.max_message_age_seconds,
            )
        {
            audit.reason = "message_changed".into();
            return self.store.record(resource, &audit);
        }
        audit.status = "sending".into();
        self.store.record(resource, &audit)?;
        match self.adapter.send(&message, &proposal).await {
            Ok(id) => {
                audit.status = "sent".into();
                audit.sent = Some(proposal);
                audit.graph_message_id = Some(id);
            }
            Err(_) => {
                audit.status = "uncertain".into();
                audit.reason = "send_result_unknown_manual_review".into();
            }
        }
        self.store.record(resource, &audit)
    }
    fn checkpoint(&self, resource: &str, audit: &Audit, stage: &str) -> Result<()> {
        let mut saved = audit.clone();
        saved.status = "processing".into();
        saved.reason = stage.into();
        self.store.record(resource, &saved)
    }
    fn valid_answer(&self, answer: &str) -> bool {
        !answer.trim().is_empty()
            && answer.chars().count() <= self.config.policy.max_answer_chars
            && self.redactor.clean(answer)
    }
}
