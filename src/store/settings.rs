//! The settings aggregate: the profile's `Config`, its `KnowledgeMap` (the organization layer
//! included) and the installation's tunnel configuration path, which are always committed
//! together. The validation and access rules stay with their callers (`save_settings`, the
//! portable configuration, the portal); a store only keeps and returns what it was given.

use crate::app::{read_config, write_private};
use crate::{config::Config, knowledge::KnowledgeMap};
use anyhow::{Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

pub(crate) const CONFIG_FILE: &str = "config.toml";
pub(crate) const MAP_FILE: &str = "knowledge-map.toml";
pub(crate) const TUNNEL_FILE: &str = "cloudflared-path.txt";
const JOURNAL_FILE: &str = "settings-rollback.json";

pub(crate) trait SettingsStore: Send + Sync {
    fn config(&self) -> Result<Config>;
    /// The map with its initial question added when it has none (`KnowledgeMap::load`).
    fn map(&self) -> Result<KnowledgeMap>;
    /// The tunnel configuration path; empty when none was ever saved.
    fn tunnel(&self) -> Result<String>;
    /// Replace the three parts at once: after an error the previous ones are kept.
    fn commit(&self, config: &Config, map: &KnowledgeMap, tunnel: &str) -> Result<()>;
    /// Replace only the configuration (sign-in, the Microsoft setup restored from Variables).
    fn save_config(&self, config: &Config) -> Result<()>;
    /// Replace only the map (a repository cloned from GitHub).
    fn save_map(&self, map: &KnowledgeMap) -> Result<()>;
    /// Changes whenever any stored part changes, and only then.
    fn revision(&self) -> Result<String>;
}

/// The profile's files, which the Teams service, the local chat and `pta doctor --offline` read
/// directly. Writes are private temporary files renamed into place; `commit` keeps a journal
/// that `open` replays when a commit was interrupted.
pub(crate) struct LocalSettings {
    config: PathBuf,
    map: PathBuf,
    tunnel: PathBuf,
}

impl LocalSettings {
    /// The settings of the profile in `dir`, as they are: nothing is created or recovered.
    pub(crate) fn at(dir: &Path) -> Self {
        Self {
            config: dir.join(CONFIG_FILE),
            map: dir.join(MAP_FILE),
            tunnel: dir.join(TUNNEL_FILE),
        }
    }

    /// The host's own profile: a missing map or configuration is created with the defaults, and
    /// an interrupted commit is rolled back from its journal.
    pub(crate) fn open(dir: &Path, data_dir: &Path) -> Result<Self> {
        let store = Self::at(dir);
        if !store.map.exists() {
            write_private(
                &store.map,
                &toml::to_string_pretty(&KnowledgeMap {
                    repositories: BTreeMap::new(),
                    resources: Vec::new(),
                    contexts: Vec::new(),
                    cases: Vec::new(),
                    questions: None,
                    people: Vec::new(),
                    agent_prompts: Vec::new(),
                    work_item_guides: Vec::new(),
                    organization: Default::default(),
                    workflows: Vec::new(),
                    workflow_blocks: Vec::new(),
                })?,
            )?;
        }
        if !store.config.exists() {
            let mut config = Config::desktop_template()?;
            config.knowledge_map = store.map.clone();
            config.server.data_dir = data_dir.to_path_buf();
            config.server.bind = "127.0.0.1:38655".into();
            config.graph.user_id = uuid::Uuid::nil().to_string();
            write_private(&store.config, &toml::to_string_pretty(&config)?)?;
        }
        let journal = store.journal();
        if journal.exists() {
            let previous: Vec<(PathBuf, String)> = serde_json::from_slice(&fs::read(&journal)?)?;
            for (path, content) in previous {
                ensure!(store.paths().contains(&&path), "invalid recovery path");
                write_private(&path, &content)?;
            }
            fs::remove_file(journal)?;
        }
        Ok(store)
    }

    pub(crate) fn profile(&self) -> &Path {
        self.config.parent().unwrap()
    }

    /// The configuration and the knowledge map as the account store mirrors them: two records.
    /// What stays on this installation never leaves it: the `[secrets]` references and the
    /// account store link; the organization and the guides travel in their own record.
    pub(crate) fn export(&self) -> Result<Vec<crate::mirror::Exported>> {
        records(self.config()?, self.map()?)
    }

    pub(crate) fn config_path(&self) -> &Path {
        &self.config
    }
    pub(crate) fn map_path(&self) -> &Path {
        &self.map
    }

    fn paths(&self) -> [&PathBuf; 3] {
        [&self.config, &self.map, &self.tunnel]
    }
    fn journal(&self) -> PathBuf {
        self.config.with_file_name(JOURNAL_FILE)
    }
}

/// The configuration and the map exactly as this store would read them back after committing
/// them, so a record computed before a commit is the record the stores export after it.
pub(crate) fn read_back(config: &Config, map: &KnowledgeMap) -> Result<(Config, KnowledgeMap)> {
    let config: Config = toml::from_str(&toml::to_string_pretty(config)?)?;
    let mut map = KnowledgeMap::parse(&toml::to_string_pretty(map)?)?;
    map.initialize_questions();
    Ok((config, map))
}

/// The configuration's top-level sections that belong to one installation (decision 19 of the
/// cloud storage design): its listener, public URL, data directory, tunnel and webhook prefix,
/// its Microsoft identity and chats, the map's path and the knowledge index (embeddings). Besides
/// these, `policy.dry_run` and the map's repository folders are the installation's too.
pub(crate) const INSTALLATION_SECTIONS: [&str; 4] =
    ["server", "graph", "knowledge_map", "knowledge_index"];

/// The settings records of a configuration and its map (see [`LocalSettings::export`]): the
/// account's configuration and map, and, once the installation is enrolled, its own settings.
pub(crate) fn records(config: Config, map: KnowledgeMap) -> Result<Vec<crate::mirror::Exported>> {
    let installation_id = config.cloud.installation.clone();
    let mut config = serde_json::to_value(config)?;
    let mut installation = serde_json::Map::new();
    if let Some(object) = config.as_object_mut() {
        object.remove("secrets");
        object.remove("cloud");
        for section in INSTALLATION_SECTIONS {
            if let Some(value) = object.remove(section) {
                installation.insert(section.into(), value);
            }
        }
        if let Some(dry_run) = object
            .get_mut("policy")
            .and_then(Value::as_object_mut)
            .and_then(|policy| policy.remove("dry_run"))
        {
            installation.insert("dry_run".into(), dry_run);
        }
    }
    let mut map = serde_json::to_value(map)?;
    if let Some(object) = map.as_object_mut() {
        object.remove("organization");
        object.remove("work_item_guides");
        if let Some(repositories) = object.remove("repositories") {
            installation.insert("repositories".into(), repositories);
        }
    }
    let mut records = vec![
        crate::mirror::Exported {
            id: "config".into(),
            value: config,
        },
        crate::mirror::Exported {
            id: "knowledge_map".into(),
            value: map,
        },
    ];
    if !installation_id.is_empty() {
        records.push(crate::mirror::Exported {
            id: crate::mirror::installation_record(&installation_id),
            value: Value::Object(installation),
        });
    }
    Ok(records)
}

/// The account's configuration record installed over this installation's: every section but the
/// installation's own ones (and `[secrets]`, `[cloud]`, `policy.dry_run`) comes from the record,
/// so an account section the record leaves out while it holds its defaults (`message_style`,
/// `teams_reading`) returns to them here too. A record written before the split still carries
/// the installation's sections; they are ignored as well.
pub(crate) fn install_account_config(local: &Config, record: Value) -> Result<Config> {
    let local = serde_json::to_value(local)?;
    let (Some(local), Value::Object(mut merged)) = (local.as_object(), record) else {
        anyhow::bail!("the configuration record is not an object");
    };
    for section in INSTALLATION_SECTIONS.iter().chain(&["secrets", "cloud"]) {
        merged.remove(*section);
        if let Some(value) = local.get(*section) {
            merged.insert((*section).into(), value.clone());
        }
    }
    if let Some(policy) = merged.get_mut("policy").and_then(Value::as_object_mut) {
        policy.insert("dry_run".into(), local["policy"]["dry_run"].clone());
    }
    Ok(serde_json::from_value(Value::Object(merged))?)
}

impl SettingsStore for LocalSettings {
    fn config(&self) -> Result<Config> {
        read_config(&self.config)
    }
    fn map(&self) -> Result<KnowledgeMap> {
        KnowledgeMap::load(&self.map)
    }
    fn tunnel(&self) -> Result<String> {
        match fs::read_to_string(&self.tunnel) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            read => Ok(read?),
        }
    }
    fn commit(&self, config: &Config, map: &KnowledgeMap, tunnel: &str) -> Result<()> {
        let paths = self.paths();
        let previous: Vec<(PathBuf, String)> = paths
            .iter()
            .map(|p| (p.to_path_buf(), fs::read_to_string(p).unwrap_or_default()))
            .collect();
        let journal = self.journal();
        let contents = [
            toml::to_string_pretty(config)?,
            toml::to_string_pretty(map)?,
            tunnel.into(),
        ];
        write_private(&journal, &serde_json::to_string(&previous)?)?;
        for (path, content) in paths.iter().zip(contents) {
            if let Err(error) = write_private(path, &content) {
                for (path, old) in &previous {
                    write_private(path, old)?;
                }
                fs::remove_file(&journal)?;
                return Err(error);
            }
        }
        fs::remove_file(journal)?;
        crate::mirror::wake();
        Ok(())
    }
    fn save_config(&self, config: &Config) -> Result<()> {
        write_private(&self.config, &toml::to_string_pretty(config)?)?;
        crate::mirror::wake();
        Ok(())
    }
    fn save_map(&self, map: &KnowledgeMap) -> Result<()> {
        write_private(&self.map, &toml::to_string_pretty(map)?)?;
        crate::mirror::wake();
        Ok(())
    }
    fn revision(&self) -> Result<String> {
        let mut hash = Sha256::new();
        for path in self.paths() {
            hash.update(fs::read(path).unwrap_or_default());
            hash.update([0]);
        }
        Ok(format!("{:x}", hash.finalize()))
    }
}

