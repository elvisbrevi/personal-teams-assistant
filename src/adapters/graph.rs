use super::{
    MessageAdapter,
    oauth::AccessToken,
    teams::{self, ConversationKind, GraphMessage, IncomingMessage},
};
use crate::{
    config::Config,
    state::{Store, Subscription},
};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::Arc;

/// A message the user wrote, with the chat it belongs to and its Teams link when Graph gives one.
#[derive(Debug)]
pub struct OwnMessage {
    pub message: crate::evidence::TeamsMessage,
    /// The chat's topic, or the other participants seen in it.
    pub chat: String,
    pub url: Option<String>,
}

/// Review evidence for the user's own Teams messages: dated lines grouped by chat, the
/// messages themselves (author kept) and a link for each message Graph gives one for.
pub fn own_messages_evidence(
    messages: &[OwnMessage],
    chats: usize,
    partial: bool,
    days: i64,
) -> crate::evidence::Evidence {
    let since = chrono::Utc::now() - chrono::Duration::days(days);
    let mut text = format!(
        "The user's own Teams messages from {} to {} (last {days} days; {} messages in {chats} chats, the personal chat left out). They show what the user said they did or coordinated; only messages that describe concrete work count as activity:\n",
        since.format("%Y-%m-%d"),
        chrono::Utc::now().format("%Y-%m-%d"),
        messages.len(),
    );
    let mut references = Vec::new();
    for own in messages {
        let at = chrono::DateTime::parse_from_rfc3339(&own.message.date)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|_| own.message.date.clone());
        let mentions = !crate::ado::review::mentions(&own.message.text).is_empty()
            || own.message.text.contains("/_workitems/edit/");
        text.push_str(&format!(
            "- {at} · {}: \"{}\"{}\n",
            own.chat,
            own.message.text.replace('\n', " "),
            if mentions {
                " (mentions a work item)"
            } else {
                ""
            }
        ));
        if let Some(url) = &own.url {
            references.push(crate::evidence::Reference {
                id: format!(
                    "teams_message:{}:{}",
                    own.message.conversation, own.message.message
                ),
                kind: "teams_message".into(),
                label: format!("Teams message of {at} in {}", own.chat),
                url: url.clone(),
                organization: "https://teams.microsoft.com".into(),
                project: "teams".into(),
                aliases: Vec::new(),
                parent: None,
                revision: None,
                authority: Some("mine".into()),
                author: None,
                author_role: None,
            });
        }
    }
    if messages.is_empty() {
        text.push_str("No message written by the user was found in the window.\n");
    }
    if partial {
        text.push_str("Partial coverage: some chats could not be read.\n");
    }
    references.retain(|r| r.validate().is_ok());
    crate::evidence::Evidence {
        text,
        references,
        teams: messages.iter().map(|m| m.message.clone()).collect(),
        partial,
        ..Default::default()
    }
}

#[derive(Clone)]
pub struct Graph {
    pub client: reqwest::Client,
    pub token: Arc<dyn AccessToken>,
    pub base_url: String,
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub client_state: String,
}

