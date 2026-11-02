//! The organization aggregate: the account's organization layer (its display name, entries and
//! documenting sources) together with its work item guides, read and written as one unit. Each
//! entry is a document identified by its `id` (its `updated_at` is its revision), each guide by
//! its `id` (its content is its revision); the whole has one revision. The validation, the
//! entries' `updated_at` and the permissions stay with the callers (`save_settings`,
//! `organization::validate`, `work_items::validate`); a store only keeps and returns what it was
//! given.
//!
//! The settings editors (GUI, CLI and MCP) still save the organization with the rest of the map
//! in one settings commit; the local implementation keeps it in the map, so both paths meet the
//! same files.

use crate::{
    organization::{Entry, Organization},
    store::settings::SettingsStore,
    work_items::Guide,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The organization and its work item guides.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct OrganizationRecord {
    pub organization: Organization,
    pub guides: Vec<Guide>,
}

pub trait OrganizationStore: Send + Sync {
    fn organization(&self) -> Result<OrganizationRecord>;
    /// Replace the organization and its guides at once.
    fn replace(&self, record: &OrganizationRecord) -> Result<()>;
    /// Replace the entry with the same `id`, or add it last.
    fn save_entry(&self, entry: &Entry) -> Result<()>;
    /// Whether there was an entry to remove.
    fn remove_entry(&self, id: &str) -> Result<bool>;
    /// Replace the guide with the same `id`, or add it last.
    fn save_guide(&self, guide: &Guide) -> Result<()>;
    /// Whether there was a guide to remove.
    fn remove_guide(&self, id: &str) -> Result<bool>;
    /// Changes whenever the organization or a guide changes, and only then.
    fn revision(&self) -> Result<String>;
}

/// The `[organization]` and `[[work_item_guides]]` sections of the profile's knowledge map,
/// written with the settings store's journaled commit; no file or format changes.
pub(crate) struct LocalOrganization<'a> {
    settings: &'a dyn SettingsStore,
}

impl<'a> LocalOrganization<'a> {
    pub(crate) fn new(settings: &'a dyn SettingsStore) -> Self {
        Self { settings }
    }

    /// Change the map's organization and guides and commit them with the current
    /// configuration and tunnel path.
    fn change<T>(&self, change: impl FnOnce(&mut OrganizationRecord) -> T) -> Result<T> {
        let config = self.settings.config()?;
        let mut map = self.settings.map()?;
        let tunnel = self.settings.tunnel()?;
        let mut record = OrganizationRecord {
            organization: std::mem::take(&mut map.organization),
            guides: std::mem::take(&mut map.work_item_guides),
        };
        let out = change(&mut record);
        map.organization = record.organization;
        map.work_item_guides = record.guides;
        self.settings.commit(&config, &map, &tunnel)?;
        Ok(out)
    }
}

impl OrganizationStore for LocalOrganization<'_> {
    fn organization(&self) -> Result<OrganizationRecord> {
        let map = self.settings.map()?;
        Ok(OrganizationRecord {
            organization: map.organization,
            guides: map.work_item_guides,
        })
    }

    fn replace(&self, record: &OrganizationRecord) -> Result<()> {
        self.change(|held| *held = record.clone())
    }

    fn save_entry(&self, entry: &Entry) -> Result<()> {
        self.change(|record| {
            let entries = &mut record.organization.entries;
            match entries.iter_mut().find(|e| e.id == entry.id) {
                Some(held) => *held = entry.clone(),
                None => entries.push(entry.clone()),
            }
        })
    }

    fn remove_entry(&self, id: &str) -> Result<bool> {
        if !self
            .organization()?
            .organization
            .entries
            .iter()
            .any(|e| e.id == id)
        {
            return Ok(false);
        }
        self.change(|record| record.organization.entries.retain(|e| e.id != id))?;
        Ok(true)
    }

    fn save_guide(&self, guide: &Guide) -> Result<()> {
        self.change(
            |record| match record.guides.iter_mut().find(|g| g.id == guide.id) {
                Some(held) => *held = guide.clone(),
                None => record.guides.push(guide.clone()),
            },
        )
    }

    fn remove_guide(&self, id: &str) -> Result<bool> {
        if !self.organization()?.guides.iter().any(|g| g.id == id) {
            return Ok(false);
        }
        self.change(|record| record.guides.retain(|g| g.id != id))?;
        Ok(true)
    }

    fn revision(&self) -> Result<String> {
        let record = serde_json::to_vec(&self.organization()?)?;
        Ok(format!("{:x}", Sha256::digest(record)))
    }
}