/// What every `SettingsStore` must do, whatever keeps the data. `store` starts with any valid
/// settings and is changed by the suite.
#[cfg(test)]
pub(crate) mod contract {
    use super::*;

    fn text<T: serde::Serialize>(value: &T) -> String {
        toml::to_string(value).unwrap()
    }

    pub(crate) fn run(store: &dyn SettingsStore) {
        committed_settings_read_back(store);
        revision_follows_content(store);
        partial_saves_keep_the_other_parts(store);
    }

    fn committed_settings_read_back(store: &dyn SettingsStore) {
        let mut config = store.config().unwrap();
        config.policy.greeting = "Contract greeting".into();
        let mut map = store.map().unwrap();
        map.organization.name = "Contoso".into();
        store.commit(&config, &map, "/srv/tunnel.yml").unwrap();
        assert_eq!(text(&store.config().unwrap()), text(&config));
        assert_eq!(text(&store.map().unwrap()), text(&map));
        assert_eq!(store.tunnel().unwrap(), "/srv/tunnel.yml");
        store.commit(&config, &map, "").unwrap();
        assert_eq!(store.tunnel().unwrap(), "");
    }

    fn revision_follows_content(store: &dyn SettingsStore) {
        let (mut config, map) = (store.config().unwrap(), store.map().unwrap());
        store.commit(&config, &map, "").unwrap();
        let first = store.revision().unwrap();
        assert_eq!(store.revision().unwrap(), first, "reading changes nothing");
        store.commit(&config, &map, "").unwrap();
        assert_eq!(store.revision().unwrap(), first, "the same content");
        config.policy.greeting = "Another greeting".into();
        store.commit(&config, &map, "").unwrap();
        let second = store.revision().unwrap();
        assert_ne!(second, first, "a configuration change");
        store.commit(&config, &map, "/srv/tunnel.yml").unwrap();
        assert_ne!(store.revision().unwrap(), second, "a tunnel change");
        store.commit(&config, &map, "").unwrap();
        assert_eq!(
            store.revision().unwrap(),
            second,
            "back to the same content"
        );
    }

