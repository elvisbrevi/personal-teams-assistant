//! The account's portable configuration: the positive allowlist of what follows a signed-in
//! account from one installation to another, how it is projected from a profile, restored into
//! one, compared and changed.
//!
//! Only the fields named here ever cross machines. Data and repository paths, allowed roots,
//! tunnels, control and machine credentials, internal keys, Microsoft identity (tenant, client,
//! user, chat IDs) and the Microsoft, GitHub and coding-provider sessions are *not* portable and
//! have no representation in a [`Document`], so a new field added to `Config` stays local until
//! someone adds it here deliberately. A source keeps its account meaning (ID, audiences, flags,
//! the repository *alias* and a relative path) while the alias's folder on a computer stays that
//! computer's own binding: a source whose binding does not resolve is unavailable there, never
//! reinterpreted.
use crate::{
    config::{Config, Language, LlmChoice},
    index::Note,
    knowledge::{Access, Context, KnowledgeMap, Resource},
    questions::Question,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

pub const SCHEMA: u32 = 1;
/// Notes travel in pages of this many; the largest legal note is far below the reply limit.
pub const NOTES_PAGE: usize = 8;
/// Pages read before a snapshot is refused (`NOTES_PAGE × MAX_NOTE_PAGES` notes).
pub const MAX_NOTE_PAGES: usize = 256;
pub const MAX_NOTES: usize = NOTES_PAGE * MAX_NOTE_PAGES;

/// The documented list of portable settings, shown to users and agents.
pub const SETTINGS_FIELDS: [&str; 7] = [
    "llm.language",
    "llm.style",
    "llm.chain",
    "policy.greeting",
    "policy.max_context_chars",
    "policy.max_message_age_seconds",
    "policy.sensitive_patterns",
];
/// What synchronization carries besides the settings.
pub const COLLECTIONS: [&str; 4] = ["sources", "contexts", "questions", "notes"];

/// Preferences that follow the account.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub language: Language,
    pub style: String,
    /// The account's model choices, in order. Restoring them never installs or signs in to a
    /// provider and never reorders or enables another one.
    pub chain: Vec<LlmChoice>,
    pub greeting: String,
    pub max_context_chars: usize,
    pub max_message_age_seconds: i64,
    pub sensitive_patterns: Vec<String>,
}

impl Settings {
    pub fn of(config: &Config) -> Self {
        Self {
            language: config.llm.language,
            style: config.llm.style.clone(),
            chain: config.llm.chain.clone(),
            greeting: config.policy.greeting.clone(),
            max_context_chars: config.policy.max_context_chars,
            max_message_age_seconds: config.policy.max_message_age_seconds,
            sensitive_patterns: config.policy.sensitive_patterns.clone(),
        }
    }
    /// Write the portable fields into a profile's configuration. An empty chain keeps the
    /// profile's own (a legacy single-provider profile has none to restore).
    pub fn apply(&self, config: &mut Config) {
        config.llm.language = self.language;
        config.llm.style = self.style.clone();
        if !self.chain.is_empty() {
            config.llm.chain = self.chain.clone();
        }
        config.policy.greeting = self.greeting.clone();
        config.policy.max_context_chars = self.max_context_chars;
        config.policy.max_message_age_seconds = self.max_message_age_seconds;
        config.policy.sensitive_patterns = self.sensitive_patterns.clone();
    }
    /// The existing validation rules, applied to the settings alone.
    pub fn check(&self) -> Result<()> {
        ensure!(
            self.style.chars().count() <= 20_000
                && self.greeting.chars().count() <= 2_000
                && self.sensitive_patterns.len() <= 200
                && self
                    .sensitive_patterns
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().count() <= 1_000),
            "a portable setting is too large"
        );
        let mut scratch = Config::desktop_template()?;
        self.apply(&mut scratch);
        scratch.validate()?;
        crate::security::Redactor::new(&self.sensitive_patterns, Vec::new())?;
        Ok(())
    }
}

