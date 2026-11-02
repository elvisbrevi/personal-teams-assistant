//! The organization layer: the user's employer's conventions kept as data, never as code. It is
//! optional and belongs to the account (it is not shared between accounts): norms, reference
//! architecture, process conventions, glossary terms and templates are its entries; the work
//! item guides (`crate::work_items`) are its `work_item_guide` entries and keep their own
//! section of the map; and `sources` names existing sources that document the organization,
//! whose access stays exactly that of the source.
//!
//! An entry is untrusted data wherever it goes: the `organization` source reads the entries
//! from the knowledge index for Teams answers, and the implementation agents receive the norms,
//! architecture and conventions that apply to the item's project. An entry never authorizes a
//! source, a destination, a type, a field or a value.

use crate::knowledge::Access;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Entries an organization holds.
pub const MAX_ENTRIES: usize = 200;
/// Characters of one entry's text.
pub const MAX_TEXT: usize = 20_000;
/// Characters of entry text an implementation run receives.
pub const AGENT_BUDGET: usize = 24_000;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Organization {
    /// Shown to the user only; it names nothing in code.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<Entry>,
    /// IDs of the map's sources that document the organization (a Wiki, a repository, a file or
    /// a URL). Labelling a source grants nothing: it is read only where it is authorized.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

/// One stored item of the organization.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    /// Markdown, at most `MAX_TEXT` characters.
    pub text: String,
    /// Azure projects or GitHub repositories (`owner/name`) it applies to; empty applies to all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    /// When its content last changed (Unix seconds); set by the host when it is saved.
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Norm,
    Architecture,
    Convention,
    GlossaryTerm,
    Template,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Norm,
        Kind::Architecture,
        Kind::Convention,
        Kind::GlossaryTerm,
        Kind::Template,
    ];
    /// What it is, for the index and the agents' input.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Norm => "Norm",
            Kind::Architecture => "Architecture",
            Kind::Convention => "Convention",
            Kind::GlossaryTerm => "Glossary term",
            Kind::Template => "Template",
        }
    }
    /// The kinds the implementation agents receive with the item's project.
    pub fn for_agents(self) -> bool {
        matches!(self, Kind::Norm | Kind::Architecture | Kind::Convention)
    }
}

impl Organization {
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.entries.is_empty() && self.sources.is_empty()
    }
}

/// Whether a source of this kind can document the organization.
pub fn documents(access: &Access) -> bool {
    match access {
        Access::File { .. } | Access::Url { .. } | Access::Urls { .. } => true,
        Access::Repository { .. } => true,
        Access::Tool { tool } => matches!(tool, crate::tools::ToolSpec::AzureDevopsWiki { .. }),
        Access::Notes { .. } | Access::Organization {} => false,
    }
}

fn plain(text: &str, max: usize) -> bool {
    text.chars().count() <= max
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}

/// Check the organization with the map's sources (`source` finds one by ID).
pub fn validate<'a>(
    organization: &Organization,
    source: impl Fn(&str) -> Option<&'a Access>,
) -> Result<()> {
    ensure!(
        plain(&organization.name, 120) && !organization.name.contains('\n'),
        "the organization's name takes at most 120 characters on one line"
    );
    ensure!(
        organization.entries.len() <= MAX_ENTRIES,
        "an organization holds at most {MAX_ENTRIES} entries"
    );
    let id_pattern = regex::Regex::new(r"^[a-z][a-z0-9_-]{0,63}$")?;
    let mut ids = BTreeSet::new();
    for entry in &organization.entries {
        ensure!(
            id_pattern.is_match(&entry.id) && ids.insert(entry.id.as_str()),
            "invalid or duplicate organization entry ID"
        );
        ensure!(
            !entry.title.trim().is_empty() && plain(&entry.title, 200),
            "organization entry {} needs a title of at most 200 characters",
            entry.id
        );
        ensure!(
            !entry.text.trim().is_empty() && plain(&entry.text, MAX_TEXT),
            "organization entry {} needs a text of at most {MAX_TEXT} characters",
            entry.id
        );
        ensure!(
            entry.projects.len() <= 50
                && entry
                    .projects
                    .iter()
                    .all(|p| !p.trim().is_empty() && plain(p, 200)),
            "organization entry {} applies to at most 50 projects",
            entry.id
        );
        ensure!(
            entry.updated_at >= 0,
            "invalid date in organization entry {}",
            entry.id
        );
    }
    ensure!(
        organization.sources.len() <= 50,
        "an organization names at most 50 sources"
    );
    let mut named = BTreeSet::new();
    for id in &organization.sources {
        let access = source(id);
        ensure!(
            access.is_some() && named.insert(id.as_str()),
            "the organization's sources must exist in the knowledge map, once each"
        );
        ensure!(
            access.is_some_and(documents),
            "source {id} cannot document the organization: choose a Wiki, a repository, a file or a URL"
        );
    }
    Ok(())
}