    fn partial_saves_keep_the_other_parts(store: &dyn SettingsStore) {
        let (mut config, mut map) = (store.config().unwrap(), store.map().unwrap());
        store.commit(&config, &map, "/srv/tunnel.yml").unwrap();
        config.policy.greeting = "Only the configuration".into();
        store.save_config(&config).unwrap();
        assert_eq!(text(&store.config().unwrap()), text(&config));
        map.organization.name = "Example".into();
        store.save_map(&map).unwrap();
        assert_eq!(text(&store.map().unwrap()), text(&map));
        assert_eq!(text(&store.config().unwrap()), text(&config));
        assert_eq!(store.tunnel().unwrap(), "/srv/tunnel.yml");
        let revision = store.revision().unwrap();
        map.organization.name = "Contoso".into();
        store.save_map(&map).unwrap();
        assert_ne!(store.revision().unwrap(), revision);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> (tempfile::TempDir, LocalSettings) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("profile");
        crate::security::private_dir(&dir).unwrap();
        let store = LocalSettings::open(&dir, &root.path().join("data")).unwrap();
        (root, store)
    }

    #[test]
    fn local_profile_meets_the_contract() {
        contract::run(&profile().1);
    }

    #[test]
    fn a_new_profile_gets_the_defaults_once() {
        let (root, store) = profile();
        let dir = store.profile().to_path_buf();
        let config = store.config().unwrap();
        assert_eq!(config.knowledge_map, dir.join(MAP_FILE));
        assert_eq!(config.server.data_dir, root.path().join("data"));
        assert_eq!(config.server.bind, "127.0.0.1:38655");
        assert_eq!(config.graph.user_id, uuid::Uuid::nil().to_string());
        assert_eq!(store.tunnel().unwrap(), "");
        let mut map = store.map().unwrap();
        assert!(
            map.questions.is_some(),
            "the initial question is added on read"
        );
        map.organization.name = "Contoso".into();
        store.save_map(&map).unwrap();
        let reopened = LocalSettings::open(&dir, &root.path().join("elsewhere")).unwrap();
        assert_eq!(reopened.map().unwrap().organization.name, "Contoso");
        assert_eq!(
            reopened.config().unwrap().server.data_dir,
            root.path().join("data")
        );
    }

