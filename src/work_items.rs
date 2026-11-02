//! Work item guides: how the user's organization uses each work item type, which fields it
//! carries and how to fill them. The implementation agents receive the guides that apply when
//! they plan children, implement an item or write a request. A guide shapes what they write and
//! grants nothing: the destination and type are still checked against the provider, and a field
//! value is written only when its guide marks the field `fill` and it is valid for the type's
//! live definition.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Guide {
    pub id: String,
    /// `azure` or `github`.
    #[serde(default = "azure")]
    pub provider: String,
    /// The provider's exact type name («Product Backlog Item», «Task», «Epic»).
    #[serde(rename = "type")]
    pub kind: String,
    /// What the user calls it («HU», «tarea de apoyo»).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Azure projects or GitHub repositories (`owner/name`) it applies to; empty applies to all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    /// When this type is used.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub purpose: String,
    /// How to write one: title, description, conventions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    /// To-Do lists this Azure task-category type together with the bugs (a process may classify
    /// a development bug as a task). Without it, To-Do lists only requirement and bug categories.
    #[serde(default, skip_serializing_if = "is_false")]
    pub listed_with_bugs: bool,
}
fn azure() -> String {
    "azure".into()
}
fn is_false(value: &bool) -> bool {
    !*value
}

/// One field of a work item type, as the user's organization uses it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// The provider's reference name («Custom.Month», «Microsoft.VSTS.Common.AcceptanceCriteria»).
    pub reference: String,
    /// The label people see.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// The allowed values, when it is a list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// The value the provider sets when none is given.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default: String,
    /// What it is for and how to fill it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub guidance: String,
    /// The agents may propose its value when they create an item of this type; the user reviews
    /// child values before creation, and every value is checked against the live type.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fill: bool,
}

pub fn valid_reference(reference: &str) -> bool {
    let mut chars = reference.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && reference.len() <= 200
        && chars.all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}
/// Fields code always sets itself (title, description, assignment, area, links…).
pub fn reserved(reference: &str) -> bool {
    reference.starts_with("System.")
}

pub fn validate(guides: &[Guide]) -> Result<()> {
    ensure!(
        guides.len() <= 50,
        "at most 50 work item guides are allowed"
    );
    let id_pattern = regex::Regex::new(r"^[a-z][a-z0-9_-]{0,63}$")?;
    let plain = |text: &str, max: usize| {
        text.chars().count() <= max
            && !text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
    };
    let mut ids = BTreeSet::new();
    for guide in guides {
        ensure!(
            id_pattern.is_match(&guide.id) && ids.insert(guide.id.as_str()),
            "invalid or duplicate work item guide ID"
        );
        ensure!(
            matches!(guide.provider.as_str(), "azure" | "github"),
            "a work item guide's provider is azure or github"
        );
        ensure!(
            !guide.kind.trim().is_empty()
                && plain(&guide.kind, 120)
                && plain(&guide.name, 80)
                && plain(&guide.purpose, 2000)
                && plain(&guide.instructions, 8000),
            "a work item guide needs its type (at most 120 characters); its name takes at most 80, its purpose 2000 and its instructions 8000"
        );
        ensure!(
            guide.projects.len() <= 50
                && guide
                    .projects
                    .iter()
                    .all(|p| !p.trim().is_empty() && plain(p, 200)),
            "a work item guide applies to at most 50 projects"
        );
        ensure!(
            !guide.listed_with_bugs || guide.provider == "azure",
            "only an Azure work item guide can list its type with the bugs"
        );
        ensure!(
            guide.fields.len() <= 80,
            "a work item guide has at most 80 fields"
        );
        let mut references = BTreeSet::new();
        for field in &guide.fields {
            ensure!(
                valid_reference(&field.reference) && references.insert(field.reference.as_str()),
                "invalid or duplicate field reference {} in work item guide {}",
                field.reference,
                guide.id
            );
            ensure!(
                plain(&field.name, 120)
                    && plain(&field.default, 200)
                    && plain(&field.guidance, 1000)
                    && field.values.len() <= 200
                    && field.values.iter().all(|v| !v.is_empty() && plain(v, 200)),
                "field {} in work item guide {}: name up to 120 characters, default 200, guidance 1000, up to 200 values",
                field.reference,
                guide.id
            );
            ensure!(
                !field.fill || (guide.provider == "azure" && !reserved(&field.reference)),
                "field {} in work item guide {} cannot be filled by the agents: only Azure fields outside System.* can",
                field.reference,
                guide.id
            );
        }
    }
    Ok(())
}

