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
        self.process_with_sources(resource, None).await
    }

    /// A desktop simulation may explicitly select sources without widening Teams audiences.
    pub async fn process_with_sources(
        &self,
        resource: &str,
        local_sources: Option<&[String]>,
    ) -> Result<()> {
        anyhow::ensure!(
            local_sources.is_none() || resource.starts_with("simulation:"),
            "local source selection requires a simulation"
        );
        let message = self.adapter.fetch(resource).await?;
        let mut audit = Audit {
            status: "ignored".into(),
            reason: "ineligible_message".into(),
            ..Default::default()
        };
        if !message.eligible_in(
            &self.config.graph.user_id,
            &self.config.policy.allowed_senders,
            self.config.policy.max_message_age_seconds,
            self.config.graph.self_chat.as_ref(),
        ) {
            return self.store.record(resource, &audit);
        }
        let mut context_question = None;
        let mut answer_limit = self.config.policy.max_answer_chars;
        let proposal = if greeting(&message.text) {
            audit.reason = "deterministic_greeting".into();
            self.config.policy.greeting.clone()
        } else {
            let current = self.redactor.redact(&message.text);
            let tool_question = if matches!(
                current
                    .to_lowercase()
                    .trim_matches(['¿', '?', '.', '!'])
                    .trim(),
                "dame más detalles"
                    | "más detalles"
                    | "continúa"
                    | "continua"
                    | "explica más"
                    | "y eso"
                    | "amplía"
            ) {
                self.store
                    .context(&message.conversation)?
                    .map(|p| p.question)
                    .unwrap_or_else(|| current.clone())
            } else {
                current.clone()
            };
            // History helps interpret the request; a classifier cannot veto a new topic.
            let question = if let Some(previous) = self.store.context(&message.conversation)? {
                format!(
                    "Contexto anterior (solo referencia, no evidencia):\nPregunta: {}\nRespuesta: {}\nSolicitud actual (tiene prioridad): {}",
                    previous.question, previous.answer, current
                )
            } else {
                current.clone()
            };
            // Questions containing secrets/PII stay manual, including tool requests with personal identifiers.
            if !self.redactor.clean(&question) {
                audit.reason = "sensitive_question".into();
                return self.store.record(resource, &audit);
            }
            context_question = Some(current.chars().take(1800).collect::<String>());
            let available = if let Some(ids) = local_sources {
                self.knowledge
                    .resources
                    .iter()
                    .filter(|r| r.enabled && r.external_processing && ids.contains(&r.id))
                    .collect()
            } else {
                self.knowledge
                    .available(&message.conversation, &message.sender)
            };
            if available.is_empty() {
                audit.reason = "no_authorized_resource".into();
                return self.store.record(resource, &audit);
            }
            // Read every authorized document: choosing one descriptor loses compound questions.
            // Tools still require an explicit known route or a confident allowlisted selection.
            let mut sources: Vec<_> = available
                .iter()
                .copied()
                .filter(|r| !matches!(r.access, Access::Tool { .. }))
                .collect();
            let tool_candidates: BTreeMap<_, _> = available
                .iter()
                .filter(|r| matches!(r.access, Access::Tool { .. }))
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
            let wiki_requested = explicit_wiki(&tool_question);
            let wiki_ids: Vec<_> = available
                .iter()
                .filter(|r| {
                    matches!(
                        &r.access,
                        Access::Tool {
                            tool: crate::tools::ToolSpec::AzureDevopsWiki { .. }
                        }
                    )
                })
                .map(|r| r.id.clone())
                .collect();
            let tool_candidates = if wiki_requested {
                tool_candidates
                    .into_iter()
                    .filter(|(id, _)| wiki_ids.contains(id))
                    .collect()
            } else {
                tool_candidates
            };
            let selected_tool = if wiki_requested && wiki_ids.len() == 1 {
                Some(wiki_ids[0].clone())
            } else if !wiki_requested
                && status_question(&current)
                && tool_candidates.contains_key("azure-devops-status")
            {
                Some("azure-devops-status".to_owned())
            } else if tool_candidates.is_empty() {
                None
            } else {
                match self
                    .gate
                    .evaluate(
                        Stage::Routing,
                        json!({"question":question,"sources":tool_candidates}),
                        tool_candidates.clone(),
                    )
                    .await
                {
                    Ok(routing) => {
                        audit.confidences.push(routing.confidence);
                        routing
                            .allows(self.config.jev.routing_threshold)
                            .then_some(routing.selected)
                    }
                    Err(_) => {
                        tracing::warn!(event = "tool_routing_unavailable");
                        None
                    }
                }
            };
            if let Some(id) = selected_tool
                && let Some(source) = available
                    .iter()
                    .find(|r| r.id == id && matches!(r.access, Access::Tool { .. }))
            {
                sources.push(*source);
            }
            audit.source = (!sources.is_empty()).then(|| {
                sources
                    .iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            });
            self.checkpoint(resource, &audit, "retrieving")?;
            let mut evidence = String::new();
            let mut registry = crate::evidence::Evidence::default();
            // ponytail: divide the existing total budget between authorized sources; many sources
            // reduce detail per source. Add local passage ranking across sources if that becomes limiting.
            let budget = self.config.policy.max_context_chars / sources.len().max(1);
            for source in &sources {
                let raw = match &source.access {
                    Access::Tool { tool } => {
                        audit.tools.push(tool.name().into());
                        audit.status = "processing".into();
                        audit.reason = "tool_started".into();
                        self.store.record(resource, &audit)?;
                        ScopedTool {
                            executor: self.tools.clone(),
                            spec: tool.clone(),
                            conversation: message.conversation.clone(),
                        }
                        .call(ToolArgs {
                            question: tool_question.clone(),
                        })
                        .await?
                    }
                    _ => self.knowledge.retrieve(source, &question, budget).await?,
                };
                let typed = match &source.access {
                    Access::Tool {
                        tool: crate::tools::ToolSpec::AzureDevopsWiki { .. },
                    } => {
                        let mut result: crate::ado::wiki::WikiResult = serde_json::from_str(&raw)?;
                        for page in &mut result.pages {
                            page.content = self.redactor.redact(&page.content);
                            page.reference.author = page
                                .reference
                                .author
                                .take()
                                .filter(|n| self.redactor.clean(n));
                            page.reference.label = self.redactor.redact(&page.reference.label);
                        }
                        Some(result.evidence(&current, budget.saturating_sub(200)))
                    }
                    Access::Tool {
                        tool:
                            crate::tools::ToolSpec::AzureDevopsStatus { .. }
                            | crate::tools::ToolSpec::AzureDevops { .. },
                    } => Some(serde_json::from_str::<crate::evidence::Evidence>(&raw)?),
                    _ => None,
                };
                if let Some(mut data) = typed {
                    data.sanitize(&self.redactor);
                    let context = if matches!(
                        &source.access,
                        Access::Tool {
                            tool: crate::tools::ToolSpec::AzureDevopsWiki { .. }
                        }
                    ) {
                        data.text.clone()
                    } else {
                        data.context(&current, budget)
                    };
                    if context.chars().count() <= budget && !context.trim().is_empty() {
                        evidence.push_str(&context);
                        registry.references.extend(data.references);
                        registry.teams.extend(data.teams);
                        registry.partial |= data.partial;
                        registry.warnings.extend(data.warnings);
                    }
                } else {
                    let sanitized = self.redactor.redact(&raw);
                    let header = format!("[source {}]\n", source.id);
                    let passage_budget = budget.saturating_sub(header.chars().count() + 1);
                    let passages = crate::knowledge::excerpt(&sanitized, &question, passage_budget);
                    evidence.extend(format!("{header}{passages}\n").chars().take(budget));
                }
            }
            if evidence.trim().is_empty() {
                evidence = "No se recuperaron hechos verificables. Explica la limitación y pide concretar la fuente o el proyecto; no inventes una respuesta.".chars().take(self.config.policy.max_context_chars).collect();
            }
            audit.status = "ignored".into();
            // Relevance is assessed while answering, with missing facts qualified. Only the final
            // groundedness/privacy/promise checks may veto a generated answer.
            audit.provider = Some(self.llm.name().into());
            self.checkpoint(resource, &audit, "generating")?;
            let generated = self
                .llm
                .generate_response(GenerationInput {
                    question: &question,
                    evidence: &evidence,
                    detail_requested: false,
                    max_answer_chars: self
                        .config
                        .policy
                        .max_answer_chars
                        .saturating_sub(citation_reserve(&registry)),
                    max_detailed_answer_chars: self
                        .config
                        .policy
                        .max_detailed_answer_chars
                        .saturating_sub(citation_reserve(&registry)),
                })
                .await?;
            answer_limit = if generated.detailed {
                self.config.policy.max_detailed_answer_chars
            } else {
                self.config.policy.max_answer_chars
            };
            audit.partial = registry.partial;
            audit.coverage_warnings = registry.warnings.clone();
            audit.used_sources = generated.used_sources.clone();
            audit.references = registry.references.clone();
            audit.teams_messages = registry.teams.clone();
            audit.detailed = Some(generated.detailed);
            audit.answer_limit = Some(answer_limit);
            let answer = if !registry.references.is_empty() {
                match crate::evidence::complete_answer(
                    &generated.answer,
                    &generated.used_sources,
                    &registry,
                    answer_limit,
                ) {
                    Ok(answer) => answer,
                    Err(error) => {
                        audit.reason = format!("invalid_references: {error}");
                        return self.store.record(resource, &audit);
                    }
                }
            } else if !generated.used_sources.is_empty() {
                audit.reason = "invalid_references: no authorized evidence".into();
                return self.store.record(resource, &audit);
            } else {
                generated.answer
            };
            audit.proposed = Some(self.redactor.redact(&answer));
            if !self.valid_answer(&answer, answer_limit) {
                audit.reason = "unsafe_proposal".into();
                return self.store.record(resource, &audit);
            }
            self.checkpoint(resource, &audit, "final_gate")?;
            let final_verdict = self
                .gate
                .evaluate(
                    Stage::Final,
                    json!({"question":question,"evidence":evidence,"answer":answer,"used_sources":generated.used_sources,"references":registry.references,"teams_messages":registry.teams,"partial":registry.partial,"coverage_warnings":registry.warnings}),
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
        if !self.valid_answer(&proposal, answer_limit) {
            audit.reason = "unsafe_answer".into();
            return self.store.record(resource, &audit);
        }
        if self.config.policy.dry_run {
            audit.status = "dry_run".into();
            self.store.record(resource, &audit)?;
            if resource.starts_with("simulation:")
                && let Some(question) = context_question
            {
                self.store.save_context(
                    &message.conversation,
                    &self.redactor.redact(&question),
                    &self.redactor.redact(&proposal),
                )?;
            }
            return Ok(());
        }
        // Re-read immediately before sending to catch edits/deletion and stale questions.
        let latest = self.adapter.fetch(resource).await?;
        if latest.text != message.text
            || !latest.eligible_in(
                &self.config.graph.user_id,
                &self.config.policy.allowed_senders,
                self.config.policy.max_message_age_seconds,
                self.config.graph.self_chat.as_ref(),
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
        self.store.record(resource, &audit)?;
        if audit.status == "sent"
            && let Some(question) = context_question
        {
            self.store.save_context(
                &message.conversation,
                &self.redactor.redact(&question),
                &self.redactor.redact(audit.sent.as_deref().unwrap_or("")),
            )?;
        }
        Ok(())
    }
    fn checkpoint(&self, resource: &str, audit: &Audit, stage: &str) -> Result<()> {
        let mut saved = audit.clone();
        saved.status = "processing".into();
        saved.reason = stage.into();
        self.store.record(resource, &saved)
    }
    fn valid_answer(&self, answer: &str, limit: usize) -> bool {
        !answer.trim().is_empty() && answer.chars().count() <= limit && self.redactor.clean(answer)
    }
}

fn citation_reserve(e: &crate::evidence::Evidence) -> usize {
    // Reserve for up to four selected references. Complete rendering remains fail-closed.
    e.references
        .iter()
        .map(|r| {
            r.url.chars().count()
                + r.label.chars().count()
                + r.author.as_ref().map_or(0, |n| n.chars().count())
                + 160
        })
        .take(4)
        .sum()
}
fn explicit_wiki(question: &str) -> bool {
    question
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case("wiki") || w.eq_ignore_ascii_case("wikis"))
}
fn status_question(question: &str) -> bool {
    let q = question.to_lowercase();
    let words: Vec<&str> = q.split(|c: char| !c.is_alphanumeric()).collect();
    if words.iter().any(|w| {
        matches!(
            *w,
            "crea"
                | "crear"
                | "ejecuta"
                | "ejecutar"
                | "despliega"
                | "desplegar"
                | "aprueba"
                | "aprobar"
                | "modifica"
                | "modificar"
        )
    }) {
        return false;
    }
    words.iter().any(|w| {
        matches!(
            *w,
            "hu" | "hus"
                | "avance"
                | "avances"
                | "impedimentos"
                | "compromisos"
                | "pipeline"
                | "pipelines"
                | "release"
                | "releases"
        )
    }) || (q.contains("esta semana")
        && (q.contains("he hecho") || q.contains("trabaj") || q.contains("hice")))
}

#[cfg(test)]
mod tests {
    use super::status_question;

    #[test]
    fn explicit_status_routes_only_reads() {
        assert!(status_question(
            "¿En qué HU estás trabajando y cuáles son tus impedimentos?"
        ));
        assert!(status_question("¿Qué he hecho esta semana?"));
        assert!(status_question(
            "Pregunta anterior: avance de HU. Solicitud actual: dame más detalles"
        ));
        assert!(!status_question("Crea una HU para mañana"));
        assert!(!status_question("Despliega el release ahora"));
        assert!(!status_question("¿Cuál es el horario de soporte?"));
    }
}