/// Date the entries whose content changed since `previous` (new ones included) with `now`;
/// unchanged entries keep their date.
pub fn stamp(previous: &Organization, next: &mut Organization, now: i64) {
    for entry in &mut next.entries {
        let before = previous.entries.iter().find(|e| e.id == entry.id);
        let same = before.is_some_and(|b| {
            b.kind == entry.kind
                && b.title == entry.title
                && b.text == entry.text
                && b.projects == entry.projects
        });
        entry.updated_at = match before {
            Some(b) if same => b.updated_at,
            _ => now,
        };
    }
}

fn applies(entry: &Entry, project: &str) -> bool {
    entry.projects.is_empty()
        || entry
            .projects
            .iter()
            .any(|p| p.eq_ignore_ascii_case(project))
}

/// The norms, architecture and conventions an implementation run in `project` receives: those
/// naming the project first, then the general ones, each whole, while their texts fit in
/// `AGENT_BUDGET` characters.
pub fn for_agents<'a>(organization: &'a Organization, project: &str) -> Vec<&'a Entry> {
    let mut candidates: Vec<&Entry> = organization
        .entries
        .iter()
        .filter(|e| e.kind.for_agents() && applies(e, project))
        .collect();
    candidates.sort_by_key(|e| e.projects.is_empty());
    let mut left = AGENT_BUDGET;
    candidates
        .into_iter()
        .filter(|e| {
            let size = e.title.chars().count() + e.text.chars().count();
            let fits = size <= left;
            if fits {
                left -= size;
            }
            fits
        })
        .collect()
}

/// Entries as an agent's input: what each is, its title, text and projects.
pub fn agent_input<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> serde_json::Value {
    serde_json::Value::Array(
        entries
            .into_iter()
            .map(|e| {
                serde_json::json!({"kind": e.kind, "title": e.title, "text": e.text, "projects": e.projects})
            })
            .collect(),
    )
}

/// An entry as an indexed document: its title and Markdown, with what it is and where it
/// applies.
pub fn entry_text(entry: &Entry) -> String {
    let scope = if entry.projects.is_empty() {
        String::new()
    } else {
        format!(" Applies to: {}.", entry.projects.join(", "))
    };
    format!("{}.{scope}\n\n{}", entry.kind.label(), entry.text)
}

/// A work item guide as an indexed document: how the type is used and written, and its fields.
pub fn guide_text(guide: &crate::work_items::Guide) -> String {
    let mut text = format!(
        "Work item guide for the {} type «{}».",
        if guide.provider == "github" {
            "GitHub"
        } else {
            "Azure DevOps"
        },
        guide.kind
    );
    if !guide.projects.is_empty() {
        text.push_str(&format!(" Applies to: {}.", guide.projects.join(", ")));
    }
    for (heading, body) in [
        ("When it is used", &guide.purpose),
        ("How to write it", &guide.instructions),
    ] {
        if !body.trim().is_empty() {
            text.push_str(&format!("\n\n## {heading}\n{body}"));
        }
    }
    if !guide.fields.is_empty() {
        text.push_str("\n\n## Fields");
        for field in &guide.fields {
            let name = if field.name.is_empty() {
                &field.reference
            } else {
                &field.name
            };
            text.push_str(&format!("\n- {name}"));
            if field.required {
                text.push_str(" (required)");
            }
            if !field.guidance.is_empty() {
                text.push_str(&format!(": {}", field.guidance));
            }
            if !field.values.is_empty() {
                text.push_str(&format!(" Values: {}.", field.values.join(", ")));
            }
        }
    }
    text
}