fn applies(guide: &Guide, provider: &str, project: &str) -> bool {
    guide.provider == provider
        && (guide.projects.is_empty()
            || guide
                .projects
                .iter()
                .any(|p| p.eq_ignore_ascii_case(project)))
}
/// The guide for one type in one project; a guide naming the project wins over a general one.
pub fn for_item<'a>(
    guides: &'a [Guide],
    provider: &str,
    kind: &str,
    project: &str,
) -> Option<&'a Guide> {
    let mut matching = guides
        .iter()
        .filter(|g| applies(g, provider, project) && g.kind.eq_ignore_ascii_case(kind));
    let first = matching.next()?;
    Some(if first.projects.is_empty() {
        matching.find(|g| !g.projects.is_empty()).unwrap_or(first)
    } else {
        first
    })
}
/// Whether To-Do lists an Azure task-category type of a project with the bugs.
pub fn listed_with_bugs(guides: &[Guide], kind: &str, project: &str) -> bool {
    for_item(guides, "azure", kind, project).is_some_and(|g| g.listed_with_bugs)
}
/// Every guide that applies in a project, one per type.
pub fn for_project<'a>(guides: &'a [Guide], provider: &str, project: &str) -> Vec<&'a Guide> {
    let mut kinds = BTreeSet::new();
    guides
        .iter()
        .filter(|g| applies(g, provider, project))
        .filter_map(|g| {
            kinds
                .insert(g.kind.to_lowercase())
                .then(|| for_item(guides, provider, &g.kind, project))
                .flatten()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guide(id: &str, kind: &str, projects: &[&str]) -> Guide {
        Guide {
            id: id.into(),
            provider: "azure".into(),
            kind: kind.into(),
            projects: projects.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_guide_lists_an_azure_task_type_with_the_bugs_in_its_projects() {
        let mut general = guide("dev-bug", "Development Bug", &[]);
        general.listed_with_bugs = true;
        let mut payments = guide("dev-bug-payments", "Development Bug", &["Payments"]);
        payments.listed_with_bugs = false;
        let guides = [general.clone(), payments];
        assert!(validate(&guides).is_ok());
        assert!(listed_with_bugs(&guides, "development bug", "Billing"));
        // The project's own guide wins over the general one.
        assert!(!listed_with_bugs(&guides, "Development Bug", "Payments"));
        assert!(!listed_with_bugs(&guides, "Task", "Billing"));
        general.provider = "github".into();
        assert!(validate(&[general]).is_err());
    }

    #[test]
    fn a_project_guide_wins_over_a_general_one() {
        let guides = [
            guide("task", "Task", &[]),
            guide("task-cobros", "Task", &["Cobros"]),
            guide("hu", "Product Backlog Item", &[]),
            guide("pr", "Apoyo", &["Desarrollo"]),
        ];
        assert!(validate(&guides).is_ok());
        assert_eq!(
            for_item(&guides, "azure", "task", "cobros").unwrap().id,
            "task-cobros"
        );
        assert_eq!(
            for_item(&guides, "azure", "Task", "Otro").unwrap().id,
            "task"
        );
        assert!(for_item(&guides, "github", "Task", "Cobros").is_none());
        let ids: Vec<_> = for_project(&guides, "azure", "Cobros")
            .iter()
            .map(|g| g.id.as_str())
            .collect();
        assert_eq!(ids, ["task-cobros", "hu"]);
    }

    #[test]
    fn only_azure_fields_outside_system_can_be_filled() {
        let mut task = guide("task", "Task", &[]);
        task.fields = vec![Field {
            reference: "Custom.Month".into(),
            values: vec!["Octubre".into()],
            fill: true,
            ..Default::default()
        }];
        assert!(validate(std::slice::from_ref(&task)).is_ok());
        task.fields[0].reference = "System.AssignedTo".into();
        assert!(validate(std::slice::from_ref(&task)).is_err());
        task.fields[0].fill = false;
        assert!(validate(std::slice::from_ref(&task)).is_ok());
        task.fields.push(task.fields[0].clone());
        assert!(validate(std::slice::from_ref(&task)).is_err());
        let mut epic = guide("epic", "Epic", &[]);
        epic.provider = "github".into();
        epic.fields = vec![Field {
            reference: "labels".into(),
            fill: true,
            ..Default::default()
        }];
        assert!(validate(&[epic]).is_err());
        assert!(validate(&[guide("Bad id", "Task", &[])]).is_err());
        assert!(validate(&[guide("x", " ", &[])]).is_err());
        // Saved, a guide keeps only what it says; `type` is the provider's type name.
        let saved = toml::to_string(&guide("task", "Task", &[])).unwrap();
        assert_eq!(
            saved,
            "id = \"task\"\nprovider = \"azure\"\ntype = \"Task\"\n"
        );
    }
}
