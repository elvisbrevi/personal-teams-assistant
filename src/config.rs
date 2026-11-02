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
    #[serde(default)]
    pub activity_registration: crate::activity::Settings,
    /// The knowledge index: optional embeddings for semantic search.
    #[serde(default, skip_serializing_if = "crate::index::Settings::is_default")]
    pub knowledge_index: crate::index::Settings,
    /// How answers look in Teams; omitted while it is the default (plain answers), so a host
    /// that predates it still loads the profile.
    #[serde(default, skip_serializing_if = "MessageStyle::is_default")]
    pub message_style: MessageStyle,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub bind: String,
    pub public_url: String,
    pub data_dir: PathBuf,
    #[serde(default)]
    pub cloudflare_tunnel: bool,
    /// Optional path reserved by the web portal for this profile's Graph callbacks.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub webhook_prefix: String,
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
    /// Language of what the user reads: the app's interface, Teams replies and fixed texts, and
    /// the activity drafts and notices written for them.
    #[serde(default)]
    pub language: Language,
    /// Providers in priority order. Every model call starts again from the first enabled one
    /// and falls back to the next only when it fails (for example, without usage credits).
    #[serde(default)]
    pub chain: Vec<LlmChoice>,
}
/// Language of what the user reads: the app's interface (translated from its English source),
/// Teams replies and notices, and activity drafts. The CLI, audit log and diagnostics stay in
/// English.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Es,
    En,
}
impl Language {
    /// The text written for the user in this language.
    pub fn say(self, english: impl Into<String>, spanish: impl Into<String>) -> String {
        match self {
            Self::Es => spanish.into(),
            Self::En => english.into(),
        }
    }
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
/// How the assistant's messages look in Teams (`[message_style]`): off, they are plain chat
/// HTML; on, they are an Adaptive Card with a colored stripe along the left edge and a band
/// with a label above or below the text (`adapters::card`). Teams ignores styles in chat HTML,
/// so a card is the only way to color a message.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct MessageStyle {
    pub enabled: bool,
    /// The band's text; empty means «Respuesta generada con IA» or "AI-generated response",
    /// following `llm.language`.
    pub label: String,
    pub position: BandPosition,
    /// The stripe's color, `#RRGGBB`. Any color: the stripe carries no text.
    pub accent: String,
    /// The band's background, by Teams' name: Teams picks the shade for its light or dark
    /// theme, so the label stays readable in both.
    pub band: BandColor,
    pub label_color: LabelColor,
    /// A robot icon before the label.
    pub icon: bool,
    /// The stripe's width in pixels, 2 to 16.
    pub stripe_width: u8,
    /// The size of the message's text; headings stay a step larger.
    pub text_size: TextSize,
    /// The size of the band's text and icon.
    pub label_size: TextSize,
}
impl Default for MessageStyle {
    fn default() -> Self {
        Self {
            enabled: false,
            label: String::new(),
            position: BandPosition::Bottom,
            accent: "#5B5FC7".into(),
            band: BandColor::Accent,
            label_color: LabelColor::Default,
            icon: true,
            stripe_width: 4,
            text_size: TextSize::Default,
            label_size: TextSize::Small,
        }
    }
}
impl MessageStyle {
    pub const MAX_LABEL_CHARS: usize = 80;
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.label.chars().count() <= Self::MAX_LABEL_CHARS
                && !self.label.chars().any(char::is_control),
            "the message style label is one line of up to 80 characters"
        );
        ensure!(
            self.accent.len() == 7
                && self.accent.starts_with('#')
                && self.accent[1..].chars().all(|c| c.is_ascii_hexdigit()),
            "the message style accent is a #RRGGBB color"
        );
        ensure!(
            (2..=16).contains(&self.stripe_width),
            "the message style stripe is 2 to 16 pixels wide"
        );
        Ok(())
    }
    /// The band's text: the configured one or the default in the reply language.
    pub fn label(&self, language: Language) -> String {
        match self.label.trim() {
            "" => language.say("AI-generated response", "Respuesta generada con IA"),
            label => label.to_owned(),
        }
    }
}
/// Adaptive Card text sizes; Teams sets the actual size for each client.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TextSize {
    Small,
    #[default]
    Default,
    Medium,
    Large,
    ExtraLarge,
}
impl TextSize {
    /// The Adaptive Card value.
    pub fn card(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Default => "Default",
            Self::Medium => "Medium",
            Self::Large => "Large",
            Self::ExtraLarge => "ExtraLarge",
        }
    }
    /// One size larger, up to the largest.
    pub fn larger(self) -> Self {
        match self {
            Self::Small => Self::Default,
            Self::Default => Self::Medium,
            Self::Medium => Self::Large,
            Self::Large | Self::ExtraLarge => Self::ExtraLarge,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BandPosition {
    Top,
    #[default]
    Bottom,
}
/// Adaptive Card container styles; `none` leaves the band on the card's own background.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BandColor {
    #[default]
    Accent,
    Emphasis,
    Good,
    Warning,
    Attention,
    None,
}
/// Adaptive Card text colors; `subtle` is the default color, dimmed.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LabelColor {
    #[default]
    Default,
    Subtle,
    Accent,
    Good,
    Warning,
    Attention,
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
            self.server.webhook_prefix.is_empty()
                || self
                    .server
                    .webhook_prefix
                    .strip_prefix("/webhooks/")
                    .is_some_and(|id| {
                        uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id)
                    }),
            "invalid webhook prefix"
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
        self.activity_registration.validate()?;
        self.knowledge_index.validate()?;
        self.message_style.validate()?;
        if let Some(reference) = self
            .knowledge_index
            .embeddings
            .as_ref()
            .and_then(|e| e.secret_ref.as_ref())
        {
            ensure!(
                self.secrets.contains_key(reference),
                "map the embeddings credential reference in [secrets]"
            );
        }
        Ok(())
    }
    /// The delegated Graph scopes already consented for this Entra registration. Never widen.
    pub fn scopes(&self) -> &'static str {
        "offline_access User.Read Chat.Read ChatMessage.Send"
    }

    pub fn graph_callback(&self, lifecycle: bool) -> String {
        format!(
            "{}{}/graph/{}",
            self.server.public_url.trim_end_matches('/'),
            self.server.webhook_prefix,
            if lifecycle {
                "lifecycle"
            } else {
                "notifications"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Language};

    #[test]
    fn graph_callbacks_keep_existing_urls_and_isolate_web_profiles() {
        let mut config = Config::desktop_template().unwrap();
        assert_eq!(
            config.graph_callback(false),
            "https://assistant.example.com/graph/notifications"
        );
        assert_eq!(
            config.graph_callback(true),
            "https://assistant.example.com/graph/lifecycle"
        );
        config.server.webhook_prefix = "/webhooks/00000000-0000-0000-0000-000000000001".into();
        config.validate().unwrap();
        assert_eq!(
            config.graph_callback(false),
            "https://assistant.example.com/webhooks/00000000-0000-0000-0000-000000000001/graph/notifications"
        );
        for invalid in [
            "/other",
            "/webhooks/../alice",
            "/webhooks/a?token=b",
            "/webhooks/00000000-0000-0000-0000-000000000001/extra",
        ] {
            config.server.webhook_prefix = invalid.into();
            assert!(config.validate().is_err());
        }
    }

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
    fn message_style_is_omitted_while_default_and_validated() {
        let example = include_str!("../config.example.toml");
        let mut config: Config = toml::from_str(example).unwrap();
        // Hosts that predate the section keep loading a profile that never changed it.
        assert!(!toml::to_string(&config).unwrap().contains("message_style"));
        config.message_style.enabled = true;
        config.message_style.position = super::BandPosition::Top;
        let saved = toml::to_string(&config).unwrap();
        assert!(saved.contains("[message_style]"), "{saved}");
        let loaded: Config = toml::from_str(&saved).unwrap();
        assert_eq!(loaded.message_style, config.message_style);
        loaded.validate().unwrap();
        // A partial section takes the defaults for the rest.
        let partial: Config =
            toml::from_str(&format!("{example}\n[message_style]\nenabled = true\n")).unwrap();
        assert_eq!(partial.message_style.accent, "#5B5FC7");
        assert_eq!(
            partial.message_style.label(Language::Es),
            "Respuesta generada con IA"
        );
        let sized: Config = toml::from_str(&format!(
            "{example}\n[message_style]\nstripe_width = 8\ntext_size = \"extralarge\"\nlabel_size = \"medium\"\n"
        ))
        .unwrap();
        sized.validate().unwrap();
        assert_eq!(sized.message_style.text_size, super::TextSize::ExtraLarge);
        for width in [1, 17] {
            let mut wrong = sized.clone();
            wrong.message_style.stripe_width = width;
            assert!(wrong.validate().is_err(), "{width}");
        }
        for (label, accent) in [
            ("a\nb", "#5B5FC7"),
            (&"x".repeat(81) as &str, "#5B5FC7"),
            ("", "5B5FC7"),
            ("", "#5B5FCZ"),
            ("", "#5B5FC77"),
        ] {
            config.message_style.label = label.into();
            config.message_style.accent = accent.into();
            assert!(config.validate().is_err(), "{label:?} {accent:?}");
        }
        assert!(
            toml::from_str::<Config>(&format!(
                "{example}\n[message_style]\nposition = \"left\"\n"
            ))
            .is_err()
        );
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
