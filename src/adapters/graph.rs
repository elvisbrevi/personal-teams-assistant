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

pub struct Graph {
    pub client: reqwest::Client,
    pub token: Arc<dyn AccessToken>,
    pub base_url: String,
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub client_state: String,
}
impl Graph {
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
        self.request_url(method, self.url(path)?, body, retry_safe)
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
        let mut chats = self.config.graph.allowed_chats.clone();
        if self.config.graph.discover_all_chats {
            chats.extend(self.list_chats().await?);
        }
        chats.sort();
        chats.dedup();
        let mut desired: Vec<_> = chats
            .into_iter()
            .map(|c| format!("chats/{c}/messages"))
            .collect();
        desired.extend(
            self.config
                .graph
                .channels
                .iter()
                .map(|c| format!("teams/{}/channels/{}/messages", c.team_id, c.channel_id)),
        );
        // Reconcile server state to recover subscription creation if a response was lost.
        let remote = self.list_collection("subscriptions").await?;
        let callback = format!(
            "{}/graph/notifications",
            self.config.server.public_url.trim_end_matches('/')
        );
        for s in &remote {
            let resource = s["resource"].as_str().unwrap_or("").trim_start_matches('/');
            let owned = s["clientState"]
                .as_str()
                .is_some_and(|v| crate::security::constant_eq(v, &self.client_state));
            if !owned {
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
            teams::canonical_resource(&format!("{resource}/0"))?;
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
                        self.store.event("subscription", "renewal_failed")?;
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
                    Err(_) => {
                        self.store.event("subscription", "creation_failed")?;
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
        let page = self.request(Method::GET, collection, None, true).await?;
        for message in page["value"].as_array().context("invalid message page")? {
            let id = message["id"].as_str().context("message ID missing")?;
            let resource = teams::canonical_resource(&format!("{collection}/{id}"))?;
            self.store.enqueue(&resource)?;
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
        let kind = if conversation.starts_with("chats/") {
            let chat = self.request(Method::GET, &conversation, None, true).await?;
            match chat["chatType"].as_str() {
                Some("oneOnOne") => ConversationKind::Direct,
                Some("group") => ConversationKind::Group,
                _ => ConversationKind::Unsupported,
            }
        } else {
            ConversationKind::Channel
        };
        let value = self.request(Method::GET, &resource, None, true).await?;
        let m: GraphMessage = serde_json::from_value(value)?;
        ensure!(
            resource.rsplit('/').next() == Some(&m.id),
            "message id mismatch"
        );
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
            is_user_message: m.message_type == "message" && m.deleted_date_time.is_none(),
        })
    }
    async fn send(&self, m: &IncomingMessage, text: &str) -> Result<String> {
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
        // Never retry a send: Graph provides no idempotency guarantee for this endpoint.
        let result = self
            .request(
                Method::POST,
                &path,
                Some(json!({"body":{"contentType":"text","content":text}})),
                false,
            )
            .await?;
        Ok(result["id"]
            .as_str()
            .context("Graph send has no message ID")?
            .into())
    }
}

#[derive(Debug)]
struct GraphHttpError(u16);
impl std::fmt::Display for GraphHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Graph HTTP {}", self.0)
    }
}
impl std::error::Error for GraphHttpError {}