    #[test]
    fn the_installations_settings_travel_in_a_record_of_their_own() {
        let (_root, store) = profile();
        let mut config = store.config().unwrap();
        let mut map = store.map().unwrap();
        map.repositories
            .insert("synthetic".into(), "/home/example/synthetic".into());
        // Not enrolled yet: only the account's two records.
        let ids = |records: &[crate::mirror::Exported]| -> Vec<String> {
            records.iter().map(|r| r.id.clone()).collect()
        };
        let plain = records(config.clone(), map.clone()).unwrap();
        assert_eq!(ids(&plain), ["config", "knowledge_map"]);
        config.cloud.installation = "00000000-0000-4000-8000-000000000001".into();
        config.policy.dry_run = true;
        let split = records(config.clone(), map.clone()).unwrap();
        assert_eq!(
            ids(&split),
            [
                "config",
                "knowledge_map",
                "installation/00000000-0000-4000-8000-000000000001"
            ]
        );
        let account = &split[0].value;
        for section in [
            "server",
            "graph",
            "knowledge_map",
            "knowledge_index",
            "secrets",
            "cloud",
        ] {
            assert!(account.get(section).is_none(), "{section}");
        }
        assert!(account["policy"].get("dry_run").is_none());
        assert!(account["llm"].is_object() && account["policy"]["greeting"].is_string());
        assert!(split[1].value.get("repositories").is_none());
        let own = &split[2].value;
        assert_eq!(own["server"]["bind"], "127.0.0.1:38655");
        assert_eq!(own["dry_run"], true);
        assert_eq!(own["repositories"]["synthetic"], "/home/example/synthetic");
    }