fn call_hours(duration: &str) -> Option<f64> {
    let pattern =
        regex::Regex::new(r"^PT(?:(\d+(?:\.\d+)?)H)?(?:(\d+(?:\.\d+)?)M)?(?:(\d+(?:\.\d+)?)S)?$")
            .ok()?;
    let c = pattern.captures(duration)?;
    let number = |index| {
        c.get(index)
            .and_then(|v| v.as_str().parse::<f64>().ok())
            .unwrap_or(0.)
    };
    let hours = number(1) + number(2) / 60. + number(3) / 3600.;
    (hours > 0. && hours <= 24.).then_some(hours)
}
fn registration_message(
    message: &Value,
    chat: &str,
    topic: &str,
    user: &str,
    since: chrono::DateTime<chrono::Utc>,
) -> Option<crate::ado::link::Linkable> {
    if !message["deletedDateTime"].is_null() {
        return None;
    }
    let timestamp = message["createdDateTime"].as_str()?;
    let at = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    if at < since || at > chrono::Utc::now() {
        return None;
    }
    let call = message["eventDetail"]["@odata.type"]
        .as_str()
        .is_some_and(|t| t.ends_with("callEndedEventMessageDetail"));
    let mine = message["messageType"].as_str() == Some("message")
        && message["from"]["user"]["id"].as_str() == Some(user);
    if !mine && !call {
        return None;
    }
    let mut url = message["webUrl"].as_str().map(str::to_owned);
    // Graph frequently omits webUrl on system messages. Build the documented deep link
    // only from the validated chat ID and the message ID that Graph returned.
    if url.is_none() {
        let mut address = url::Url::parse("https://teams.microsoft.com/l/message/").ok()?;
        address
            .path_segments_mut()
            .ok()?
            .pop_if_empty()
            .push(chat)
            .push(message["id"].as_str()?);
        url = Some(address.to_string());
    }
    let url = url?;
    let parsed = url::Url::parse(&url).ok()?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("teams.microsoft.com")
        || parsed.port_or_known_default() != Some(443)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !parsed.path().starts_with("/l/message/")
    {
        return None;
    }
    let raw = message["body"]["content"].as_str().unwrap_or("");
    if raw.contains("personalteams.invalid/output/") {
        return None;
    }
    let text = if message["body"]["contentType"].as_str() == Some("html") {
        teams::plain_text(raw)
    } else {
        raw.to_owned()
    };
    if !call && text.trim().is_empty() {
        return None;
    }
    let label = if call {
        format!("Call ended in {topic}. Its purpose and the user's attendance are not verified.")
    } else {
        format!(
            "Own message in {topic}: {}",
            text.chars().take(600).collect::<String>()
        )
    };
    Some(crate::ado::link::Linkable {
        label,
        short: if call {
            format!("Call in {topic}")
        } else {
            format!("Work described in {topic}")
        },
        url,
        organization: "https://teams.microsoft.com".into(),
        date: timestamp.get(..10)?.into(),
        occurred_at: timestamp.into(),
        context_required: call,
        duration_hours: if call {
            message["eventDetail"]["callDuration"]
                .as_str()
                .and_then(call_hours)
        } else {
            None
        },
        ..Default::default()
    })
}
impl Graph {
    pub async fn verify_account(&self) -> Result<()> {
        let me = self.request(Method::GET, "me", None, true).await?;
        ensure!(
            me["id"].as_str() == Some(&self.config.graph.user_id),
            "connected account differs"
        );
        Ok(())
    }
    pub async fn validate_self_chat(&self, id: &str) -> Result<()> {
        teams::canonical_resource(&format!("chats/{id}/messages/0"))?;
        let me = self.request(Method::GET, "me", None, true).await?;
        ensure!(
            me["id"].as_str() == Some(&self.config.graph.user_id),
            "connected Graph account differs"
        );
        // Teams' reserved notes thread has no ChatThread metadata/members API.
        // Verify its delegated /me scope and actual authors; never infer another
        // chat is personal merely because the caller wrote its latest message.
        if id == "48:notes" {
            let page = self.personal_page(id).await?;
            let messages = page["value"]
                .as_array()
                .context("invalid personal message page")?;
            let human: Vec<_> = messages
                .iter()
                .filter(|m| m["messageType"] == "message")
                .collect();
            ensure!(
                !human.is_empty()
                    && human
                        .iter()
                        .all(|m| m["from"]["user"]["id"].as_str()
                            == Some(&self.config.graph.user_id)),
                "personal notes have no verified account authorship; write a note first"
            );
            return Ok(());
        }
        let chat = self
            .request(Method::GET, &format!("chats/{id}"), None, true)
            .await?;
        ensure!(
            chat["chatType"].as_str() == Some("oneOnOne"),
            "personal chat must be oneOnOne"
        );
        let members = self.list_collection(&format!("chats/{id}/members")).await?;
        ensure!(
            !members.is_empty()
                && members
                    .iter()
                    .all(|m| m["userId"].as_str() == Some(&self.config.graph.user_id)),
            "chat has another participant or no verified membership"
        );
        Ok(())
    }
    /// Explicit manual reconciliation by output nonce and a Graph message ID.
    pub async fn reconcile_output(&self, nonce: &str, id: &str) -> Result<()> {
        let chat = self
            .config
            .graph
            .self_chat
            .as_ref()
            .context("personal chat disabled")?;
        self.validate_self_chat(&chat.id).await?;
        let (conversation, created) = self
            .store
            .output_intent(nonce)?
            .context("unresolved output not found")?;
        ensure!(
            conversation == format!("chats/{}", chat.id),
            "output conversation differs"
        );
        let path = teams::canonical_resource(&format!("{conversation}/messages/{id}"))?;
        let value = self.request(Method::GET, &path, None, true).await?;
        let message: GraphMessage = serde_json::from_value(value)?;
        ensure!(
            message.id == id
                && message
                    .from
                    .and_then(|f| f.user)
                    .is_some_and(|u| u.id == chat.user_id)
                && message.created_date_time.timestamp() >= created
                && message.deleted_date_time.is_none(),
            "message does not match this send's account/time"
        );
        self.store.finish_output(nonce, id)
    }
    pub async fn discover_self_chat(&self) -> Result<String> {
        // A candidate only: validate it against this account before enabling it.
        if self.validate_self_chat("48:notes").await.is_ok() {
            return Ok("48:notes".into());
        }
        let mut found = Vec::new();
        for id in self.list_chats().await? {
            if self.validate_self_chat(&id).await.is_ok() {
                found.push(id);
            }
        }
        ensure!(
            found.len() == 1,
            "personal chat discovery is inconclusive; provide a chat ID to validate"
        );
        Ok(found.remove(0))
    }
    async fn personal_page(&self, id: &str) -> Result<Value> {
        let path = if id == "48:notes" {
            format!("me/chats/{id}/messages")
        } else {
            format!("chats/{id}/messages")
        };
        let mut url = self.url(&path)?;
        url.query_pairs_mut()
            .append_pair("$top", "50")
            .append_pair("$orderby", "createdDateTime desc");
        self.request_url(Method::GET, url, None, true).await
    }
    pub async fn poll_self_chat(&self) -> Result<()> {
        if let Some(chat) = &self.config.graph.self_chat {
            self.validate_self_chat(&chat.id).await?;
            let collection = format!("chats/{}/messages", chat.id);
            let page = self.personal_page(&chat.id).await?;
            let mut newest = chat.enabled_at;
            for message in page["value"]
                .as_array()
                .context("invalid personal message page")?
                .iter()
                .rev()
            {
                let date = message["createdDateTime"]
                    .as_str()
                    .context("message date missing")?;
                let created = chrono::DateTime::parse_from_rfc3339(date)?.timestamp_millis();
                newest = newest.max(created);
                // A bounded overlapping page plus jobs' persistent PK handles duplicates/restarts.
                if created > chat.enabled_at {
                    let id = message["id"].as_str().context("message ID missing")?;
                    self.store.is_output(
                        &format!("chats/{}", chat.id),
                        id,
                        message["body"]["content"].as_str().unwrap_or(""),
                    )?;
                    self.store
                        .enqueue(&teams::canonical_resource(&format!("{collection}/{id}"))?)?;
                }
            }
            self.store
                .save_activity_cache("self_chat_cursor", &newest.to_string())?;
        }
        Ok(())
    }
    pub async fn recent_project_context(
        &self,
        projects: &[String],
        conversation: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<crate::evidence::TeamsMessage>> {
        let broad = conversation.starts_with("chats/simulation-");
        if projects.is_empty() && !broad {
            return Ok(Vec::new());
        }
        let chats = if broad {
            let mut url = self.url("me/chats")?;
            url.query_pairs_mut()
                .append_pair("$expand", "lastMessagePreview")
                .append_pair("$orderby", "lastMessagePreview/createdDateTime desc")
                .append_pair("$top", "50");
            let page = self.request_url(Method::GET, url, None, true).await?;
            page["value"]
                .as_array()
                .context("invalid Graph chats")?
                .iter()
                .filter(|c| matches!(c["chatType"].as_str(), Some("oneOnOne" | "group")))
                .filter(|c| {
                    c["lastUpdatedDateTime"].as_str().is_some_and(|date| {
                        chrono::DateTime::parse_from_rfc3339(date)
                            .is_ok_and(|d| d.with_timezone(&chrono::Utc) >= since)
                    })
                })
                .take(30)
                .cloned()
                .collect::<Vec<_>>()
        } else if conversation.starts_with("chats/")
            && self.allowed_collection(&format!("{conversation}/messages"))
        {
            vec![json!({"id":conversation.trim_start_matches("chats/"),"topic":""})]
        } else {
            Vec::new()
        };
        let terms: Vec<String> = projects
            .iter()
            .flat_map(|p| {
                p.to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| {
                        w.len() >= 3
                            && !["de", "del", "para", "sistema", "proyecto", "red"].contains(w)
                    })
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut jobs = tokio::task::JoinSet::new();
        let limit = Arc::new(tokio::sync::Semaphore::new(6));
        for chat in chats {
            let Some(id) = chat["id"].as_str() else {
                continue;
            };
            let collection = format!("chats/{id}/messages");
            if !self.allowed_collection(&collection) {
                continue;
            }
            let graph = self.clone();
            let terms = terms.clone();
            let limit = limit.clone();
            jobs.spawn(async move {
                let _permit = limit.acquire().await?;
                let mut url = graph.url(&collection)?;
                url.query_pairs_mut()
                    .append_pair("$top", "50")
                    .append_pair("$orderby", "lastModifiedDateTime desc")
                    .append_pair(
                        "$filter",
                        &format!("lastModifiedDateTime gt {}", since.to_rfc3339()),
                    );
                let page = match graph.request_url(Method::GET, url, None, true).await {
                    Ok(page) => page,
                    Err(_) => graph.request(Method::GET, &collection, None, true).await?,
                };
                let messages = page["value"]
                    .as_array()
                    .context("invalid Graph chat messages")?;
                let mut found = Vec::new();
                let topic = chat["topic"].as_str().unwrap_or("");
                let topic_matches = terms.iter().any(|term| topic.to_lowercase().contains(term));
                for message in messages.iter().rev() {
                    if message["messageType"].as_str() != Some("message")
                        || !message["deletedDateTime"].is_null()
                    {
                        continue;
                    }
                    let Some(date) = message["createdDateTime"].as_str() else {
                        continue;
                    };
                    if !chrono::DateTime::parse_from_rfc3339(date)
                        .is_ok_and(|d| d.with_timezone(&chrono::Utc) >= since)
                    {
                        continue;
                    }
                    let raw = message["body"]["content"].as_str().unwrap_or("");
                    let body = if message["body"]["contentType"].as_str() == Some("html") {
                        let mut body = teams::plain_text(raw);
                        let fragment = scraper::Html::parse_fragment(raw);
                        let selector = scraper::Selector::parse("a[href]").unwrap();
                        for link in fragment
                            .select(&selector)
                            .filter_map(|e| e.value().attr("href"))
                            .filter(|s| s.starts_with("https://dev.azure.com/"))
                            .take(8)
                        {
                            body.push(' ');
                            body.push_str(link);
                        }
                        body
                    } else {
                        raw.into()
                    };
                    if body.trim().is_empty() {
                        continue;
                    }
                    let sender = message["from"]["user"]["displayName"]
                        .as_str()
                        .unwrap_or("Participant");
                    let mine = message["from"]["user"]["id"].as_str()
                        == Some(graph.config.graph.user_id.as_str());
                    let metadata = crate::evidence::TeamsMessage {
                        conversation: collection.clone(),
                        message: message["id"].as_str().unwrap_or("").into(),
                        sender: message["from"]["user"]["id"].as_str().unwrap_or("").into(),
                        name: (sender != "Participant").then(|| sender.to_owned()),
                        mine,
                        date: date.into(),
                        text: body.clone(),
                    };
                    found.push((body, metadata, mine));
                }
                let project_matches = topic_matches
                    || found.iter().any(|(body, _, _)| {
                        terms.iter().any(|term| body.to_lowercase().contains(term))
                    });
                let work_terms = [
                    "desplieg",
                    "producc",
                    "stage",
                    "pipeline",
                    "reuni",
                    "coordina",
                    "gestion",
                    "gestión",
                    "incidente",
                    "revis",
                    "pase a prod",
                    "bug",
                    "deploy",
                    "production",
                    "meeting",
                    "coordinat",
                    "incident",
                    "review",
                ];
                let own_work: Vec<usize> = found
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (body, _, mine))| {
                        (*mine
                            && work_terms
                                .iter()
                                .any(|term| body.to_lowercase().contains(term)))
                        .then_some(i)
                    })
                    .collect();
                if !project_matches && own_work.is_empty() {
                    return Ok::<_, anyhow::Error>((0u8, Vec::new()));
                }
                let mut out = Vec::new();
                let matching: Vec<usize> = found
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (body, _, mine))| {
                        let lower = body.to_lowercase();
                        (terms.iter().any(|term| lower.contains(term))
                            || (topic_matches
                                && work_terms.iter().any(|term| lower.contains(term)))
                            || (*mine && work_terms.iter().any(|term| lower.contains(term))))
                        .then_some(i)
                    })
                    .collect();
                let deployment_chat = found.iter().any(|(body, _, _)| {
                    let lower = body.to_lowercase();
                    lower.contains("prod") || lower.contains("stage") || lower.contains("pase a")
                });
                let start = found.len().saturating_sub(20);
                let mut selected: Vec<_> = found
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| {
                        *i >= start && matching.iter().any(|hit| i.abs_diff(*hit) <= 1)
                    })
                    .map(|(_, message)| message)
                    .collect();
                if selected.is_empty() {
                    return Ok((0u8, Vec::new()));
                }
                let schedule = selected
                    .iter()
                    .find(|(body, _, _)| {
                        let lower = body.to_lowercase();
                        lower.contains("fecha estimada") || lower.contains("hora:")
                    })
                    .cloned();
                if selected.len() > 6 {
                    selected.drain(..selected.len() - 6);
                }
                if let Some(schedule) = schedule
                    && !selected.iter().any(|m| m.1.message == schedule.1.message)
                {
                    selected.insert(0, schedule);
                }
                for (body, mut metadata, _) in selected {
                    metadata.text = body.chars().take(400).collect();
                    out.push(metadata);
                }
                Ok((
                    if deployment_chat {
                        4
                    } else if topic_matches {
                        3
                    } else if project_matches {
                        2
                    } else {
                        1
                    },
                    out,
                ))
            });
        }
        let mut sections = Vec::new();
        while let Some(result) = jobs.join_next().await {
            if let Ok(Ok((priority, section))) = result
                && !section.is_empty()
            {
                sections.push((priority, section));
            }
        }
        sections.sort_by_key(|a| std::cmp::Reverse(a.0));
        Ok(sections
            .into_iter()
            .flat_map(|(_, section)| section)
            .take(12)
            .collect())
    }
    /// Messages the user wrote in their Teams chats since `since`, newest first and bounded,
    /// for an activity review. The personal chat is left out: it holds requests to this app.
    /// Only chats this profile may read are read, with the delegated `Chat.Read` scope.
    pub async fn own_messages(
        &self,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<(Vec<OwnMessage>, usize, bool)> {
        let mut url = self.url("me/chats")?;
        url.query_pairs_mut()
            .append_pair("$expand", "lastMessagePreview")
            .append_pair("$orderby", "lastMessagePreview/createdDateTime desc")
            .append_pair("$top", "50");
        let page = self.request_url(Method::GET, url, None, true).await?;
        let personal = self.config.graph.self_chat.as_ref().map(|c| c.id.clone());
        let chats: Vec<Value> = page["value"]
            .as_array()
            .context("invalid Graph chats")?
            .iter()
            .filter(|c| {
                matches!(
                    c["chatType"].as_str(),
                    Some("oneOnOne" | "group" | "meeting")
                )
            })
            .filter(|c| {
                c["lastMessagePreview"]["createdDateTime"]
                    .as_str()
                    .is_some_and(|date| {
                        chrono::DateTime::parse_from_rfc3339(date)
                            .is_ok_and(|d| d.with_timezone(&chrono::Utc) >= since)
                    })
            })
            .filter(|c| {
                c["id"].as_str().is_some_and(|id| {
                    personal.as_deref() != Some(id)
                        && id != "48:notes"
                        && teams::canonical_resource(&format!("chats/{id}/messages/0")).is_ok()
                        && self.allowed_collection(&format!("chats/{id}/messages"))
                })
            })
            .take(30)
            .cloned()
            .collect();
        let mut jobs = tokio::task::JoinSet::new();
        let limit = Arc::new(tokio::sync::Semaphore::new(6));
        for chat in chats {
            let graph = self.clone();
            let limit = limit.clone();
            jobs.spawn(async move {
                let _permit = limit.acquire().await?;
                let id = chat["id"].as_str().unwrap_or_default().to_owned();
                let collection = format!("chats/{id}/messages");
                let mut url = graph.url(&collection)?;
                url.query_pairs_mut()
                    .append_pair("$top", "50")
                    .append_pair("$orderby", "lastModifiedDateTime desc")
                    .append_pair(
                        "$filter",
                        &format!("lastModifiedDateTime gt {}", since.to_rfc3339()),
                    );
                let page = match graph.request_url(Method::GET, url, None, true).await {
                    Ok(page) => page,
                    Err(_) => graph.request(Method::GET, &collection, None, true).await?,
                };
                let messages = page["value"]
                    .as_array()
                    .context("invalid Graph chat messages")?;
                let mut others: Vec<String> = Vec::new();
                let mut own = Vec::new();
                for message in messages {
                    if message["messageType"].as_str() != Some("message")
                        || !message["deletedDateTime"].is_null()
                    {
                        continue;
                    }
                    let mine = message["from"]["user"]["id"].as_str()
                        == Some(graph.config.graph.user_id.as_str());
                    if !mine {
                        if let Some(name) = message["from"]["user"]["displayName"].as_str()
                            && others.len() < 3
                            && !others.iter().any(|o| o == name)
                        {
                            others.push(name.to_owned());
                        }
                        continue;
                    }
                    let Some(date) = message["createdDateTime"].as_str() else {
                        continue;
                    };
                    if !chrono::DateTime::parse_from_rfc3339(date)
                        .is_ok_and(|d| d.with_timezone(&chrono::Utc) >= since)
                    {
                        continue;
                    }
                    let raw = message["body"]["content"].as_str().unwrap_or("");
                    let text = if message["body"]["contentType"].as_str() == Some("html") {
                        teams::plain_text(raw)
                    } else {
                        raw.to_owned()
                    };
                    if text.trim().is_empty() || raw.contains("personalteams.invalid/output/") {
                        continue;
                    }
                    own.push(OwnMessage {
                        message: crate::evidence::TeamsMessage {
                            conversation: collection.clone(),
                            message: message["id"].as_str().unwrap_or("").into(),
                            sender: graph.config.graph.user_id.clone(),
                            name: message["from"]["user"]["displayName"]
                                .as_str()
                                .map(str::to_owned),
                            mine: true,
                            date: date.into(),
                            text: text.chars().take(300).collect(),
                        },
                        chat: String::new(),
                        url: message["webUrl"]
                            .as_str()
                            .filter(|u| u.starts_with("https://teams.microsoft.com/l/message/"))
                            .map(str::to_owned),
                    });
                    if own.len() >= 15 {
                        break;
                    }
                }
                let chat_name = chat["topic"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        if others.is_empty() {
                            "a chat".into()
                        } else {
                            format!("chat with {}", others.join(", "))
                        }
                    });
                for message in &mut own {
                    message.chat = chat_name.clone();
                }
                Ok::<_, anyhow::Error>(own)
            });
        }
        let mut found = Vec::new();
        let mut chats_with_messages = 0;
        let mut partial = false;
        while let Some(result) = jobs.join_next().await {
            match result {
                Ok(Ok(messages)) => {
                    chats_with_messages += usize::from(!messages.is_empty());
                    found.extend(messages);
                }
                _ => partial = true,
            }
        }
        found.sort_by(|a, b| b.message.date.cmp(&a.message.date));
        found.truncate(40);
        Ok((found, chats_with_messages, partial))
    }
    pub fn user_messages_resource(&self) -> String {
        format!("users/{}/chats/getAllMessages", self.config.graph.user_id)
    }
    /// Registration candidates from authorized chats and their call-ended events, using
    /// Chat.Read only. An event proves a call happened, never that the user attended it.
    pub async fn registration_activity(
        &self,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<crate::evidence::Evidence> {
        let mut url = self.url("me/chats")?;
        url.query_pairs_mut()
            .append_pair("$expand", "lastMessagePreview")
            .append_pair("$orderby", "lastMessagePreview/createdDateTime desc")
            .append_pair("$top", "50");
        let page = self.request_url(Method::GET, url, None, true).await?;
        let chats = page["value"].as_array().context("invalid chats")?;
        let mut partial = page["@odata.nextLink"].is_string();
        let mut activities = Vec::new();
        let mut read = 0;
        for chat in chats {
            let Some(id) = chat["id"].as_str() else {
                continue;
            };
            if id == "48:notes"
                || self
                    .config
                    .graph
                    .self_chat
                    .as_ref()
                    .is_some_and(|s| s.id == id)
                || !matches!(
                    chat["chatType"].as_str(),
                    Some("oneOnOne" | "group" | "meeting")
                )
                || !self.allowed_collection(&format!("chats/{id}/messages"))
            {
                continue;
            }
            teams::canonical_resource(&format!("chats/{id}/messages/0"))?;
            if chat["lastMessagePreview"]["createdDateTime"]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .is_some_and(|d| d < since)
            {
                continue;
            }
            if read >= 30 {
                partial = true;
                break;
            }
            read += 1;
            let collection = format!("chats/{id}/messages");
            let mut url = self.url(&collection)?;
            url.query_pairs_mut().append_pair("$top", "50");
            let reply = match self.request_url(Method::GET, url, None, true).await {
                Ok(p) => p,
                Err(_) => {
                    partial = true;
                    continue;
                }
            };
            partial |= reply["@odata.nextLink"].is_string();
            let topic = chat["topic"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("a chat without a description");
            let Some(messages) = reply["value"].as_array() else {
                partial = true;
                continue;
            };
            for message in messages {
                if let Some(a) =
                    registration_message(message, id, topic, &self.config.graph.user_id, since)
                {
                    activities.push(a);
                }
            }
        }
        activities.sort_by(|a, b| b.occurred_at.cmp(&a.occurred_at));
        partial |= activities.len() > 60;
        activities.truncate(60);
        let text = activities
            .iter()
            .map(|a| {
                format!(
                    "{} · {}{}\n",
                    a.occurred_at,
                    a.label,
                    if a.context_required {
                        " (attendance, purpose and HU need user confirmation)"
                    } else {
                        ""
                    }
                )
            })
            .collect::<String>();
        Ok(crate::evidence::Evidence {
            text,
            partial,
            links: Some(crate::ado::link::LinkOffer {
                activities,
                ..Default::default()
            }),
            ..Default::default()
        })
    }
    fn url(&self, path: &str) -> Result<url::Url> {
        ensure!(
            !path.starts_with('/') && !path.contains("://"),
            "invalid Graph path"
        );
        let mut url = url::Url::parse(&format!("{}/", self.base_url.trim_end_matches('/')))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("invalid Graph base"))?;
            segments.pop_if_empty();
            for segment in path.split('/') {
                ensure!(![".", ".."].contains(&segment), "invalid path segment");
                segments.push(segment);
            }
        }
        Ok(url)
    }
    pub fn allowed_collection(&self, resource: &str) -> bool {
        self.config
            .graph
            .self_chat
            .as_ref()
            .is_some_and(|c| resource == format!("chats/{}/messages", c.id))
            || (self.config.graph.discover_all_chats && resource == self.user_messages_resource())
            || self
                .config
                .graph
                .allowed_chats
                .iter()
                .any(|c| resource == format!("chats/{c}/messages"))
            || (self.config.graph.discover_all_chats && resource.starts_with("chats/"))
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        retry_safe: bool,
    ) -> Result<Value> {
        let scoped = if method == Method::GET && path.starts_with("chats/48:notes/messages/") {
            format!("me/{path}")
        } else {
            path.to_string()
        };
        self.request_url(method, self.url(&scoped)?, body, retry_safe)
            .await
    }
    async fn request_url(
        &self,
        method: Method,
        url: url::Url,
        body: Option<Value>,
        retry_safe: bool,
    ) -> Result<Value> {
        for attempt in 0..3 {
            let mut request = self
                .client
                .request(method.clone(), url.clone())
                .bearer_auth(self.token.access_token().await?);
            if let Some(b) = &body {
                request = request.json(b);
            }
            let response = request.send().await?;
            let status = response.status();
            if retry_safe && status.as_u16() == 401 && attempt < 2 {
                self.token.invalidate().await;
                continue;
            }
            if retry_safe && (status.as_u16() == 429 || status.is_server_error()) && attempt < 2 {
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1 << attempt)
                    .min(30);
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                continue;
            }
            if !status.is_success() {
                return Err(anyhow::Error::new(GraphHttpError(status.as_u16())));
            }
            if status.as_u16() == 204 {
                return Ok(Value::Null);
            }
            return super::bounded_json(response, 2_000_000).await;
        }
        anyhow::bail!("Graph retries exhausted")
    }
    async fn list_collection(&self, path: &str) -> Result<Vec<Value>> {
        let mut url = self.url(path)?;
        let origin = url.origin();
        let expected_path = url.path().to_owned();
        let mut values = Vec::new();
        for _ in 0..100 {
            ensure!(
                url.origin() == origin
                    && url.path() == expected_path
                    && url.username().is_empty()
                    && url.password().is_none(),
                "untrusted Graph pagination URL"
            );
            let page = self
                .request_url(Method::GET, url.clone(), None, true)
                .await?;
            values.extend(
                page["value"]
                    .as_array()
                    .context("invalid Graph collection")?
                    .iter()
                    .cloned(),
            );
            match page["@odata.nextLink"].as_str() {
                Some(next) => url = url::Url::parse(next)?,
                None => return Ok(values),
            }
        }
        anyhow::bail!("Graph pagination limit reached")
    }
    async fn list_chats(&self) -> Result<Vec<String>> {
        let mut chats = Vec::new();
        for chat in self.list_collection("me/chats").await? {
            if matches!(chat["chatType"].as_str(), Some("oneOnOne" | "group")) {
                let id = chat["id"].as_str().context("missing chat id")?;
                teams::canonical_resource(&format!("chats/{id}/messages/0"))?;
                chats.push(id.into());
            }
        }
        Ok(chats)
    }
    pub async fn reconcile_subscriptions(&self) -> Result<()> {
        let desired: Vec<_> = if self.config.graph.discover_all_chats {
            vec![self.user_messages_resource()]
        } else {
            self.config
                .graph
                .allowed_chats
                .iter()
                .map(|c| format!("chats/{c}/messages"))
                .collect()
        };
        // Reconcile server state to recover subscription creation if a response was lost.
        let remote = match self.list_collection("subscriptions").await {
            Ok(remote) => remote,
            Err(error) => {
                self.store.event(
                    "subscription",
                    &format!("list_failed_{}", subscription_error_code(&error)),
                )?;
                return Err(error);
            }
        };
        let callback = self.config.graph_callback(false);
        for s in &remote {
            let resource = s["resource"].as_str().unwrap_or("").trim_start_matches('/');
            let locally_owned = self
                .store
                .subscriptions()?
                .iter()
                .any(|local| Some(local.id.as_str()) == s["id"].as_str());
            // Graph intentionally omits clientState from GET /subscriptions. Recover only a
            // subscription for this app, user, resource, and exact callback.
            let recoverable = desired.iter().any(|r| r == resource)
                && s["notificationUrl"].as_str() == Some(&callback)
                && s["applicationId"]
                    .as_str()
                    .is_some_and(|v| v.eq_ignore_ascii_case(&self.config.graph.client_id))
                && s["creatorId"]
                    .as_str()
                    .is_some_and(|v| v.eq_ignore_ascii_case(&self.config.graph.user_id));
            if !locally_owned && !recoverable {
                continue;
            }
            let id = s["id"].as_str().context("subscription id missing")?;
            if !desired.iter().any(|r| r == resource)
                || s["notificationUrl"].as_str() != Some(&callback)
            {
                // A restarted local tunnel has a different URL: recreate only our own subscriptions.
                self.request(Method::DELETE, &format!("subscriptions/{id}"), None, true)
                    .await?;
                self.store.remove_subscription(id)?;
                continue;
            }
            if !self
                .store
                .subscriptions()?
                .iter()
                .any(|local| local.id == id)
            {
                self.persist_subscription(s, resource)?;
            }
        }
        let subscriptions = self.store.subscriptions()?;
        for s in subscriptions
            .iter()
            .filter(|s| !desired.contains(&s.resource))
        {
            self.request(
                Method::DELETE,
                &format!("subscriptions/{}", s.id),
                None,
                true,
            )
            .await?;
            self.store.remove_subscription(&s.id)?;
        }
        let mut failed = false;
        for resource in desired {
            if resource != self.user_messages_resource() {
                teams::canonical_resource(&format!("{resource}/0"))?;
            }
            let expires = chrono::Utc::now() + chrono::Duration::minutes(50);
            let existing = subscriptions.iter().find(|s| s.resource == resource);
            if existing.is_some_and(|s| s.expires_at > chrono::Utc::now().timestamp() + 600) {
                continue;
            }
            if let Some(s) = existing {
                // Expired/not-found subscriptions are removed locally; next sweep recreates them.
                if s.expires_at > 0 && s.expires_at <= chrono::Utc::now().timestamp() {
                    self.store.remove_subscription(&s.id)?;
                    continue;
                }
                let updated = self
                    .request(
                        Method::PATCH,
                        &format!("subscriptions/{}", s.id),
                        Some(json!({"expirationDateTime":expires.to_rfc3339()})),
                        true,
                    )
                    .await;
                match updated {
                    Ok(value) => self.persist_subscription(&value, &resource)?,
                    Err(error) => {
                        if error
                            .downcast_ref::<GraphHttpError>()
                            .is_some_and(|e| e.0 == 404)
                        {
                            self.store.remove_subscription(&s.id)?;
                        }
                        self.store.event(
                            "subscription",
                            &format!("renewal_failed_{}", subscription_error_code(&error)),
                        )?;
                        failed = true;
                    }
                }
            } else {
                let payload = json!({"changeType":"created","resource":resource,"notificationUrl":self.config.graph_callback(false),"lifecycleNotificationUrl":self.config.graph_callback(true),"includeResourceData":false,"expirationDateTime":expires.to_rfc3339(),"clientState":self.client_state});
                match self
                    .request(Method::POST, "subscriptions", Some(payload), false)
                    .await
                {
                    Ok(created) => self.persist_subscription(&created, &resource)?,
                    Err(error) => {
                        self.store.event(
                            "subscription",
                            &format!("creation_failed_{}", subscription_error_code(&error)),
                        )?;
                        failed = true;
                    }
                }
            }
        }
        ensure!(!failed, "some subscriptions could not be synchronized");
        Ok(())
    }
    fn persist_subscription(&self, v: &Value, resource: &str) -> Result<()> {
        self.store.save_subscription(&Subscription {
            id: v["id"].as_str().context("subscription id missing")?.into(),
            resource: resource.into(),
            expires_at: chrono::DateTime::parse_from_rfc3339(
                v["expirationDateTime"]
                    .as_str()
                    .context("expiration missing")?,
            )?
            .timestamp(),
        })
    }
    pub async fn recover_missed(&self, collection: &str) -> Result<()> {
        // Recovery is bounded to the newest page. Older gaps remain manual (silence by default).
        ensure!(
            self.allowed_collection(collection),
            "recovery collection denied"
        );
        let collections = if collection == self.user_messages_resource() {
            self.list_chats()
                .await?
                .into_iter()
                .map(|id| format!("chats/{id}/messages"))
                .collect()
        } else {
            vec![collection.to_owned()]
        };
        for collection in collections {
            let page = self.request(Method::GET, &collection, None, true).await?;
            for message in page["value"].as_array().context("invalid message page")? {
                let id = message["id"].as_str().context("message ID missing")?;
                let resource = teams::canonical_resource(&format!("{collection}/{id}"))?;
                self.store.enqueue(&resource)?;
            }
        }
        Ok(())
    }
}
#[async_trait]
impl MessageAdapter for Graph {
    async fn fetch(&self, resource: &str) -> Result<IncomingMessage> {
        let resource = teams::canonical_resource(resource)?;
        let collection = teams::collection(&resource)?;
        ensure!(
            self.allowed_collection(&collection),
            "conversation no longer allowed"
        );
        let conversation = teams::conversation(&resource)?;
        let kind = if conversation == "chats/48:notes"
            && self
                .config
                .graph
                .self_chat
                .as_ref()
                .is_some_and(|c| c.id == "48:notes")
        {
            ConversationKind::Direct
        } else {
            let chat = self.request(Method::GET, &conversation, None, true).await?;
            match chat["chatType"].as_str() {
                Some("oneOnOne") => ConversationKind::Direct,
                Some("group") => ConversationKind::Group,
                _ => ConversationKind::Unsupported,
            }
        };
        if self
            .config
            .graph
            .self_chat
            .as_ref()
            .is_some_and(|c| conversation == format!("chats/{}", c.id))
        {
            self.validate_self_chat(self.config.graph.self_chat.as_ref().unwrap().id.as_str())
                .await?;
        }
        let value = self.request(Method::GET, &resource, None, true).await?;
        let m: GraphMessage = serde_json::from_value(value)?;
        ensure!(
            resource.rsplit('/').next() == Some(&m.id),
            "message id mismatch"
        );
        let output = self
            .store
            .is_output(&conversation, &m.id, &m.body.content)?;
        let unresolved = self
            .config
            .graph
            .self_chat
            .as_ref()
            .is_some_and(|c| conversation == format!("chats/{}", c.id))
            && self.store.has_unresolved_output(&conversation)?;
        if unresolved && !output {
            self.store.event("self_chat", "paused_unknown_output")?;
        }
        let text = if m.body.content_type == "html" {
            teams::plain_text(&m.body.content)
        } else {
            m.body.content
        };
        Ok(IncomingMessage {
            resource,
            conversation,
            sender: m
                .from
                .and_then(|f| f.user)
                .map(|u| u.id)
                .unwrap_or_default(),
            kind,
            mentions: m
                .mentions
                .into_iter()
                .filter_map(|m| m.mentioned.user.map(|u| u.id))
                .collect(),
            text,
            created_at: m.created_date_time.timestamp(),
            created_at_millis: m.created_date_time.timestamp_millis(),
            is_user_message: !output
                && !unresolved
                && m.message_type == "message"
                && m.deleted_date_time.is_none(),
        })
    }
    async fn history(
        &self,
        m: &IncomingMessage,
        limit: usize,
    ) -> Result<Vec<teams::HistoryMessage>> {
        let resource = teams::canonical_resource(&m.resource)?;
        let collection = teams::collection(&resource)?;
        ensure!(
            self.allowed_collection(&collection),
            "conversation no longer allowed"
        );
        // Notes are readable only through /me, as in personal polling.
        let path = if m.conversation == "chats/48:notes" {
            "me/chats/48:notes/messages".to_owned()
        } else {
            collection
        };
        let mut url = self.url(&path)?;
        url.query_pairs_mut()
            .append_pair("$top", "50")
            .append_pair("$orderby", "createdDateTime desc");
        let page = self.request_url(Method::GET, url, None, true).await?;
        let current = resource.rsplit('/').next().unwrap_or("");
        let mut found = Vec::new();
        for value in page["value"].as_array().context("invalid message page")? {
            if found.len() >= limit {
                break;
            }
            let id = value["id"].as_str().unwrap_or("");
            let Some(created) = value["createdDateTime"]
                .as_str()
                .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
            else {
                continue;
            };
            if id.is_empty()
                || id == current
                || value["messageType"].as_str() != Some("message")
                || !value["deletedDateTime"].is_null()
                || created.timestamp_millis() >= m.created_at_millis
            {
                continue;
            }
            let content = value["body"]["content"].as_str().unwrap_or("");
            let output = self.store.is_output(&m.conversation, id, content)?;
            let text = if value["body"]["contentType"] == "html" {
                teams::plain_text(content)
            } else {
                content.to_owned()
            };
            // Answers sent by this app end with their «PTA» output marker.
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let text: String = text
                .trim_end_matches("PTA")
                .trim_end()
                .chars()
                .take(800)
                .collect();
            if text.is_empty() {
                continue;
            }
            let author = if output {
                "assistant".to_owned()
            } else if value["from"]["user"]["id"].as_str() == Some(&self.config.graph.user_id) {
                "me".to_owned()
            } else {
                value["from"]["user"]["displayName"]
                    .as_str()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or("otra persona")
                    .chars()
                    .take(80)
                    .collect()
            };
            found.push(teams::HistoryMessage {
                author,
                at: created
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
                text,
            });
        }
        found.reverse();
        Ok(found)
    }
    async fn send(&self, m: &IncomingMessage, text: &str) -> Result<String> {
        // Teams renders HTML bodies: titled links, lists and code blocks instead of raw Markdown.
        let content = teams::html(text);
        ensure!(
            content.len() <= 27_800,
            "Graph answer body exceeds the transport byte limit"
        );
        let resource = teams::canonical_resource(&m.resource)?;
        ensure!(
            self.allowed_collection(&teams::collection(&resource)?),
            "send destination denied"
        );
        let path = teams::collection(&resource)?;
        let own = self
            .config
            .graph
            .self_chat
            .as_ref()
            .is_some_and(|c| m.conversation == format!("chats/{}", c.id));
        let nonce = if own {
            Some(self.store.begin_output(&m.conversation)?)
        } else {
            None
        };
        let content = match &nonce {
            Some(nonce) => format!(
                "{content}<p><a href=\"https://personalteams.invalid/output/{nonce}\">PTA</a></p>"
            ),
            None => content,
        };
        let body = json!({"body":{"contentType":"html","content":content}});
        // Never retry a send: Graph provides no idempotency guarantee for this endpoint.
        let result = self.request(Method::POST, &path, Some(body), false).await?;
        let id = result["id"]
            .as_str()
            .context("Graph send has no message ID")?;
        if let Some(nonce) = nonce {
            self.store.finish_output(&nonce, id)?;
        }
        Ok(id.into())
    }
}