/// Everything portable except the notes, which are paged. Collections are ordered by ID.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub schema: u32,
    pub settings: Settings,
    pub sources: Vec<Resource>,
    pub contexts: Vec<Context>,
    pub questions: Vec<Question>,
}

impl PartialEq for Resource {
    fn eq(&self, other: &Self) -> bool {
        json(self) == json(other)
    }
}

fn json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// The repository alias a source depends on, if it is bound to a folder on the computer.
pub fn alias(access: &Access) -> Option<&str> {
    match access {
        Access::File { repository, .. } | Access::Repository { repository } => Some(repository),
        Access::Tool { tool } => tool.catalog_repository(),
        _ => None,
    }
}

/// Why something from the account cannot be used on this computer.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Unavailable {
    /// Sources whose repository folder is not bound (or no longer holds the file) here.
    #[serde(default)]
    pub sources: Vec<UnavailableSource>,
    /// Questions that only read unavailable sources.
    #[serde(default)]
    pub questions: Vec<String>,
    /// Contexts and questions that lost some unavailable sources locally.
    #[serde(default)]
    pub partial: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UnavailableSource {
    pub id: String,
    /// The repository alias that has no folder on this computer.
    pub repository: String,
}
impl Unavailable {
    pub fn ids(&self) -> BTreeSet<String> {
        self.sources.iter().map(|s| s.id.clone()).collect()
    }
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.questions.is_empty() && self.partial.is_empty()
    }
}

impl Document {
    /// The portable projection of a profile: nothing else leaves it.
    pub fn project(config: &Config, map: &KnowledgeMap) -> Self {
        let mut sources = map.resources.clone();
        sources.sort_by(|a, b| a.id.cmp(&b.id));
        let mut contexts = map.contexts.clone();
        contexts.sort_by(|a, b| a.id.cmp(&b.id));
        let mut questions = map.questions.clone().unwrap_or_default();
        questions.sort_by(|a, b| a.id.cmp(&b.id));
        Self {
            schema: SCHEMA,
            settings: Settings::of(config),
            sources,
            contexts,
            questions,
        }
    }

    /// Structural validation with the knowledge map's own rules (IDs, limits, references),
    /// independent of any computer's repository folders.
    pub fn check(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "unsupported portable schema");
        self.settings.check()?;
        let mut repositories = BTreeMap::new();
        for source in &self.sources {
            if let Some(alias) = alias(&source.access) {
                repositories.insert(alias.to_owned(), PathBuf::from("unbound"));
            }
        }
        let scratch = KnowledgeMap {
            repositories,
            resources: self.sources.clone(),
            contexts: self.contexts.clone(),
            cases: Vec::new(),
            questions: Some(self.questions.clone()),
            people: Vec::new(),
        };
        KnowledgeMap::parse(&toml::to_string(&scratch)?)?;
        Ok(())
    }

    /// The revision digest of this document with the account's notes (any order).
    pub fn digest(&self, notes: &[Note]) -> String {
        let mut notes: Vec<&Note> = notes.iter().collect();
        notes.sort_by(|a, b| a.id.cmp(&b.id));
        let canonical = serde_json::to_vec(&serde_json::json!({
            "document": self,
            "notes": notes,
        }))
        .unwrap_or_default();
        let mut hash = Sha256::new();
        hash.update(b"personal-teams-assistant/portable-configuration/v1\0");
        hash.update(canonical);
        format!("{:x}", hash.finalize())
    }

    /// Install this document into a profile: the portable fields are replaced, everything
    /// local (paths, repository bindings, people, legacy cases, identity) is kept. Sources whose
    /// repository binding does not resolve here are left out and reported, and so are the
    /// questions that only read them; contexts and questions that lose some are reported as
    /// partial. Nothing is enabled, widened or defaulted.
    pub fn install(
        &self,
        config: &Config,
        map: &KnowledgeMap,
        resolves: impl Fn(&Resource) -> bool,
    ) -> (Config, KnowledgeMap, Unavailable) {
        let mut config = config.clone();
        self.settings.apply(&mut config);
        let mut unavailable = Unavailable::default();
        let mut kept = Vec::new();
        for source in &self.sources {
            if resolves(source) {
                kept.push(source.clone());
            } else {
                unavailable.sources.push(UnavailableSource {
                    id: source.id.clone(),
                    repository: alias(&source.access).unwrap_or_default().to_owned(),
                });
            }
        }
        let missing = unavailable.ids();
        let strip = |ids: &[String]| -> Vec<String> {
            ids.iter()
                .filter(|id| !missing.contains(*id))
                .cloned()
                .collect()
        };
        let contexts = self
            .contexts
            .iter()
            .map(|context| {
                let sources = strip(&context.sources);
                if sources.len() != context.sources.len() {
                    unavailable.partial.push(context.id.clone());
                }
                Context {
                    sources,
                    ..context.clone()
                }
            })
            .collect();
        let mut questions = Vec::new();
        for question in &self.questions {
            let sources = strip(&question.sources);
            if sources.is_empty() {
                unavailable.questions.push(question.id.clone());
                continue;
            }
            if sources.len() != question.sources.len() {
                unavailable.partial.push(question.id.clone());
            }
            questions.push(Question {
                sources,
                ..question.clone()
            });
        }
        let map = KnowledgeMap {
            repositories: map.repositories.clone(),
            resources: kept,
            contexts,
            cases: map.cases.clone(),
            questions: Some(questions),
            people: map.people.clone(),
        };
        (config, map, unavailable)
    }
}

