pub mod graph;
pub mod oauth;
pub mod teams;
pub mod webhook;
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::de::DeserializeOwned;

#[async_trait]
pub trait MessageAdapter: Send + Sync {
    async fn fetch(&self, resource: &str) -> Result<teams::IncomingMessage>;
    async fn send(&self, message: &teams::IncomingMessage, text: &str) -> Result<String>;
    /// Up to `limit` messages sent before `message` in the same conversation, oldest first.
    async fn history(
        &self,
        _message: &teams::IncomingMessage,
        _limit: usize,
    ) -> Result<Vec<teams::HistoryMessage>> {
        Ok(Vec::new())
    }
}
pub async fn bounded_bytes(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        response.content_length().unwrap_or(0) <= limit as u64,
        "response too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(bytes.len() + chunk.len() <= limit, "response too large");
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
pub async fn bounded_json<T: DeserializeOwned>(
    response: reqwest::Response,
    limit: usize,
) -> Result<T> {
    Ok(serde_json::from_slice(
        &bounded_bytes(response, limit).await?,
    )?)
}