#[derive(Debug)]
struct GraphHttpError(u16);
fn subscription_error_code(error: &anyhow::Error) -> String {
    match error.downcast_ref::<GraphHttpError>() {
        Some(status) => format!("graph_http_{}", status.0),
        None => "request_failed".into(),
    }
}
impl std::fmt::Display for GraphHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Graph HTTP {}", self.0)
    }
}
impl std::error::Error for GraphHttpError {}

#[cfg(test)]
mod registration_tests {
    use super::*;

    #[test]
    fn call_without_description_needs_attendance_and_hu_context() {
        let now = chrono::Utc::now();
        let message = json!({"id":"1741000","messageType":"systemEventMessage",
            "createdDateTime":(now-chrono::Duration::minutes(5)).to_rfc3339(),
            "eventDetail":{"@odata.type":"#microsoft.graph.callEndedEventMessageDetail","callDuration":"PT1H30M"}});
        let activity = registration_message(
            &message,
            "19:meeting_abc@thread.v2",
            "a chat without a description",
            "me",
            now - chrono::Duration::days(1),
        )
        .unwrap();
        assert!(activity.context_required);
        assert_eq!(activity.duration_hours, Some(1.5));
        assert!(activity.label.contains("attendance are not verified"));
        let url = url::Url::parse(&activity.url).unwrap();
        assert_eq!(url.host_str(), Some("teams.microsoft.com"));
        assert!(activity.occurred_at.ends_with("+00:00"));
    }

