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

#[derive(Clone)]
pub struct Graph {
    pub client: reqwest::Client,
    pub token: Arc<dyn AccessToken>,
    pub base_url: String,
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub client_state: String,
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
                        .unwrap_or("Participante");
                    let mine = message["from"]["user"]["id"].as_str()
                        == Some(graph.config.graph.user_id.as_str());
                    let metadata = crate::evidence::TeamsMessage {
                        conversation: collection.clone(),
                        message: message["id"].as_str().unwrap_or("").into(),
                        sender: message["from"]["user"]["id"].as_str().unwrap_or("").into(),
                        name: (sender != "Participante").then(|| sender.to_owned()),
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
    pub fn user_messages_resource(&self) -> String {
        format!("users/{}/chats/getAllMessages", self.config.graph.user_id)
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
            || self.config.graph.channels.iter().any(|c| {
                resource == format!("teams/{}/channels/{}/messages", c.team_id, c.channel_id)
            })
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
        let mut desired: Vec<_> = if self.config.graph.discover_all_chats {
            vec![self.user_messages_resource()]
        } else {
            self.config
                .graph
                .allowed_chats
                .iter()
                .map(|c| format!("chats/{c}/messages"))
                .collect()
        };
        desired.extend(
            self.config
                .graph
                .channels
                .iter()
                .map(|c| format!("teams/{}/channels/{}/messages", c.team_id, c.channel_id)),
        );
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
        let callback = format!(
            "{}/graph/notifications",
            self.config.server.public_url.trim_end_matches('/')
        );
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
                let payload = json!({"changeType":"created","resource":resource,"notificationUrl":format!("{}/graph/notifications",self.config.server.public_url.trim_end_matches('/')),"lifecycleNotificationUrl":format!("{}/graph/lifecycle",self.config.server.public_url.trim_end_matches('/')),"includeResourceData":false,"expirationDateTime":expires.to_rfc3339(),"clientState":self.client_state});
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
        } else if conversation.starts_with("chats/") {
            let chat = self.request(Method::GET, &conversation, None, true).await?;
            match chat["chatType"].as_str() {
                Some("oneOnOne") => ConversationKind::Direct,
                Some("group") => ConversationKind::Group,
                _ => ConversationKind::Unsupported,
            }
        } else {
            ConversationKind::Channel
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
    async fn send(&self, m: &IncomingMessage, text: &str) -> Result<String> {
        ensure!(
            text.len() <= 28_000,
            "Graph answer body exceeds the transport byte limit"
        );
        let resource = teams::canonical_resource(&m.resource)?;
        ensure!(
            self.allowed_collection(&teams::collection(&resource)?),
            "send destination denied"
        );
        let path = if m.kind == ConversationKind::Channel {
            let root = resource.split("/replies/").next().unwrap();
            format!("{root}/replies")
        } else {
            teams::collection(&resource)?
        };
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
        let body = if let Some(nonce) = &nonce {
            let escaped = text
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('\n', "<br>");
            json!({"body":{"contentType":"html","content":format!("<p>{escaped}</p><p><a href=\"https://personalteams.invalid/output/{nonce}\">PTA</a></p>")}})
        } else {
            json!({"body":{"contentType":"text","content":text}})
        };
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
