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
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

/// Deterministic notice sent once when the model is still working after `HOLDING_AFTER`.
pub const HOLDING_REPLY: &str = "Déjame revisarlo.";
const HOLDING_AFTER: std::time::Duration = std::time::Duration::from_secs(300);

/// Model calls have no deadline; this tracks when to tell the sender the answer is coming.
struct Holding {
    enabled: bool,
    sent: bool,
    deadline: tokio::time::Instant,
}

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
        // A holding notice from an earlier attempt commits the assistant to answering.
        let holding_reply = self
            .store
            .audit(resource)?
            .and_then(|previous| previous.holding_reply);
        let mut holding = Holding {
            enabled: !self.config.policy.dry_run && !resource.starts_with("simulation:"),
            sent: holding_reply.is_some(),
            deadline: tokio::time::Instant::now() + HOLDING_AFTER,
        };
        let started = chrono::Utc::now().timestamp();
        let mut audit = Audit {
            status: "ignored".into(),
            reason: "ineligible_message".into(),
            holding_reply,
            ..Default::default()
        };
        if !message.eligible_in(
            &self.config.graph.user_id,
            &self.config.policy.allowed_senders,
            if holding.sent {
                i64::MAX
            } else {
                self.config.policy.max_message_age_seconds
            },
            self.config.graph.self_chat.as_ref(),
        ) {
            return self.store.record(resource, &audit);
        }
        let current = self.redactor.redact(&message.text);
        if !self.redactor.clean(&current) {
            audit.reason = "sensitive_question".into();
            return self.store.record(resource, &audit);
        }
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
        let is_greeting = if greeting(&current) {
            true
        } else if question_request(&current) {
            false
        } else {
            if available.is_empty() {
                audit.reason = "no_authorized_resource".into();
                return self.store.record(resource, &audit);
            }
            let intent = self
                .gate
                .evaluate(Stage::Intent, json!({"message":current}))
                .await?;
            audit.confidences.push(intent.confidence);
            if !intent.allows(0.5) || !matches!(intent.selected.as_str(), "question" | "greeting") {
                audit.reason = "informational_message".into();
                return self.store.record(resource, &audit);
            }
            intent.selected == "greeting"
        };
        let mut context_question = None;
        let mut context_answer = None;
        let proposal = if is_greeting {
            audit.reason = "deterministic_greeting".into();
            self.config.policy.greeting.clone()
        } else {
            let previous = self.store.context(&message.conversation)?;
            let more_details = matches!(
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
            );
            let mut tool_question = match &previous {
                Some(previous) if more_details => previous.question.clone(),
                _ => current.clone(),
            };
            // History helps interpret the request; a classifier cannot veto a new topic.
            let question = if let Some(previous) = &previous {
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
            if available.is_empty() {
                audit.reason = "no_authorized_resource".into();
                return self.store.record(resource, &audit);
            }
            // Read every authorized document: choosing one descriptor loses compound questions.
            // Read tools are selected from this authorized set; Jev does not veto retrieval.
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
            // A follow-up often omits its subject («¿y si quiero pagar 2 servicios?»): searching
            // those words alone misses the page. Resolve it against the previous exchange first;
            // the rewrite only shapes the query inside sources already authorized by code.
            let mut wiki_topic = None;
            if let Some(previous) = previous.as_ref().filter(|_| !more_details)
                && !tool_candidates.is_empty()
            {
                match self
                    .awaiting_model(
                        &mut holding,
                        resource,
                        &message,
                        &mut audit,
                        self.llm
                            .standalone_request(&previous.question, &previous.answer, &current),
                    )
                    .await?
                {
                    Ok(Some(resolved)) => {
                        let question = resolved.question.trim();
                        let topic = resolved.topic.trim();
                        if [(question, 500), (topic, 120)].iter().all(|(text, max)| {
                            !text.is_empty()
                                && text.chars().count() <= *max
                                && !text.chars().any(char::is_control)
                                && self.redactor.clean(text)
                        }) {
                            tool_question = question.to_owned();
                            wiki_topic = Some(topic.to_owned());
                        }
                    }
                    Ok(None) => {}
                    Err(_) => tracing::warn!(event = "follow_up_resolution_unavailable"),
                }
            }
            // Chained follow-ups keep the resolved topic instead of the bare follow-up.
            context_question = Some(tool_question.chars().take(1800).collect::<String>());
            let question = if wiki_topic.is_some() {
                format!(
                    "{question}\nSolicitud actual interpretada con el contexto: {tool_question}"
                )
            } else {
                question
            };
            let wiki_requested =
                explicit_wiki(&tool_question) || documentation_question(&tool_question);
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
                && status_question(&tool_question)
                && tool_candidates.contains_key("azure-devops-status")
            {
                Some("azure-devops-status".to_owned())
            } else if wiki_ids.len() == 1
                && available.iter().all(|r| {
                    !matches!(r.access, Access::Tool { .. })
                        || matches!(
                            r.access,
                            Access::Tool {
                                tool: crate::tools::ToolSpec::AzureDevopsWiki { .. }
                                    | crate::tools::ToolSpec::AzureDevopsStatus { .. }
                            }
                        )
                })
            {
                // A bounded Wiki read retrieves facts; it does not need a model to assert
                // that an unseen page already answers the question. Final checks still apply.
                Some(wiki_ids[0].clone())
            } else if tool_candidates.is_empty() {
                None
            } else if tool_candidates.len() == 1 {
                tool_candidates.keys().next().cloned()
            } else {
                match self
                    .awaiting_model(
                        &mut holding,
                        resource,
                        &message,
                        &mut audit,
                        self.llm.select_tool(&tool_question, &tool_candidates),
                    )
                    .await?
                {
                    Ok(Some(id)) if tool_candidates.contains_key(&id) => Some(id),
                    Ok(_) => None,
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
                            // Wiki search needs the topic; other tools read the whole request.
                            question: match (tool, &wiki_topic) {
                                (crate::tools::ToolSpec::AzureDevopsWiki { .. }, Some(topic)) => {
                                    topic.clone()
                                }
                                _ => tool_question.clone(),
                            },
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
                        Some(result.evidence(&tool_question, budget.saturating_sub(200)))
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
                .awaiting_model(
                    &mut holding,
                    resource,
                    &message,
                    &mut audit,
                    self.llm.generate_response(GenerationInput {
                        question: &question,
                        evidence: &evidence,
                        detail_requested: false,
                    }),
                )
                .await??;
            context_answer = Some(generated.answer.clone());
            audit.partial = registry.partial;
            audit.coverage_warnings = registry.warnings.clone();
            audit.references = registry.references.clone();
            audit.teams_messages = registry.teams.clone();
            audit.detailed = Some(generated.detailed);
            if !self.valid_answer(&generated.answer) {
                audit.proposed = Some(self.redactor.redact(&generated.answer));
                audit.reason = "unsafe_proposal".into();
                return self.store.record(resource, &audit);
            }
            let used_sources = if registry.references.is_empty() {
                Vec::new()
            } else {
                self.checkpoint(resource, &audit, "selecting_references")?;
                match self
                    .gate
                    .select_references(
                        &generated.answer,
                        &evidence,
                        &registry.references,
                        &generated.used_sources,
                    )
                    .await
                {
                    Ok(ids) => ids,
                    Err(_) => {
                        audit.reason = "reference_selection_failed".into();
                        return self.store.record(resource, &audit);
                    }
                }
            };
            audit.used_sources = used_sources.clone();
            let answer = if !registry.references.is_empty() {
                match crate::evidence::complete_answer(&generated.answer, &used_sources, &registry)
                {
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
            if !self.valid_answer(&answer) {
                audit.reason = "unsafe_proposal".into();
                return self.store.record(resource, &audit);
            }
            self.checkpoint(resource, &audit, "final_gate")?;
            let final_verdict = self
                .gate
                .evaluate(
                    Stage::Final,
                    json!({"question":question,"evidence":evidence,"answer":answer,"used_sources":used_sources,"references":registry.references,"teams_messages":registry.teams,"partial":registry.partial,"coverage_warnings":registry.warnings}))
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
            self.store.record(resource, &audit)?;
            if resource.starts_with("simulation:")
                && let Some(question) = context_question
            {
                // The body without appended sources: copied citation URLs would fail verification.
                self.store.save_context(
                    &message.conversation,
                    &self.redactor.redact(&question),
                    &self
                        .redactor
                        .redact(context_answer.as_deref().unwrap_or(&proposal)),
                )?;
            }
            return Ok(());
        }
        // Re-read immediately before sending to catch edits/deletion and stale questions.
        let latest = self.adapter.fetch(resource).await?;
        // Age is judged when processing started: a slow answer is not a stale question.
        let max_age = if holding.sent {
            i64::MAX
        } else {
            self.config.policy.max_message_age_seconds + (chrono::Utc::now().timestamp() - started)
        };
        if latest.text != message.text
            || !latest.eligible_in(
                &self.config.graph.user_id,
                &self.config.policy.allowed_senders,
                max_age,
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
                &self.redactor.redact(
                    context_answer
                        .as_deref()
                        .or(audit.sent.as_deref())
                        .unwrap_or(""),
                ),
            )?;
        }
        Ok(())
    }
    /// Await a model call without a deadline. If it is still running when the holding
    /// deadline passes, send the deterministic notice once; the intent is recorded first so
    /// a retried job never sends it again, and a failed send is never retried.
    async fn awaiting_model<T>(
        &self,
        holding: &mut Holding,
        resource: &str,
        message: &crate::adapters::teams::IncomingMessage,
        audit: &mut Audit,
        work: impl std::future::Future<Output = T>,
    ) -> Result<T> {
        tokio::pin!(work);
        if !holding.enabled || holding.sent {
            return Ok(work.await);
        }
        tokio::select! {
            biased;
            output = &mut work => return Ok(output),
            _ = tokio::time::sleep_until(holding.deadline) => {}
        }
        holding.sent = true;
        audit.holding_reply = Some("sending".into());
        self.checkpoint(resource, audit, "holding_reply")?;
        audit.holding_reply = Some(
            match self.adapter.send(message, HOLDING_REPLY).await {
                Ok(_) => "sent",
                Err(_) => "uncertain",
            }
            .into(),
        );
        self.checkpoint(resource, audit, "holding_reply")?;
        Ok(work.await)
    }
    fn checkpoint(&self, resource: &str, audit: &Audit, stage: &str) -> Result<()> {
        let mut saved = audit.clone();
        saved.status = "processing".into();
        saved.reason = stage.into();
        self.store.record(resource, &saved)
    }
    fn valid_answer(&self, answer: &str) -> bool {
        // No character cap; the rendered Teams HTML must still fit Graph's body limit.
        !answer.trim().is_empty()
            && crate::adapters::teams::html(answer).len() <= 27_800
            && self.redactor.clean(answer)
    }
}

fn explicit_wiki(question: &str) -> bool {
    question
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case("wiki") || w.eq_ignore_ascii_case("wikis"))
}
fn question_request(question: &str) -> bool {
    if question.contains(['?', '¿']) {
        return true;
    }
    let normalized = question
        .to_lowercase()
        .replace('ó', "o")
        .replace('é', "e")
        .replace('í', "i")
        .replace('á', "a")
        .replace('ú', "u");
    let normalized = normalized.trim_start_matches(|c: char| !c.is_alphanumeric());
    [
        "como ",
        "que ",
        "cual ",
        "cuales ",
        "cuando ",
        "donde ",
        "por que ",
        "para que ",
        "quien ",
        "cuanto ",
        "explica ",
        "explicame ",
        "dime ",
        "dame ",
        "muestra ",
        "necesito ",
        "busca ",
        "resume ",
        "detalla ",
        "amplia ",
        "continua ",
        "mas detalles",
        "how ",
        "what ",
        "where ",
        "when ",
        "why ",
        "who ",
        "explain ",
        "tell me ",
    ]
    .iter()
    .any(|prefix| normalized.starts_with(prefix))
}
fn documentation_question(question: &str) -> bool {
    let normalized = question
        .to_lowercase()
        .replace('ó', "o")
        .replace('é', "e")
        .replace('í', "i")
        .replace('á', "a")
        .replace('ú', "u")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    // These requests ask for existing instructions, not execution or current activity.
    [
        "como se usa ",
        "como usar ",
        "como se utiliza ",
        "como utilizar ",
        "como funciona ",
        "como se configura ",
        "como configurar ",
        "como se instala ",
        "como instalar ",
        "como se invoca ",
        "como invocar ",
        "como se llama a ",
        "como llamar a ",
        "como se consume ",
        "como consumir ",
        "como se integra ",
        "como integrar ",
        "que hace ",
        "que es ",
        "que parametros ",
        "que requisitos ",
        "para que sirve ",
        "explica ",
        "explicame ",
        "how to use ",
        "how to configure ",
    ]
    .iter()
    .any(|p| normalized.starts_with(p))
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
    use super::{documentation_question, question_request, status_question};

    #[test]
    fn clear_information_requests_do_not_need_a_model_to_allow_retrieval() {
        for q in [
            "como funciona la notificacion de pagos",
            "Explícame la integración",
            "¿Qué requisitos tiene?",
            "Necesito los parámetros",
            "dame más detalles",
            "Como se usa un componente desconocido",
        ] {
            assert!(question_request(q), "{q}");
        }
        for q in [
            "El despliegue terminó",
            "He añadido una Wiki",
            "Crea una HU mañana",
        ] {
            assert!(!question_request(q), "{q}");
        }
    }

    #[test]
    fn documentation_routes_read_requests_with_a_topic() {
        for question in [
            "como se usa el microservicio crearsps",
            "¿Cómo se invoca el microservicio Crear SPS para pagar 2 servicios?",
            "¿Cómo se configura el pipeline?",
            "Explícame la configuración del pipeline",
            "¿Qué parámetros necesita el pipeline?",
            "¿Qué hace este servicio?",
            "Cómo instalar el componente",
            "How to use this service?",
        ] {
            assert!(documentation_question(question), "{question}");
        }
        for question in [
            "Crea una HU para mañana",
            "Ejecuta el pipeline",
            "¿Qué he hecho esta semana?",
            "¿Cómo está el pipeline?",
            "¿Cómo se usa?",
            "Confirma que funciona y despliega el servicio",
        ] {
            assert!(!documentation_question(question), "{question}");
        }
    }

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