/// Changes to one collection, by ID.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, bound(deserialize = "T: Deserialize<'de>"))]
pub struct Delta<T> {
    #[serde(default)]
    pub upsert: Vec<T>,
    #[serde(default)]
    pub remove: Vec<String>,
}
impl<T> Default for Delta<T> {
    fn default() -> Self {
        Self {
            upsert: Vec::new(),
            remove: Vec::new(),
        }
    }
}
impl<T> Delta<T> {
    pub fn is_empty(&self) -> bool {
        self.upsert.is_empty() && self.remove.is_empty()
    }
}

/// What a connected installation changed since the account state it loaded.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    #[serde(default)]
    pub settings: Option<Settings>,
    #[serde(default)]
    pub sources: Delta<Resource>,
    #[serde(default)]
    pub contexts: Delta<Context>,
    #[serde(default)]
    pub questions: Delta<Question>,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.settings.is_none()
            && self.sources.is_empty()
            && self.contexts.is_empty()
            && self.questions.is_empty()
    }
    /// What changes between two projections of the same installation.
    pub fn between(before: &Document, after: &Document) -> Self {
        fn delta<T: Clone + Serialize>(
            before: &[T],
            after: &[T],
            id: impl Fn(&T) -> &str,
        ) -> Delta<T> {
            let old: BTreeMap<&str, Value> = before.iter().map(|x| (id(x), json(x))).collect();
            let kept: BTreeSet<&str> = after.iter().map(&id).collect();
            Delta {
                upsert: after
                    .iter()
                    .filter(|x| old.get(id(x)) != Some(&json(x)))
                    .cloned()
                    .collect(),
                remove: old
                    .keys()
                    .filter(|k| !kept.contains(**k))
                    .map(|k| (*k).to_owned())
                    .collect(),
            }
        }
        Self {
            settings: (before.settings != after.settings).then(|| after.settings.clone()),
            sources: delta(&before.sources, &after.sources, |s| &s.id),
            contexts: delta(&before.contexts, &after.contexts, |c| &c.id),
            questions: delta(&before.questions, &after.questions, |q| &q.id),
        }
    }

    /// Apply the changes to the account's full document. `unavailable` are the sources the
    /// sender could not see: references to them survive an edit of the context or question that
    /// holds them. A change may never broaden what the assistant may read: a new source starts
    /// disabled, without audiences or external processing, and an existing one keeps (or
    /// narrows) its flags, audiences and, while enabled, its destination. Granting access is
    /// done on the account's own coordinator.
    pub fn apply_to(&self, document: &mut Document, unavailable: &BTreeSet<String>) -> Result<()> {
        ensure!(document.schema == SCHEMA, "unsupported portable schema");
        if let Some(settings) = &self.settings {
            document.settings = settings.clone();
        }
        for id in &self.sources.remove {
            ensure!(
                !unavailable.contains(id),
                "a source that is unavailable on that computer cannot be removed from there"
            );
            document.sources.retain(|s| &s.id != id);
        }
        for source in &self.sources.upsert {
            ensure!(
                !unavailable.contains(&source.id),
                "a source that is unavailable on that computer cannot be changed from there"
            );
            match document.sources.iter_mut().find(|s| s.id == source.id) {
                Some(existing) => {
                    ensure!(
                        !broadens(existing, source),
                        "Granting access to a source (enabling it, external processing, audiences or a new destination) is done on the account's own coordinator, not through synchronization."
                    );
                    *existing = source.clone();
                }
                None => {
                    ensure!(
                        !source.enabled
                            && !source.external_processing
                            && source.allowed_conversations.is_empty()
                            && source.allowed_senders.is_empty(),
                        "A new source starts disabled, without audiences or external processing."
                    );
                    document.sources.push(source.clone());
                }
            }
        }
        merge(
            &mut document.contexts,
            &self.contexts,
            unavailable,
            |c| &c.id,
            |c| &mut c.sources,
        );
        merge(
            &mut document.questions,
            &self.questions,
            unavailable,
            |q| &q.id,
            |q| &mut q.sources,
        );
        document.sources.sort_by(|a, b| a.id.cmp(&b.id));
        document.contexts.sort_by(|a, b| a.id.cmp(&b.id));
        document.questions.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(())
    }
}

