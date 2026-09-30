use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub graph: Graph,
    pub jev: Jev,
    pub llm: Llm,
    pub policy: Policy,
    pub knowledge_map: PathBuf,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub bind: String,
    pub public_url: String,
    pub data_dir: PathBuf,
    #[serde(default)]
    pub cloudflare_tunnel: bool,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub tenant_id: String,
    pub client_id: String,
    pub user_id: String,
    #[serde(default)]
    pub allowed_chats: Vec<String>,
    #[serde(default)]
    pub discover_all_chats: bool,
    #[serde(default)]
    pub channels: Vec<Channel>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub team_id: String,
    pub channel_id: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Jev {
    pub model: String,
    #[serde(default = "default_follow_up_threshold")]
    pub follow_up_threshold: f64,
    pub routing_threshold: f64,
    pub evidence_threshold: f64,
    pub final_threshold: f64,
}
fn default_follow_up_threshold() -> f64 {
    0.7
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Llm {
    pub provider: String,
    pub model: String,
    pub style: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub dry_run: bool,
    pub greeting: String,
    pub max_context_chars: usize,
    pub max_answer_chars: usize,
    pub max_message_age_seconds: i64,
    #[serde(default)]
    pub sensitive_patterns: Vec<String>,
    #[serde(default)]
    pub allowed_senders: Vec<String>,
}
impl Config {
    pub fn desktop_template() -> Result<Self> {
        Ok(toml::from_str(include_str!("../config.example.toml"))?)
    }

    /// Desktop profiles are explicit; process-level overrides must not switch Entra identity.
    pub fn load_desktop(path: &str) -> Result<Self> {
        let cfg: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &str) -> Result<Self> {
        let mut cfg: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        // Only non-secret operational overrides; secrets are resolved separately.
        for (name, field) in [
            ("ENTRA_TENANT_ID", &mut cfg.graph.tenant_id),
            ("ENTRA_CLIENT_ID", &mut cfg.graph.client_id),
            ("TEAMS_USER_ID", &mut cfg.graph.user_id),
            ("PUBLIC_URL", &mut cfg.server.public_url),
            ("LLM_PROVIDER", &mut cfg.llm.provider),
            ("LLM_MODEL", &mut cfg.llm.model),
        ] {
            if let Ok(v) = std::env::var(name) {
                *field = v;
            }
        }
        cfg.validate()?;
        Ok(cfg)
    }
    pub fn validate(&self) -> Result<()> {
        for id in [
            &self.graph.tenant_id,
            &self.graph.client_id,
            &self.graph.user_id,
        ] {
            uuid::Uuid::parse_str(id)?;
        }
        let url = url::Url::parse(&self.server.public_url)?;
        ensure!(
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "public_url must be an HTTPS origin"
        );
        for t in [
            self.jev.follow_up_threshold,
            self.jev.routing_threshold,
            self.jev.evidence_threshold,
            self.jev.final_threshold,
        ] {
            ensure!(
                t.is_finite() && (0.5..=1.).contains(&t),
                "invalid confidence threshold"
            );
        }
        ensure!(
            (256..=32000).contains(&self.policy.max_context_chars),
            "invalid context limit"
        );
        ensure!(
            (1..=4000).contains(&self.policy.max_answer_chars),
            "invalid answer limit"
        );
        ensure!(
            (30..=3600).contains(&self.policy.max_message_age_seconds),
            "invalid age limit"
        );
        ensure!(
            !self.policy.greeting.trim().is_empty()
                && self.policy.greeting.chars().count() <= self.policy.max_answer_chars,
            "invalid greeting"
        );
        crate::llm::validate_provider(&self.llm.provider)?;
        Ok(())
    }
    pub fn scopes(&self) -> String {
        let mut scopes = "offline_access User.Read Chat.Read ChatMessage.Send".to_owned();
        if !self.graph.channels.is_empty() {
            scopes.push_str(" ChannelMessage.Read.All ChannelMessage.Send");
        }
        scopes
    }
}