/// The title of a guide's document.
pub fn guide_title(guide: &crate::work_items::Guide) -> String {
    if guide.name.is_empty() {
        guide.kind.clone()
    } else {
        format!("{} ({})", guide.name, guide.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: Kind, projects: &[&str], text: &str) -> Entry {
        Entry {
            id: id.into(),
            kind,
            title: format!("Title of {id}"),
            text: text.into(),
            projects: projects.iter().map(|p| p.to_string()).collect(),
            updated_at: 0,
        }
    }

    fn page() -> Access {
        Access::Url {
            url: "https://example.com/handbook".into(),
        }
    }

    #[test]
    fn entries_and_sources_are_checked() {
        let page = page();
        let notes = Access::Notes { contexts: vec![] };
        let lookup = |id: &str| match id {
            "handbook" => Some(&page),
            "notes" => Some(&notes),
            _ => None,
        };
        let mut organization = Organization {
            name: "Contoso".into(),
            entries: vec![entry("review", Kind::Norm, &[], "Two approvals.")],
            sources: vec!["handbook".into()],
        };
        validate(&organization, lookup).unwrap();
        // Saved, the organization keeps only what it says.
        assert_eq!(
            toml::to_string(&organization).unwrap(),
            "name = \"Contoso\"\nsources = [\"handbook\"]\n\n[[entries]]\nid = \"review\"\nkind = \"norm\"\ntitle = \"Title of review\"\ntext = \"Two approvals.\"\nupdated_at = 0\n"
        );
        for broken in [
            |o: &mut Organization| o.sources.push("missing".into()),
            |o: &mut Organization| o.sources.push("notes".into()),
            |o: &mut Organization| o.sources.push("handbook".into()),
            |o: &mut Organization| o.entries.push(o.entries[0].clone()),
            |o: &mut Organization| o.entries[0].text = " ".into(),
            |o: &mut Organization| o.entries[0].text = "x".repeat(MAX_TEXT + 1),
            |o: &mut Organization| o.entries[0].id = "Bad id".into(),
            |o: &mut Organization| o.name = "Two\nlines".into(),
        ] {
            let mut changed = organization.clone();
            broken(&mut changed);
            assert!(validate(&changed, lookup).is_err());
        }
        organization.sources.clear();
        organization.entries.clear();
        organization.name.clear();
        assert!(organization.is_empty());
    }

    #[test]
    fn only_changed_entries_get_a_new_date() {
        let mut previous = Organization {
            entries: vec![
                entry("a", Kind::Norm, &[], "One."),
                entry("b", Kind::Convention, &[], "Two."),
            ],
            ..Default::default()
        };
        previous.entries[0].updated_at = 10;
        previous.entries[1].updated_at = 10;
        let mut next = previous.clone();
        next.entries[1].text = "Changed.".into();
        next.entries.push(entry("c", Kind::Template, &[], "New."));
        // A client cannot choose the date.
        next.entries[0].updated_at = 99;
        stamp(&previous, &mut next, 50);
        let dates: Vec<i64> = next.entries.iter().map(|e| e.updated_at).collect();
        assert_eq!(dates, [10, 50, 50]);
    }

    #[test]
    fn agents_get_the_project_entries_first_within_the_budget() {
        let organization = Organization {
            entries: vec![
                entry("general", Kind::Norm, &[], "General norm."),
                entry("glossary", Kind::GlossaryTerm, &[], "Not for agents."),
                entry(
                    "billing",
                    Kind::Architecture,
                    &["Billing"],
                    "Billing layers.",
                ),
                entry("other", Kind::Convention, &["Other"], "Elsewhere."),
                entry("huge", Kind::Convention, &[], &"x".repeat(AGENT_BUDGET)),
                entry("late", Kind::Convention, &[], "Still fits."),
            ],
            ..Default::default()
        };
        let ids: Vec<&str> = for_agents(&organization, "billing")
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(ids, ["billing", "general", "late"]);
    }
}