fn merge<T: Clone>(
    items: &mut Vec<T>,
    delta: &Delta<T>,
    unavailable: &BTreeSet<String>,
    id: impl Fn(&T) -> &str,
    sources: impl Fn(&mut T) -> &mut Vec<String>,
) {
    items.retain(|x| !delta.remove.iter().any(|r| r == id(x)));
    for item in &delta.upsert {
        let mut item = item.clone();
        if let Some(existing) = items.iter_mut().find(|x| id(x) == id(&item)) {
            // References the sender could not see are not its to drop.
            let keep: Vec<String> = sources(existing)
                .iter()
                .filter(|s| unavailable.contains(*s))
                .cloned()
                .collect();
            sources(&mut item).extend(keep);
            *existing = item;
        } else {
            items.push(item);
        }
    }
}

/// Whether `after` lets the assistant read more than `before` does.
fn broadens(before: &Resource, after: &Resource) -> bool {
    let wider = |old: &[String], new: &[String]| {
        // An audience that was unrestricted (empty senders) and now lists senders narrows;
        // the reverse widens.
        new.iter().any(|x| !old.contains(x))
    };
    (after.enabled && !before.enabled)
        || (after.external_processing && !before.external_processing)
        || wider(&before.allowed_conversations, &after.allowed_conversations)
        || (!before.allowed_senders.is_empty() && after.allowed_senders.is_empty())
        || (!before.allowed_senders.is_empty()
            && wider(&before.allowed_senders, &after.allowed_senders))
        || (json(&before.access) != json(&after.access) && (before.enabled || after.enabled))
}