/// What every `OrganizationStore` must do, whatever keeps the records. `store` starts with no
/// organization and no guides and is changed by the suite.
#[cfg(test)]
pub(crate) mod contract {
    use super::*;
    use crate::organization::Kind;

    pub(crate) fn run(store: &dyn OrganizationStore) {
        the_whole_is_replaced_at_once(store);
        entries_are_saved_by_id_and_removed(store);
        guides_are_saved_by_id_and_removed(store);
    }

    pub(crate) fn entry(id: &str, text: &str) -> Entry {
        Entry {
            id: id.into(),
            kind: Kind::Norm,
            title: format!("Title of {id}"),
            text: text.into(),
            projects: vec!["Payments".into()],
            updated_at: 1,
        }
    }

    pub(crate) fn guide(id: &str, purpose: &str) -> Guide {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "provider": "azure",
            "type": "Task",
            "name": format!("Guide {id}"),
            "purpose": purpose,
            "instructions": "Title: the concrete action.",
        }))
        .unwrap()
    }

    fn the_whole_is_replaced_at_once(store: &dyn OrganizationStore) {
        assert_eq!(store.organization().unwrap(), OrganizationRecord::default());
        let empty = store.revision().unwrap();
        let record = OrganizationRecord {
            organization: Organization {
                name: "Contoso".into(),
                entries: vec![entry("code-review", "Every change is reviewed.")],
                // Sources are IDs of the map's own sources, checked when it is saved.
                sources: vec![],
            },
            guides: vec![guide("task", "A unit of work.")],
        };
        store.replace(&record).unwrap();
        assert_eq!(store.organization().unwrap(), record);
        let full = store.revision().unwrap();
        assert_ne!(full, empty);
        assert_eq!(store.revision().unwrap(), full, "stable while unchanged");
        store.replace(&record).unwrap();
        assert_eq!(store.revision().unwrap(), full);
    }

    fn entries_are_saved_by_id_and_removed(store: &dyn OrganizationStore) {
        let before = store.revision().unwrap();
        let changed = entry("code-review", "Every change is reviewed by two people.");
        store.save_entry(&changed).unwrap();
        store
            .save_entry(&entry("glossary", "A story is a user capability."))
            .unwrap();
        let entries = store.organization().unwrap().organization.entries;
        assert_eq!(
            entries,
            vec![changed, entry("glossary", "A story is a user capability.")]
        );
        assert_ne!(store.revision().unwrap(), before);
        assert!(store.remove_entry("glossary").unwrap());
        assert!(!store.remove_entry("glossary").unwrap());
        let record = store.organization().unwrap();
        assert_eq!(record.organization.entries.len(), 1);
        assert_eq!(record.organization.name, "Contoso", "the rest is kept");
        assert_eq!(record.guides.len(), 1);
    }

    fn guides_are_saved_by_id_and_removed(store: &dyn OrganizationStore) {
        let changed = guide("task", "A unit of work under a story.");
        store.save_guide(&changed).unwrap();
        store.save_guide(&guide("bug", "A defect.")).unwrap();
        assert_eq!(
            store.organization().unwrap().guides,
            vec![changed.clone(), guide("bug", "A defect.")]
        );
        assert!(store.remove_guide("bug").unwrap());
        assert!(!store.remove_guide("bug").unwrap());
        let record = store.organization().unwrap();
        assert_eq!(record.guides, vec![changed]);
        assert_eq!(record.organization.entries.len(), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::settings::LocalSettings;

    fn profile() -> (tempfile::TempDir, LocalSettings) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("profile");
        crate::security::private_dir(&dir).unwrap();
        let store = LocalSettings::open(&dir, &root.path().join("data")).unwrap();
        (root, store)
    }

    #[test]
    fn local_map_meets_the_contract() {
        let (_dir, settings) = profile();
        contract::run(&LocalOrganization::new(&settings));
    }

    #[test]
    fn the_organization_stays_in_the_map_and_keeps_the_rest_of_it() {
        let (_dir, settings) = profile();
        let before = settings.map().unwrap();
        let store = LocalOrganization::new(&settings);
        store
            .save_entry(&contract::entry("code-review", "Every change is reviewed."))
            .unwrap();
        store
            .save_guide(&contract::guide("task", "A unit of work."))
            .unwrap();
        let map = settings.map().unwrap();
        assert_eq!(map.organization.entries[0].id, "code-review");
        assert_eq!(map.work_item_guides[0].id, "task");
        assert_eq!(
            serde_json::to_string(&map.resources).unwrap(),
            serde_json::to_string(&before.resources).unwrap(),
            "the sources are untouched"
        );
    }
}
