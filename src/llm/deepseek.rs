use super::{Backend, Failure, Unavailable};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

/// The response also carries the reasoning, which can reach the provider's token default.
const RESPONSE_LIMIT: usize = 16_000_000;

/// DeepSeek over its HTTP API (`/chat/completions`), without an SDK.
pub struct DeepSeek {
    client: reqwest::Client,
    url: String,
    key: String,
    model: String,
    effort: String,
    label: String,
}
impl DeepSeek {
    pub fn new(key: &str, model: &str, effort: &str, endpoint: &str) -> Result<Self> {
        // No overall deadline: maximum-effort reasoning takes as long as it takes, and the
        // pipeline tells the sender when it is slow. Only connecting is bounded, and
        // keepalive detects a dead connection.
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            url: format!("{}/chat/completions", endpoint.trim_end_matches('/')),
            key: key.into(),
            model: model.into(),
            effort: effort.into(),
            label: format!("deepseek:{model}:{effort}"),
        })
    }
}
#[async_trait]
impl Backend for DeepSeek {
    fn label(&self) -> &str {
        &self.label
    }
    /// One JSON-mode chat completion with the configured reasoning effort and the provider's
    /// default token limit. Returns only the final content, never the reasoning.
    async fn complete_json(&self, system: &str, user: &str, _: &Value) -> Result<String> {
        #[derive(Deserialize)]
        struct Completion {
            choices: Vec<Choice>,
        }
        #[derive(Deserialize)]
        struct Choice {
            message: Message,
            finish_reason: Option<String>,
        }
        #[derive(Deserialize)]
        struct Message {
            content: Option<String>,
        }
        let response = self
            .client
            .post(&self.url)
            .bearer_auth(&self.key)
            .json(&request_body(&self.model, &self.effort, system, user))
            .send()
            .await?;
        // 402 Insufficient Balance and 429 Rate Limit hand the call to the next provider.
        if matches!(response.status().as_u16(), 402 | 429) {
            return Err(Unavailable(Failure::UsageLimit).into());
        }
        anyhow::ensure!(response.status().is_success(), "LLM request was rejected");
        let completion: Completion =
            crate::adapters::bounded_json(response, RESPONSE_LIMIT).await?;
        let choice = completion
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("LLM returned no choice"))?;
        anyhow::ensure!(
            choice.finish_reason.as_deref() == Some("stop"),
            "LLM did not finish its answer"
        );
        choice
            .message
            .content
            .filter(|content| !content.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("LLM returned an empty answer"))
    }
}
/// `none` disables thinking; any other effort enables it at that level.
fn request_body(model: &str, effort: &str, system: &str, user: &str) -> Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "thinking": {"type": if effort == "none" { "disabled" } else { "enabled" }},
        "reasoning_effort": effort,
        "response_format": {"type": "json_object"},
        "stream": false,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_use_configured_reasoning_without_token_cap() {
        let body = request_body("deepseek-flash", "max", "system", "user");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "max");
        assert_eq!(body["response_format"]["type"], "json_object");
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("temperature").is_none());
        let body = request_body("deepseek-flash", "none", "system", "user");
        assert_eq!(body["thinking"]["type"], "disabled");
    }
}