/// Validate and normalize the notes of a snapshot for a profile's contexts.
pub fn check_notes(notes: &[Note], contexts: &BTreeSet<&str>) -> Result<()> {
    ensure!(notes.len() <= MAX_NOTES, "too many notes to synchronize");
    let mut ids = BTreeSet::new();
    for note in notes {
        ensure!(
            !note.id.is_empty()
                && note.id.len() <= 64
                && note
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                && ids.insert(note.id.as_str())
                && note.created_at >= 0
                && note.updated_at >= note.created_at,
            "invalid or duplicate note ID"
        );
        note.validate(contexts)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> (Config, KnowledgeMap) {
        let config = Config::desktop_template().unwrap();
        let mut map = KnowledgeMap::parse("").unwrap();
        map.initialize_questions();
        (config, map)
    }
    fn url(id: &str) -> Resource {
        Resource {
            id: id.into(),
            description: "Synthetic page".into(),
            topics: vec![],
            enabled: true,
            external_processing: true,
            allowed_conversations: vec!["chat-1".into()],
            allowed_senders: vec![],
            access: Access::Url {
                url: "https://example.com/page".into(),
            },
        }
    }

    #[test]
    fn the_projection_carries_only_the_allowlist() {
        let (mut config, map) = profile();
        config.server.data_dir = "/private/data".into();
        config.graph.user_id = "00000000-0000-0000-0000-0000000000aa".into();
        config.graph.allowed_chats = vec!["19:secret@thread.v2".into()];
        config.secrets.insert("REF".into(), "NAME".into());
        let document = Document::project(&config, &map);
        let text = serde_json::to_string(&document).unwrap();
        for private in [
            "/private/data",
            "0000000000aa",
            "19:secret",
            "REF",
            "bind",
            "public_url",
            "tenant",
            "knowledge_map",
            "repositories",
        ] {
            assert!(!text.contains(private), "{private} leaked into {text}");
        }
        let fields: Vec<&str> = document
            .settings
            .chain
            .iter()
            .map(|c| c.provider.as_str())
            .collect();
        assert_eq!(fields, ["codex", "claude", "deepseek"]);
        // Fields cannot sneak in: the portable settings reject unknown names.
        assert!(
            serde_json::from_value::<Settings>(serde_json::json!({
                "language":"es","style":"s","chain":[],"greeting":"g","max_context_chars":1000,
                "max_message_age_seconds":60,"sensitive_patterns":[],"data_dir":"/x"
            }))
            .is_err()
        );
    }

    #[test]
    fn install_keeps_local_identity_and_reports_unresolved_sources() {
        let (config, map) = profile();
        let mut account = Document::project(&config, &map);
        account.settings.language = Language::En;
        account.settings.style = "Synthetic account style".into();
        let mut file = url("handbook");
        file.access = Access::Repository {
            repository: "handbook-repo".into(),
        };
        account.sources = vec![url("site"), file];
        account.contexts = vec![Context {
            id: "billing".into(),
            name: "Billing".into(),
            sources: vec!["site".into(), "handbook".into()],
            ..Default::default()
        }];
        account.questions = vec![
            Question {
                id: "only-local-repo".into(),
                text: "Where is the handbook".into(),
                sources: vec!["handbook".into()],
                examples: vec![],
            },
            Question {
                id: "both".into(),
                text: "What is billing".into(),
                sources: vec!["site".into(), "handbook".into()],
                examples: vec![],
            },
        ];
        account.check().unwrap();

        let mut local = config.clone();
        local.server.data_dir = "/this/computer/data".into();
        local.graph.user_id = "00000000-0000-0000-0000-0000000000bb".into();
        let (installed, installed_map, unavailable) =
            account.install(&local, &map, |r| alias(&r.access).is_none());
        assert_eq!(installed.llm.language, Language::En);
        assert_eq!(installed.llm.style, "Synthetic account style");
        assert_eq!(
            installed.server.data_dir,
            PathBuf::from("/this/computer/data")
        );
        assert_eq!(installed.graph.user_id, local.graph.user_id);
        assert_eq!(installed_map.resources.len(), 1);
        // The audience and flags of the kept source are exactly the account's.
        assert!(installed_map.resources[0].enabled);
        assert_eq!(
            installed_map.resources[0].allowed_conversations,
            ["chat-1".to_owned()]
        );
        assert_eq!(unavailable.sources[0].id, "handbook");
        assert_eq!(unavailable.sources[0].repository, "handbook-repo");
        assert_eq!(unavailable.questions, ["only-local-repo"]);
        assert_eq!(unavailable.partial, ["billing", "both"]);
        assert_eq!(installed_map.contexts[0].sources, ["site"]);
        assert_eq!(installed_map.questions.as_ref().unwrap().len(), 1);
        KnowledgeMap::parse(&toml::to_string(&installed_map).unwrap()).unwrap();
    }

    #[test]
    fn a_patch_never_widens_access_or_drops_what_its_sender_could_not_see() {
        let (config, map) = profile();
        let mut account = Document::project(&config, &map);
        let mut narrow = url("site");
        narrow.enabled = false;
        narrow.external_processing = false;
        narrow.allowed_conversations = vec![];
        let mut hidden = url("hidden");
        hidden.access = Access::Repository {
            repository: "elsewhere".into(),
        };
        account.sources = vec![narrow.clone(), hidden.clone(), url("live")];
        account.contexts = vec![Context {
            id: "billing".into(),
            name: "Billing".into(),
            sources: vec!["live".into(), "hidden".into()],
            ..Default::default()
        }];
        account.questions = vec![];
        account.check().unwrap();
        let unavailable = BTreeSet::from(["hidden".to_owned()]);

        // Enabling, widening the audience or adding an enabled source is refused.
        for change in [
            |s: &mut Resource| s.enabled = true,
            |s: &mut Resource| s.external_processing = true,
            |s: &mut Resource| s.allowed_conversations.push("*".into()),
        ] {
            let mut widened = narrow.clone();
            change(&mut widened);
            let mut patch = Patch::default();
            patch.sources.upsert = vec![widened];
            assert!(patch.apply_to(&mut account.clone(), &unavailable).is_err());
        }
        let mut patch = Patch::default();
        patch.sources.upsert = vec![url("fresh")];
        assert!(patch.apply_to(&mut account.clone(), &unavailable).is_err());
        let mut disabled = url("fresh");
        disabled.enabled = false;
        disabled.external_processing = false;
        disabled.allowed_conversations = vec![];
        patch.sources.upsert = vec![disabled];
        let mut next = account.clone();
        patch.apply_to(&mut next, &unavailable).unwrap();
        assert!(next.sources.iter().any(|s| s.id == "fresh"));

        // Narrowing is allowed; an unavailable source cannot be edited or removed from there.
        let mut live = url("live");
        live.allowed_conversations = vec![];
        live.enabled = false;
        let mut patch = Patch::default();
        patch.sources.upsert = vec![live];
        let mut next = account.clone();
        patch.apply_to(&mut next, &unavailable).unwrap();
        assert!(
            !next
                .sources
                .iter()
                .find(|s| s.id == "live")
                .unwrap()
                .enabled
        );
        let mut patch = Patch::default();
        patch.sources.remove = vec!["hidden".into()];
        assert!(patch.apply_to(&mut account.clone(), &unavailable).is_err());

        // Editing the context keeps the reference to the source its sender could not see.
        let mut patch = Patch::default();
        patch.contexts.upsert = vec![Context {
            id: "billing".into(),
            name: "Billing renamed".into(),
            sources: vec!["live".into()],
            ..Default::default()
        }];
        let mut next = account.clone();
        patch.apply_to(&mut next, &unavailable).unwrap();
        assert_eq!(next.contexts[0].name, "Billing renamed");
        assert_eq!(next.contexts[0].sources, ["live", "hidden"]);
        next.check().unwrap();
    }

    #[test]
    fn a_diff_applied_to_the_before_state_gives_the_after_state() {
        let (config, map) = profile();
        let before = Document::project(&config, &map);
        let mut after = before.clone();
        after.settings.greeting = "Hola, synthetic".into();
        after.contexts.push(Context {
            id: "ops".into(),
            name: "Operations".into(),
            ..Default::default()
        });
        after.questions[0].examples.push("Qué hice hoy".into());
        let patch = Patch::between(&before, &after);
        assert!(!patch.is_empty() && patch.sources.is_empty());
        let mut applied = before.clone();
        patch.apply_to(&mut applied, &BTreeSet::new()).unwrap();
        assert_eq!(applied, after);
        assert!(Patch::between(&after, &after).is_empty());
        assert_ne!(before.digest(&[]), after.digest(&[]));
        assert_eq!(after.digest(&[]), applied.digest(&[]));
    }
}
