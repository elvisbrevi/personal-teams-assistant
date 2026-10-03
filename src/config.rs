use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub graph: Graph,
    /// The `[jev]` section of profiles written before 0.6.1, when an external classifier
    /// reviewed answers. Accepted so those profiles still load, and dropped: never written.
    #[serde(default, rename = "jev", skip_serializing)]
    pub retired_reviewer: Option<serde::de::IgnoredAny>,
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
    /// Must stay empty (validated); retained so older profiles keep parsing.
    #[serde(default)]
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub self_chat: Option<SelfChat>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelfChat {
    pub id: String,
    pub user_id: String,
    pub enabled_at: i64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub team_id: String,
    pub channel_id: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Llm {
    /// Legacy single provider; used only while `chain` is empty.
    pub provider: String,
    pub model: String,
    pub style: String,
    /// Language of everything sent to Teams: the model's answer and the app's fixed texts.
    #[serde(default)]
    pub language: Language,
    /// Providers in priority order. Every model call starts again from the first enabled one
    /// and falls back to the next only when it fails (for example, without usage credits).
    #[serde(default)]
    pub chain: Vec<LlmChoice>,
}
/// Language of the replies and notices the assistant writes in Teams. The app's own interface
/// and audit log are in English.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Es,
    En,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmChoice {
    /// `codex` or `claude` (installed CLI, its own login) or `deepseek` (API key).
    pub provider: String,
    pub model: String,
    pub effort: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}
fn enabled() -> bool {
    true
}
impl Llm {
    /// Enabled providers in order; an empty chain keeps the legacy DeepSeek profile working.
    pub fn active(&self) -> Vec<LlmChoice> {
        if self.chain.is_empty() {
            return vec![LlmChoice {
                provider: self.provider.clone(),
                model: self.model.clone(),
                effort: "max".into(),
                enabled: true,
            }];
        }
        self.chain.iter().filter(|c| c.enabled).cloned().collect()
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub dry_run: bool,
    pub greeting: String,
    pub max_context_chars: usize,
    /// No longer enforced; kept so existing profiles still load.
    pub max_answer_chars: usize,
    /// No longer enforced; kept so existing profiles still load.
    #[serde(default = "default_detailed_answer_chars")]
    pub max_detailed_answer_chars: usize,
    pub max_message_age_seconds: i64,
    #[serde(default)]
    pub sensitive_patterns: Vec<String>,
    #[serde(default)]
    pub allowed_senders: Vec<String>,
}
fn default_detailed_answer_chars() -> usize {
    8000
}
impl Config {
    pub fn desktop_template() -> Result<Self> {
        Ok(toml::from_str(include_str!("../config.example.toml"))?)
    }

    /// Profiles are explicit: no process environment override may switch Entra identity.
    pub fn load(path: &str) -> Result<Self> {
        let cfg: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
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
        // Kept in the schema so existing profiles still parse; channel scopes are never requested.
        ensure!(
            self.graph.channels.is_empty(),
            "Teams channels are not supported; remove graph.channels"
        );
        if let Some(chat) = &self.graph.self_chat {
            crate::adapters::teams::canonical_resource(&format!("chats/{}/messages/0", chat.id))?;
            ensure!(
                chat.user_id == self.graph.user_id && chat.enabled_at > 0,
                "self chat account mismatch"
            );
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
        ensure!(
            (256..=32000).contains(&self.policy.max_context_chars),
            "invalid context limit"
        );
        ensure!(
            (30..=3600).contains(&self.policy.max_message_age_seconds),
            "invalid age limit"
        );
        ensure!(!self.policy.greeting.trim().is_empty(), "invalid greeting");
        crate::llm::validate(&self.llm)?;
        Ok(())
    }
    /// The delegated Graph scopes already consented for this Entra registration. Never widen.
    pub fn scopes(&self) -> &'static str {
        "offline_access User.Read Chat.Read ChatMessage.Send"
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Language};

    #[test]
    fn reply_language_defaults_to_spanish_and_accepts_english() {
        let example = include_str!("../config.example.toml");
        let parse = |text: &str| toml::from_str::<Config>(text).map(|c| c.llm.language);
        assert_eq!(parse(example).unwrap(), Language::Es);
        // Profiles written before the field existed keep answering in Spanish.
        let missing = example.replace("language = \"es\"\n", "");
        assert_ne!(missing, example);
        assert_eq!(parse(&missing).unwrap(), Language::Es);
        let english = example.replace("language = \"es\"", "language = \"en\"");
        assert_eq!(parse(&english).unwrap(), Language::En);
        assert!(parse(&example.replace("language = \"es\"", "language = \"fr\"")).is_err());
        // JSON, as `pta config set llm.language '"en"'` and the app send it.
        assert_eq!(
            serde_json::from_str::<Language>("\"en\"").unwrap(),
            Language::En
        );
        assert_eq!(serde_json::to_string(&Language::Es).unwrap(), "\"es\"");
    }

    #[test]
    fn a_retired_reviewer_section_loads_and_is_never_written_again() {
        let example = include_str!("../config.example.toml");
        assert!(!example.contains("[jev]"));
        // A profile written before 0.6.1, with its thresholds.
        let legacy = example.replace(
            "[llm]\n",
            "[jev]\nmodel = \"jev-latest\"\nfollow_up_threshold = 0.7\nrouting_threshold = 0.5\nevidence_threshold = 0.65\nfinal_threshold = 0.65\n\n[llm]\n",
        );
        assert_ne!(legacy, example);
        let config: Config = toml::from_str(&legacy).unwrap();
        config.validate().unwrap();
        let saved = toml::to_string(&config).unwrap();
        assert!(!saved.contains("jev"), "{saved}");
        assert!(toml::from_str::<Config>(&saved).is_ok());
        let json = serde_json::to_value(&config).unwrap();
        assert!(json.get("jev").is_none());
        // Other unknown sections are still rejected.
        assert!(toml::from_str::<Config>(&legacy.replace("[jev]", "[other]")).is_err());
    }
}