    #[test]
    fn registration_messages_require_own_authorship_time_and_verified_links() {
        let now = chrono::Utc::now();
        let since = now - chrono::Duration::days(1);
        let mut message = json!({"id":"1741000","messageType":"message",
            "createdDateTime":(now-chrono::Duration::minutes(5)).to_rfc3339(),
            "from":{"user":{"id":"me"}},"body":{"contentType":"html","content":"<p>I documented the payment flow.</p>"}});
        assert!(
            registration_message(&message, "19:abc@thread.v2", "Payments", "me", since).is_some()
        );
        message["from"]["user"]["id"] = json!("someone-else");
        assert!(
            registration_message(&message, "19:abc@thread.v2", "Payments", "me", since).is_none()
        );
        message["from"]["user"]["id"] = json!("me");
        message["webUrl"] = json!("https://other.example/l/message/chat/1741000");
        assert!(
            registration_message(&message, "19:abc@thread.v2", "Payments", "me", since).is_none()
        );
        message.as_object_mut().unwrap().remove("webUrl");
        message["createdDateTime"] = json!((now - chrono::Duration::days(2)).to_rfc3339());
        assert!(
            registration_message(&message, "19:abc@thread.v2", "Payments", "me", since).is_none()
        );
        assert_eq!(call_hours("PT40M"), Some(2. / 3.));
        assert_eq!(call_hours("PT25H"), None);
    }
}
