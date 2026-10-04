use crate::{
    adapters::{MessageAdapter, teams::greeting},
    config::{Config, Language},
    knowledge::{Access, KnowledgeMap},
    llm::{GenerationInput, Intent, LlmProvider},
    security::Redactor,
    state::{Audit, Store},
    tools::{ReadOnlyTool, ScopedTool, ToolArgs},
};
use anyhow::Result;
use std::{collections::BTreeMap, sync::Arc};

/// Deterministic notice sent once when the model is still working after `HOLDING_AFTER`.
pub fn holding_reply(language: Language) -> &'static str {
    match language {
        Language::Es => "Déjame revisarlo.",
        Language::En => "Let me look into it.",
    }
}
const HOLDING_AFTER: std::time::Duration = std::time::Duration::from_secs(300);
/// Earlier messages of the conversation given to the model to interpret the current one.
pub const HISTORY_MESSAGES: usize = 10;
const HISTORY_CHARS: usize = 6000;

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
        // Notices from an earlier attempt are never sent again.
        let earlier = self.store.audit(resource)?;
        let holding_reply = earlier.as_ref().and_then(|e| e.holding_reply.clone());
        let withheld_notice = earlier.and_then(|e| e.withheld_notice);
        let mut holding = Holding {
            enabled: !self.config.policy.dry_run && !resource.starts_with("simulation:"),
            sent: holding_reply.is_some(),
            deadline: tokio::time::Instant::now() + HOLDING_AFTER,
        };
        let started = chrono::Utc::now().timestamp();
        let own_chat = self
            .config
            .graph
            .self_chat
            .as_ref()
            .is_some_and(|c| message.conversation == format!("chats/{}", c.id));
        let mut audit = Audit {
            status: "ignored".into(),
            reason: "ineligible_message".into(),
            holding_reply,
            withheld_notice,
            conversation_kind: Some(
                match (&message.kind, own_chat) {
                    (_, true) => "self",
                    (crate::adapters::teams::ConversationKind::Direct, _) => "direct",
                    (crate::adapters::teams::ConversationKind::Group, _) => "group",
                    _ => "unsupported",
                }
                .into(),
            ),
            received_at: Some(message.created_at_millis),
            ..Default::default()
        };
        audit.step("received", "message read from Teams");
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
            audit.step(
                "eligibility",
                "not evaluated: own, from a group without a mention, too old, empty or the assistant's output",
            );
            return self.store.record(resource, &audit);
        }
        let current = self.redactor.redact(&message.text);
        audit.question = Some(current.clone());
        audit.step("eligibility", "message addressed to the assistant");
        if !self.redactor.clean(&current) {
            audit.reason = "sensitive_question".into();
            audit.step("blocked", "the question contains sensitive data");
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
        // Earlier messages say what a short follow-up refers to («¿y el endpoint de test?»).
        // Read once, when an intent decision or an answer first needs them.
        let mut history: Option<String> = None;
        // Sources an activity review may read, already authorized for this conversation.
        let review_sources: Vec<_> = available
            .iter()
            .copied()
            .filter(|r| matches!(&r.access, Access::Tool { tool } if tool.reviews_activity()))
            .collect();
        let mut review = false;
        let is_greeting = if greeting(&current) {
            audit.step("intent", "greeting");
            true
        } else if !own_chat && personal_request(&current) {
            audit.step("intent", "request for the person");
            leave_to_person(&mut audit);
            return self.store.record(resource, &audit);
        } else if activity_review_request(&current) {
            review = true;
            audit.step("intent", "activity review");
            false
        } else if question_request(&current) {
            // A question about the user's own work may ask to compare it with their tasks;
            // only then a model decides, and only if some source can review activity.
            if !review_sources.is_empty()
                && work_mention(&current)
                && !status_question(&current)
                && !explicit_wiki(&current)
                && !documentation_question(&current)
            {
                let context = self.history(&message, &mut audit, &mut history).await;
                if let Ok(decision) = self.llm.classify_intent(&current, &context).await
                    && decision.intent == Intent::ActivityReview
                    && decision.confident()
                {
                    audit.confidences.push(decision.confidence);
                    review = true;
                }
            }
            audit.step(
                "intent",
                if review {
                    "activity review (model)"
                } else {
                    "question"
                },
            );
            false
        } else {
            if available.is_empty() {
                audit.reason = "no_authorized_resource".into();
                audit.step("sources", "no source is authorized for this conversation");
                return self.store.record(resource, &audit);
            }
            let context = self.history(&message, &mut audit, &mut history).await;
            // No holding notice yet: the message may not need an answer at all.
            let decision = self.llm.classify_intent(&current, &context).await?;
            audit.confidences.push(decision.confidence);
            audit.step(
                "intent",
                format!(
                    "model: {} ({:.2})",
                    decision.intent.as_str(),
                    decision.confidence
                ),
            );
            let answers = match decision.intent {
                Intent::Question | Intent::ActivityReview | Intent::Greeting => true,
                // In the personal chat the user is the one asking the assistant.
                Intent::Personal => own_chat,
                Intent::Statement => false,
            };
            if !decision.confident() || !answers {
                if decision.intent == Intent::Personal {
                    leave_to_person(&mut audit);
                } else {
                    audit.reason = "informational_message".into();
                    audit.step("no answer", "informational message: it asks for no answer");
                }
                return self.store.record(resource, &audit);
            }
            review = decision.intent == Intent::ActivityReview;
            decision.intent == Intent::Greeting
        };
        // Without a source that can read activity, a review is answered as a question.
        review &= !review_sources.is_empty();
        audit.intent = Some(
            if is_greeting {
                Intent::Greeting
            } else if review {
                Intent::ActivityReview
            } else {
                Intent::Question
            }
            .as_str()
            .into(),
        );
        let mut context_question = None;
        let mut context_answer = None;
        let proposal = if is_greeting {
            audit.reason = "deterministic_greeting".into();
            self.config.policy.greeting.clone()
        } else {
            let previous = self.store.context(&message.conversation)?;
            let history = self.history(&message, &mut audit, &mut history).await;
            audit.step(
                "context",
                format!(
                    "{} earlier messages{}",
                    audit.history_messages,
                    if previous.is_some() {
                        " and the last exchange with the assistant"
                    } else {
                        ""
                    }
                ),
            );
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
                    | "more details"
                    | "give me more details"
                    | "tell me more"
                    | "go on"
                    | "continue"
                    | "explain more"
                    | "expand"
            );
            let mut tool_question = match &previous {
                Some(previous) if more_details => previous.question.clone(),
                _ => current.clone(),
            };
            // History helps interpret the request; a classifier cannot veto a new topic.
            let question = if let Some(previous) = &previous {
                format!(
                    "Earlier context (reference only, not evidence):\nQuestion: {}\nAnswer: {}\nCurrent request (takes priority): {}",
                    previous.question, previous.answer, current
                )
            } else {
                current.clone()
            };
            // Questions containing secrets/PII stay manual, including tool requests with personal identifiers.
            if !self.redactor.clean(&question) {
                audit.reason = "sensitive_question".into();
                audit.step("blocked", "the question contains sensitive data");
                return self.store.record(resource, &audit);
            }
            if available.is_empty() {
                audit.reason = "no_authorized_resource".into();
                audit.step("sources", "no source is authorized for this conversation");
                return self.store.record(resource, &audit);
            }
            // Read every authorized document: choosing one descriptor loses compound questions.
            // Read tools are selected from this authorized set; no model vetoes retrieval.
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
            if (previous.is_some() || !history.is_empty())
                && !more_details
                && !review
                && !tool_candidates.is_empty()
            {
                let (previous_question, previous_answer) = previous
                    .as_ref()
                    .map_or(("", ""), |p| (p.question.as_str(), p.answer.as_str()));
                match self
                    .awaiting_model(
                        &mut holding,
                        resource,
                        &message,
                        &mut audit,
                        self.llm.standalone_request(
                            previous_question,
                            previous_answer,
                            &history,
                            &current,
                        ),
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
                            audit.resolved_question = Some(question.to_owned());
                            audit.topic = Some(topic.to_owned());
                            audit.step("follow-up", "request interpreted with the context");
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {
                        tracing::warn!(event = "follow_up_resolution_unavailable");
                        audit.step(
                            "follow-up",
                            "no model could interpret it; the literal text is searched",
                        );
                    }
                }
            }
            // Chained follow-ups keep the resolved topic instead of the bare follow-up.
            context_question = Some(tool_question.chars().take(1800).collect::<String>());
            let question = if wiki_topic.is_some() {
                format!("{question}\nCurrent request interpreted with the context: {tool_question}")
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
            let selected_tool = if review {
                // An activity review reads every source that can review activity, below.
                None
            } else if wiki_requested && wiki_ids.len() == 1 {
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
                                    | crate::tools::ToolSpec::TeamsMessages {}
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
            if review {
                // The one case that combines tools: activity, Wiki edits and own messages.
                sources = review_sources.clone();
            }
            audit.source = (!sources.is_empty()).then(|| {
                sources
                    .iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            });
            audit.step(
                "sources",
                audit.source.clone().unwrap_or_else(|| "none".into()),
            );
            self.checkpoint(resource, &audit, "retrieving")?;
            let (mut evidence, registry) = if review {
                let days = crate::ado::recent_window(&tool_question);
                audit.step(
                    "activity review",
                    format!("last {days} days in {} sources", sources.len()),
                );
                self.review_evidence(&sources, days, resource, &message, &mut audit)
                    .await?
            } else {
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
                                    (
                                        crate::tools::ToolSpec::AzureDevopsWiki { .. },
                                        Some(topic),
                                    ) => topic.clone(),
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
                            let mut result: crate::ado::wiki::WikiResult =
                                serde_json::from_str(&raw)?;
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
                        let passages =
                            crate::knowledge::excerpt(&sanitized, &question, passage_budget);
                        evidence.extend(format!("{header}{passages}\n").chars().take(budget));
                    }
                }
                (evidence, registry)
            };
            audit.step(
                "evidence",
                format!(
                    "{} characters, {} verified references",
                    evidence.chars().count(),
                    registry.references.len()
                ),
            );
            if evidence.trim().is_empty() {
                evidence = "No verifiable facts were retrieved. Explain the limitation and ask to specify the source or the project; do not make up an answer.".chars().take(self.config.policy.max_context_chars).collect();
            }
            audit.status = "ignored".into();
            // Relevance is assessed while answering, with missing facts qualified. Only code
            // checks (references, URLs, sensitive data, size) may withhold a generated answer.
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
                        history: &history,
                        review,
                    }),
                )
                .await??;
            // A provider chain names the member that actually wrote the answer.
            if let Some(provider) = &generated.provider {
                audit.provider = Some(provider.clone());
            }
            audit.provider_fallbacks = generated.fallbacks.clone();
            audit.step(
                "model",
                match generated.fallbacks.as_slice() {
                    [] => format!(
                        "{} answered",
                        audit.provider.as_deref().unwrap_or("the model")
                    ),
                    failed => format!(
                        "{} answered after {} failed",
                        audit.provider.as_deref().unwrap_or("the model"),
                        failed.join(", ")
                    ),
                },
            );
            context_answer = Some(generated.answer.clone());
            audit.partial = registry.partial;
            audit.coverage_warnings = registry.warnings.clone();
            audit.references = registry.references.clone();
            audit.teams_messages = registry.teams.clone();
            audit.detailed = Some(generated.detailed);
            audit.proposed = Some(self.redactor.redact(&generated.answer));
            if !self.valid_answer(&generated.answer) {
                return self
                    .withhold(resource, &message, &mut audit, "unsafe_proposal".into())
                    .await;
            }
            // A model suggests which references the text used, but cannot withhold the answer:
            // without its selection, code links the entities it sees named and the consulted
            // Wiki pages, and verifies every ID and URL.
            let mut used_sources = generated.used_sources.clone();
            if !registry.references.is_empty() {
                // Code links the entities it sees named; a model chooses among the rest
                // (Wiki pages, messages, entities paraphrased without their identifier).
                for id in crate::evidence::named_references(&generated.answer, &registry) {
                    if !used_sources.contains(&id) {
                        used_sources.push(id);
                    }
                }
                let remaining: Vec<_> = registry
                    .references
                    .iter()
                    .filter(|r| !used_sources.contains(&r.id))
                    .cloned()
                    .collect();
                if remaining.is_empty() {
                    audit.reference_selection = Some("code".into());
                    audit.step("references", "code links every one it sees named");
                } else {
                    self.checkpoint(resource, &audit, "selecting_references")?;
                    match self
                        .awaiting_model(
                            &mut holding,
                            resource,
                            &message,
                            &mut audit,
                            self.llm.select_references(&generated.answer, &remaining),
                        )
                        .await?
                    {
                        Ok(ids) => {
                            audit.reference_selection = Some("llm".into());
                            audit.step("references", format!("the model chose {}", ids.len()));
                            for id in ids {
                                if !used_sources.contains(&id) {
                                    used_sources.push(id);
                                }
                            }
                        }
                        Err(_) => {
                            audit.reference_selection = Some("code".into());
                            audit.step(
                                "references",
                                "no model could choose them; code links what it sees named",
                            );
                        }
                    }
                }
            }
            audit.used_sources = used_sources.clone();
            let answer = if !registry.references.is_empty() {
                match crate::evidence::complete_answer(
                    &generated.answer,
                    &used_sources,
                    &registry,
                    &evidence,
                    self.config.llm.language,
                ) {
                    Ok(answer) => answer,
                    Err(error) => {
                        return self
                            .withhold(
                                resource,
                                &message,
                                &mut audit,
                                format!("invalid_references: {error}"),
                            )
                            .await;
                    }
                }
            } else if !generated.used_sources.is_empty() {
                return self
                    .withhold(
                        resource,
                        &message,
                        &mut audit,
                        "invalid_references: no authorized evidence".into(),
                    )
                    .await;
            } else {
                generated.answer
            };
            audit.proposed = Some(self.redactor.redact(&answer));
            if !self.valid_answer(&answer) {
                return self
                    .withhold(resource, &message, &mut audit, "unsafe_proposal".into())
                    .await;
            }
            audit.reason = "supported_answer".into();
            answer
        };
        audit.proposed = Some(self.redactor.redact(&proposal));
        if !self.valid_answer(&proposal) {
            return self
                .withhold(resource, &message, &mut audit, "unsafe_answer".into())
                .await;
        }
        if self.config.policy.dry_run {
            audit.status = "dry_run".into();
            audit.step(
                "observation mode",
                "proposal recorded; nothing is sent to Teams",
            );
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
            audit.step(
                "not sent",
                "the message changed, was deleted or stopped being eligible",
            );
            return self.store.record(resource, &audit);
        }
        audit.status = "sending".into();
        self.store.record(resource, &audit)?;
        match self.adapter.send(&message, &proposal).await {
            Ok(id) => {
                audit.status = "sent".into();
                audit.sent = Some(proposal);
                audit.graph_message_id = Some(id);
                audit.step("send", "answer sent to Teams");
            }
            Err(_) => {
                audit.status = "uncertain".into();
                audit.reason = "send_result_unknown_manual_review".into();
                audit.step(
                    "send",
                    "uncertain result; review it manually (never retried)",
                );
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
    /// Read every activity source for a review, each on its own: a source that fails is
    /// reported as unread instead of failing the answer. The context budget goes first to
    /// the shorter sources; longer ones are cut at line boundaries and marked partial.
    async fn review_evidence(
        &self,
        sources: &[&crate::knowledge::Resource],
        days: i64,
        resource: &str,
        message: &crate::adapters::teams::IncomingMessage,
        audit: &mut Audit,
    ) -> Result<(String, crate::evidence::Evidence)> {
        let mut registry = crate::evidence::Evidence::default();
        let mut parts = Vec::new();
        // Read the sources concurrently: each review takes tens of seconds.
        let mut reads = tokio::task::JoinSet::new();
        for (index, source) in sources.iter().enumerate() {
            let Access::Tool { tool } = &source.access else {
                continue;
            };
            audit.tools.push(tool.name().into());
            let scoped = ScopedTool {
                executor: self.tools.clone(),
                spec: tool.clone(),
                conversation: message.conversation.clone(),
            };
            reads.spawn(async move { (index, scoped.review(days).await) });
        }
        audit.status = "processing".into();
        audit.reason = "tool_started".into();
        self.store.record(resource, audit)?;
        let mut results = Vec::new();
        while let Some(read) = reads.join_next().await {
            results.push(read?);
        }
        results.sort_by_key(|(index, _)| *index);
        let mut unread = Vec::new();
        for (index, read) in results {
            let source = sources[index];
            let what = match &source.access {
                Access::Tool {
                    tool: crate::tools::ToolSpec::AzureDevopsWiki { .. },
                } => "Wiki edits",
                Access::Tool {
                    tool: crate::tools::ToolSpec::TeamsMessages {},
                } => "own Teams messages",
                _ => "Azure DevOps activity",
            };
            let read = read
                .ok()
                .and_then(|raw| serde_json::from_str::<crate::evidence::Evidence>(&raw).ok());
            match read {
                Some(mut data) => {
                    data.sanitize(&self.redactor);
                    parts.push((what, data.text.clone()));
                    registry.references.extend(data.references);
                    registry.teams.extend(data.teams);
                    registry.partial |= data.partial;
                    registry.warnings.extend(data.warnings);
                }
                None => {
                    registry.partial = true;
                    audit.step(
                        "activity review",
                        format!("{} could not be read", source.id),
                    );
                    unread.push(what);
                }
            }
        }
        let lengths: Vec<usize> = parts.iter().map(|(_, p)| p.chars().count()).collect();
        let mut evidence = String::new();
        let mut cut = Vec::new();
        for ((what, part), cap) in parts
            .iter()
            .zip(shares(&lengths, self.config.policy.max_context_chars))
        {
            let mut used = 0;
            for line in part.lines() {
                let size = line.chars().count() + 1;
                if used + size > cap {
                    registry.partial = true;
                    cut.push(*what);
                    break;
                }
                evidence.push_str(line);
                evidence.push('\n');
                used += size;
            }
        }
        // Name what is missing, so the answer can say it in one line.
        for (list, state) in [
            (&unread, "could not be read"),
            (&cut, "were cut to fit the context"),
        ] {
            if !list.is_empty() {
                evidence.push_str(&format!(
                    "Partial coverage: {} {state}; do not infer that there was no activity there.\n",
                    list.join(" and ")
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        registry.references.retain(|r| seen.insert(r.id.clone()));
        Ok((evidence, registry))
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
            match self
                .adapter
                .send(message, holding_reply(self.config.llm.language))
                .await
            {
                Ok(_) => "sent",
                Err(_) => "uncertain",
            }
            .into(),
        );
        self.checkpoint(resource, audit, "holding_reply")?;
        Ok(work.await)
    }
    /// Record a generated answer that code checks withheld. In the personal chat, also tell
    /// the user once, with a fixed text: the intent is recorded first and a failed send is
    /// never retried, like the holding notice.
    async fn withhold(
        &self,
        resource: &str,
        message: &crate::adapters::teams::IncomingMessage,
        audit: &mut Audit,
        reason: String,
    ) -> Result<()> {
        // The audit log is read in the app, which is in English; the notice follows the reply language.
        let explanation = withheld_reason(&reason, Language::En);
        let language = self.config.llm.language;
        let cause = withheld_reason(&reason, language);
        audit.reason = reason;
        audit.step("withheld", explanation);
        self.store.record(resource, audit)?;
        let own_chat = self
            .config
            .graph
            .self_chat
            .as_ref()
            .is_some_and(|c| message.conversation == format!("chats/{}", c.id));
        if !own_chat
            || self.config.policy.dry_run
            || resource.starts_with("simulation:")
            || audit.withheld_notice.is_some()
        {
            return Ok(());
        }
        audit.withheld_notice = Some("sending".into());
        self.store.record(resource, audit)?;
        let notice = match language {
            Language::Es => format!(
                "No envié la respuesta a tu mensaje: {cause}. Puedes revisarla en la sección Mensajes de la app."
            ),
            Language::En => format!(
                "I didn't send the reply to your message: it {cause}. You can review it in the app's Messages section (Mensajes)."
            ),
        };
        let sent = self.adapter.send(message, &notice).await.is_ok();
        audit.withheld_notice = Some(if sent { "sent" } else { "uncertain" }.into());
        audit.step(
            "notice",
            if sent {
                "notice sent to the personal chat"
            } else {
                "notice with an uncertain result (never retried)"
            },
        );
        self.store.record(resource, audit)
    }
    /// The conversation's earlier messages as text, read from Teams once per message.
    async fn history(
        &self,
        message: &crate::adapters::teams::IncomingMessage,
        audit: &mut Audit,
        cached: &mut Option<String>,
    ) -> String {
        if let Some(text) = cached {
            return text.clone();
        }
        let history = match self.adapter.history(message, HISTORY_MESSAGES).await {
            Ok(history) => history,
            Err(_) => {
                tracing::warn!(event = "conversation_history_unavailable");
                Vec::new()
            }
        };
        audit.history_messages = history.len();
        let text = self.history_text(&history, message.created_at_millis);
        *cached = Some(text.clone());
        text
    }
    /// Earlier messages as `[date time · author] text`, redacted, newest kept within budget,
    /// followed by the time of the current request.
    fn history_text(
        &self,
        history: &[crate::adapters::teams::HistoryMessage],
        current_millis: i64,
    ) -> String {
        let mut lines: Vec<String> = Vec::new();
        let mut total = 0;
        for message in history.iter().rev() {
            let line = format!(
                "[{} · {}] {}",
                message.at,
                self.redactor.redact(&message.author),
                self.redactor.redact(&message.text).replace('\n', " ")
            );
            total += line.chars().count() + 1;
            if total > HISTORY_CHARS {
                break;
            }
            lines.push(line);
        }
        if lines.is_empty() {
            return String::new();
        }
        lines.reverse();
        if let Some(at) = chrono::DateTime::from_timestamp_millis(current_millis) {
            lines.push(format!(
                "[{} · current request]",
                at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")
            ));
        }
        lines.join("\n")
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

/// Plain-language cause of a withheld answer, without its content.
fn withheld_reason(reason: &str, language: Language) -> &'static str {
    let unsafe_answer = matches!(reason, "unsafe_proposal" | "unsafe_answer");
    let references = reason.starts_with("invalid_references");
    match language {
        Language::Es if unsafe_answer => {
            "contenía datos sensibles o no cabía en un mensaje de Teams"
        }
        Language::Es if references => "citaba referencias o enlaces que no se pudieron verificar",
        Language::Es => "no pasó los controles de la aplicación",
        Language::En if unsafe_answer => {
            "contained sensitive data or didn't fit in a Teams message"
        }
        Language::En if references => "cited references or links that couldn't be verified",
        Language::En => "didn't pass the app's checks",
    }
}
/// Split `total` characters among parts: short parts keep everything, the rest share what is
/// left evenly.
fn shares(lengths: &[usize], total: usize) -> Vec<usize> {
    let mut caps = vec![0; lengths.len()];
    let mut open: Vec<usize> = (0..lengths.len()).collect();
    let mut remaining = total;
    while !open.is_empty() {
        let share = remaining / open.len();
        let (fits, rest): (Vec<usize>, Vec<usize>) =
            open.iter().partition(|&&i| lengths[i] <= share);
        if fits.is_empty() {
            for i in rest {
                caps[i] = share;
            }
            break;
        }
        for i in fits {
            caps[i] = lengths[i];
            remaining -= lengths[i];
        }
        open = rest;
    }
    caps
}
fn normalized_phrase(text: &str) -> String {
    let normalized = text
        .to_lowercase()
        .replace('ó', "o")
        .replace('é', "e")
        .replace('í', "i")
        .replace('á', "a")
        .replace('ú', "u");
    let words: Vec<&str> = normalized
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    format!(" {} ", words.join(" "))
}
/// Asks to find the user's work that no task records («¿qué hice que no está registrado?»).
fn activity_review_request(text: &str) -> bool {
    let phrase = normalized_phrase(text);
    [
        " no registrad",
        " sin registrar",
        " no esta registrad",
        " no estan registrad",
        " falta registrar",
        " faltan registrar",
        " por registrar",
        " no he registrado",
        " sin tarea ",
        " sin tareas ",
        " sin hu ",
        " sin work item",
        " no tiene tarea",
        " no tienen tarea",
        " no tiene hu ",
        " no tienen hu ",
        " sin imputar",
        " no imputad",
        " not registered",
        " unregistered",
        " not logged",
        " unlogged",
        " not tracked",
        " untracked",
        " without a task",
        " without tasks",
        " without a work item",
        " without work items",
        " haven t logged",
        " have not logged",
        " missing from my tasks",
    ]
    .iter()
    .any(|p| phrase.contains(p))
}
/// Mentions the user's own work or tasks, so a model may check for an activity review.
fn work_mention(text: &str) -> bool {
    let phrase = normalized_phrase(text);
    phrase.split(' ').any(|w| {
        matches!(
            w,
            "tarea"
                | "tareas"
                | "trabajo"
                | "trabajos"
                | "trabaje"
                | "trabajado"
                | "hice"
                | "hecho"
                | "realizado"
                | "realice"
                | "registrado"
                | "registrados"
                | "registrar"
                | "actividad"
                | "imputar"
                | "work"
                | "worked"
                | "task"
                | "tasks"
                | "did"
                | "done"
                | "logged"
                | "activity"
        )
    })
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
    // «necesito que…», «cuando puedas…» ask someone to act; the intent classifier decides who.
    if [
        "necesito que ",
        "cuando puedas",
        "i need you to ",
        "when you can",
        "please ",
    ]
    .iter()
    .any(|p| normalized.starts_with(p))
    {
        return false;
    }
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
        "which ",
        "where ",
        "when ",
        "why ",
        "who ",
        "can you ",
        "could you ",
        "do you know ",
        "is there ",
        "are there ",
        "explain ",
        "tell me ",
        "show me ",
        "give me ",
        "list ",
        "find ",
        "summarize ",
        "describe ",
        "i need ",
    ]
    .iter()
    .any(|prefix| normalized.starts_with(prefix))
}
fn leave_to_person(audit: &mut Audit) {
    audit.reason = "personal_request".into();
    audit.step(
        "no answer",
        "asks for a call, a meeting, a joint review or availability: the person answers it",
    );
}
/// A call, meeting, joint review or the person's availability. Only the person can answer
/// it, even when it is phrased as a question («¿te puedo llamar?»).
fn personal_request(text: &str) -> bool {
    let normalized = text
        .to_lowercase()
        .replace('ó', "o")
        .replace('é', "e")
        .replace('í', "i")
        .replace('á', "a")
        .replace('ú', "u");
    let words: Vec<&str> = normalized
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    // «te llamo», «me puedes llamar»; not «cómo se llama a…» nor «me llamo Juan».
    let call = |w: &str| w.starts_with("llam");
    let call_between_us = words
        .windows(2)
        .any(|w| matches!(w[0], "te" | "me") && call(w[1]) && w != ["me", "llamo"])
        || words.windows(3).any(|w| {
            matches!(w[0], "te" | "me")
                && matches!(w[1], "puedo" | "puedes" | "podria" | "podrias")
                && call(w[2])
        });
    let phrase = format!(" {} ", words.join(" "));
    call_between_us
        || words.iter().any(|w| {
            matches!(
                *w,
                "llamarte"
                    | "llamarme"
                    | "llamame"
                    | "videollamada"
                    | "contigo"
                    | "hablemos"
                    | "conversemos"
                    | "revisemos"
                    | "coordinemos"
                    | "agendemos"
                    | "juntemonos"
                    | "reunamonos"
                    | "conectemonos"
                    | "juntarnos"
                    | "reunirnos"
                    | "conectarte"
                    | "conectate"
            )
        })
        || [
            " tienes un minuto ",
            " tienes un momento ",
            " tienes un rato ",
            " tienes unos minutos ",
            " tienes tiempo ",
            " estas disponible ",
            " estas libre ",
            " estas ahi ",
            " puedes hablar ",
            " podemos hablar ",
            " call me ",
            " call you ",
            " give me a call ",
            " on a call ",
            " have a call ",
            " quick call ",
            " are you available ",
            " are you free ",
            " can we talk ",
            " can we meet ",
            " let s talk ",
            " lets talk ",
            " let s meet ",
            " lets meet ",
            " catch up ",
            " sync up ",
            " do you have a minute ",
            " do you have a moment ",
            " do you have time ",
            " got a minute ",
            " with you ",
        ]
        .iter()
        .any(|p| phrase.contains(p))
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
        "how to install ",
        "how to invoke ",
        "how to call ",
        "how to integrate ",
        "how do i use ",
        "how do i configure ",
        "how do i install ",
        "how do i call ",
        "how do i invoke ",
        "how does ",
        "what is ",
        "what does ",
        "what parameters ",
        "what are the requirements ",
        "explain ",
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
                | "create"
                | "run"
                | "deploy"
                | "approve"
                | "modify"
                | "trigger"
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
                | "progress"
                | "blockers"
                | "impediments"
                | "commitments"
        )
    }) || (q.contains("esta semana")
        && (q.contains("he hecho") || q.contains("trabaj") || q.contains("hice")))
        || (q.contains("this week")
            && (q.contains("did") || q.contains("worked") || q.contains("done")))
}

#[cfg(test)]
mod tests {
    use super::{
        Language, activity_review_request, documentation_question, holding_reply, personal_request,
        question_request, shares, status_question, withheld_reason, work_mention,
    };

    #[test]
    fn unregistered_work_requests_are_activity_reviews() {
        for q in [
            // The real request of 2026-10-03, typos included.
            "Hola qué tareas o trabajo e realizado que no están registrados en tareas ?",
            "¿Qué hice esta semana que no está registrado?",
            "lista el trabajo sin tarea del último mes",
            "¿Tengo commits sin HU?",
            "What work did I do that is not logged?",
            "Find my untracked work from last week",
        ] {
            assert!(activity_review_request(q), "{q}");
        }
        for q in [
            "¿Cómo se registra una HU?",
            "¿Qué he hecho esta semana?",
            "¿Cuál es el endpoint de test?",
            "Registrar el usuario en el servicio",
        ] {
            assert!(!activity_review_request(q), "{q}");
        }
        assert!(work_mention("¿Qué trabajo hice ayer que debería anotar?"));
        assert!(work_mention("What did I do yesterday?"));
        assert!(!work_mention("¿Cuál es el endpoint de test?"));
    }

    #[test]
    fn short_sources_keep_everything_and_long_ones_share_the_rest() {
        assert_eq!(shares(&[100, 9000, 9000], 6000), vec![100, 2950, 2950]);
        assert_eq!(shares(&[100, 200], 6000), vec![100, 200]);
        assert_eq!(shares(&[], 6000), Vec::<usize>::new());
        assert_eq!(shares(&[5000, 5000], 6000), vec![3000, 3000]);
    }

    #[test]
    fn fixed_notices_follow_the_reply_language() {
        assert_eq!(holding_reply(Language::Es), "Déjame revisarlo.");
        assert_eq!(holding_reply(Language::En), "Let me look into it.");
        for (reason, es, en) in [
            (
                "unsafe_answer",
                "contenía datos sensibles o no cabía en un mensaje de Teams",
                "contained sensitive data or didn't fit in a Teams message",
            ),
            (
                "invalid_references: unverified answer URL",
                "citaba referencias o enlaces que no se pudieron verificar",
                "cited references or links that couldn't be verified",
            ),
            (
                "other",
                "no pasó los controles de la aplicación",
                "didn't pass the app's checks",
            ),
        ] {
            assert_eq!(withheld_reason(reason, Language::Es), es);
            assert_eq!(withheld_reason(reason, Language::En), en);
        }
    }

    #[test]
    fn clear_information_requests_do_not_need_a_model_to_allow_retrieval() {
        for q in [
            "como funciona la notificacion de pagos",
            "Explícame la integración",
            "¿Qué requisitos tiene?",
            "Necesito los parámetros",
            "dame más detalles",
            "Como se usa un componente desconocido",
            "Which endpoint does the test environment use",
            "Can you explain the deployment",
            "Show me the parameters",
        ] {
            assert!(question_request(q), "{q}");
        }
        for q in [
            "El despliegue terminó",
            "He añadido una Wiki",
            "Crea una HU mañana",
            "Necesito que me envíes el documento",
            "Cuando puedas lo revisamos",
            "The deployment finished",
            "Please send me the document",
        ] {
            assert!(!question_request(q), "{q}");
        }
    }

    #[test]
    fn requests_for_the_person_are_not_information_requests() {
        for q in [
            // Real messages answered by mistake on 2026-10-02.
            "te puedo llamar",
            "necesito llamarte",
            "necesito que revisemos lo que se debe subir en el próximo paso a prod",
            "¿Me puedes llamar cuando puedas?",
            "Llámame",
            "¿Tienes un minuto?",
            "¿Estás disponible?",
            "Hablemos mañana del despliegue",
            "¿Lo vemos contigo en la tarde?",
            "¿Juntémonos a las 3?",
            "Can we talk?",
            "Do you have a minute?",
            "Let's meet tomorrow about the deploy",
            "Can you give me a call?",
            "Are you free at 3?",
        ] {
            assert!(personal_request(q), "{q}");
        }
        for q in [
            "¿Cómo se llama al servicio de pagos?",
            "¿Cómo hago una llamada al endpoint de test?",
            "¿Me explicas cómo llamar al microservicio?",
            "Hola, me llamo Juan: ¿cómo configuro el pipeline?",
            "Necesito los parámetros",
            "¿Qué se debe subir a prod en el próximo paso?",
            "en test y desa se cae",
            "How do I call the payments service?",
            "What does the call to the endpoint return?",
        ] {
            assert!(!personal_request(q), "{q}");
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
            "How does the payment notification work?",
            "What parameters does the pipeline need?",
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
        assert!(status_question("What did I do this week?"));
        assert!(status_question("Any blockers on my work items?"));
        assert!(!status_question("Deploy the release now"));
        assert!(status_question(
            "Pregunta anterior: avance de HU. Solicitud actual: dame más detalles"
        ));
        assert!(!status_question("Crea una HU para mañana"));
        assert!(!status_question("Despliega el release ahora"));
        assert!(!status_question("¿Cuál es el horario de soporte?"));
    }
}