    #[test]
    fn the_accounts_configuration_never_replaces_the_installations_own() {
        let (_root, store) = profile();
        let local = store.config().unwrap();
        let mut other = local.clone();
        other.server.bind = "127.0.0.1:49999".into();
        other.server.public_url = "https://other.example.com".into();
        other.graph.tenant_id = uuid::Uuid::new_v4().to_string();
        other.policy.dry_run = !local.policy.dry_run;
        other.policy.greeting = "Synthetic greeting".into();
        other.message_style.label = "Synthetic label".into();
        // A record from before the split carries every section; only the account's are taken.
        let mut full = serde_json::to_value(&other).unwrap();
        full.as_object_mut().unwrap().remove("secrets");
        full.as_object_mut().unwrap().remove("cloud");
        let installed = install_account_config(&local, full).unwrap();
        assert_eq!(installed.server.bind, local.server.bind);
        assert_eq!(installed.server.public_url, local.server.public_url);
        assert_eq!(installed.graph.tenant_id, local.graph.tenant_id);
        assert_eq!(installed.policy.dry_run, local.policy.dry_run);
        assert_eq!(installed.policy.greeting, "Synthetic greeting");
        assert_eq!(installed.message_style.label, "Synthetic label");
        // The same from a record after the split.
        let split = records(other, store.map().unwrap()).unwrap();
        let installed = install_account_config(&local, split[0].value.clone()).unwrap();
        assert_eq!(installed.server.bind, local.server.bind);
        assert_eq!(installed.policy.greeting, "Synthetic greeting");

        // A section the account returned to its defaults is left out of the record, and
        // returns to them here instead of keeping this computer's former copy.
        let mut reading = local.clone();
        reading.teams_reading.all_chats = true;
        reading.graph.discover_all_chats = true;
        let account = records(local.clone(), store.map().unwrap()).unwrap();
        assert!(account[0].value.get("teams_reading").is_none());
        let installed = install_account_config(&reading, account[0].value.clone()).unwrap();
        assert!(installed.teams_reading.is_default());
        assert!(installed.graph.discover_all_chats);
        let account = records(reading.clone(), store.map().unwrap()).unwrap();
        assert_eq!(account[0].value["teams_reading"]["all_chats"], true);
        assert!(account[0].value.get("graph").is_none());
        let installed = install_account_config(&local, account[0].value.clone()).unwrap();
        assert!(installed.teams_reading.all_chats);
        assert_eq!(
            installed.graph.discover_all_chats,
            local.graph.discover_all_chats
        );
    }

    #[test]
    fn opening_rolls_back_an_interrupted_commit() {
        let (root, store) = profile();
        let dir = store.profile().to_path_buf();
        let (config, map) = (store.config().unwrap(), store.map().unwrap());
        let before = store.revision().unwrap();
        // A commit that wrote the configuration and stopped before the map and the tunnel.
        let previous: Vec<(PathBuf, String)> = store
            .paths()
            .iter()
            .map(|p| (p.to_path_buf(), fs::read_to_string(p).unwrap_or_default()))
            .collect();
        write_private(&store.journal(), &serde_json::to_string(&previous).unwrap()).unwrap();
        let mut changed = config.clone();
        changed.policy.greeting = "Half written".into();
        store.save_config(&changed).unwrap();
        let store = LocalSettings::open(&dir, &root.path().join("data")).unwrap();
        assert!(!store.journal().exists());
        assert_eq!(store.revision().unwrap(), before);
        assert_eq!(text(&store.config().unwrap()), text(&config));
        assert_eq!(text(&store.map().unwrap()), text(&map));
    }

    #[test]
    fn a_journal_cannot_write_outside_the_settings() {
        let (root, store) = profile();
        let outside = root.path().join("outside.txt");
        write_private(
            &store.journal(),
            &serde_json::to_string(&[(outside.clone(), "x".to_string())]).unwrap(),
        )
        .unwrap();
        assert!(LocalSettings::open(store.profile(), &root.path().join("data")).is_err());
        assert!(!outside.exists());
    }

    #[test]
    fn a_failed_commit_keeps_the_previous_settings() {
        let (_root, store) = profile();
        let (config, map) = (store.config().unwrap(), store.map().unwrap());
        let mut changed = config.clone();
        changed.policy.greeting = "Never saved".into();
        // The tunnel file cannot be replaced by a regular file while a directory holds its name.
        fs::create_dir(&store.tunnel).unwrap();
        assert!(store.commit(&changed, &map, "/srv/tunnel.yml").is_err());
        assert_eq!(text(&store.config().unwrap()), text(&config));
        fs::remove_dir(&store.tunnel).unwrap();
    }

    fn text<T: serde::Serialize>(value: &T) -> String {
        toml::to_string(value).unwrap()
    }
}
