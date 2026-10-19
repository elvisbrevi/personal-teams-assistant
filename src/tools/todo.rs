//! Typed results shared by the To-Do tools, the CLI and the GUI. No model is involved.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

fn default_limit() -> usize {
    10
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Filters {
    pub source: Option<String>,
    pub project: Option<String>,
    pub state: Option<String>,
    pub limit: usize,
}
impl Default for Filters {
    fn default() -> Self {
        Self {
            source: None,
            project: None,
            state: None,
            limit: default_limit(),
        }
    }
}
impl Filters {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=100).contains(&self.limit),
            "limit must be between 1 and 100"
        );
        for value in [&self.source, &self.project, &self.state]
            .into_iter()
            .flatten()
        {
            ensure!(
                !value.trim().is_empty()
                    && value.len() <= 200
                    && !value.chars().any(char::is_control),
                "invalid To-Do filter"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Date {
    pub field: String,
    pub name: String,
    pub value: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub project: String,
    pub state: String,
    pub created_at: Option<String>,
    pub dates: Vec<Date>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct List {
    pub items: Vec<Item>,
    pub states: Vec<String>,
    pub projects: Vec<String>,
    /// Field names used for developer assignment, useful when a process differs from Assigned To.
    pub assignment_fields: Vec<String>,
    pub warnings: Vec<String>,
    pub partial: bool,
    pub has_more: bool,
}
impl List {
    pub fn finish(&mut self, limit: usize) {
        self.items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
                .then_with(|| a.url.cmp(&b.url))
        });
        self.items.dedup_by(|a, b| a.url == b.url);
        self.has_more |= self.items.len() > limit;
        self.items.truncate(limit);
        for values in [
            &mut self.states,
            &mut self.projects,
            &mut self.assignment_fields,
        ] {
            values.sort();
            values.dedup();
        }
    }
    pub fn warn(&mut self, warning: String) {
        self.partial = true;
        self.warnings.push(warning);
    }
}

/// Only canonical provider work-item links may leave the native app through its shell.
pub fn validate_link(raw: &str) -> Result<url::Url> {
    let url = url::Url::parse(raw)?;
    ensure!(
        url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid work item link"
    );
    let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
    let valid = match url.host_str() {
        Some("dev.azure.com") => parts.len() == 5 && parts[2..4] == ["_workitems", "edit"],
        Some("github.com") => parts.len() == 4 && parts[2] == "issues",
        _ => false,
    };
    ensure!(
        valid
            && parts.iter().all(|p| !p.is_empty())
            && parts
                .last()
                .is_some_and(|id| id.parse::<u64>().is_ok_and(|n| n > 0)),
        "invalid work item link"
    );
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_and_links_cannot_widen_the_tool() {
        assert_eq!(serde_json::from_str::<Filters>("{}").unwrap().limit, 10);
        assert!(serde_json::from_str::<Filters>(r#"{"assignee":"someone"}"#).is_err());
        assert!(
            Filters {
                limit: 0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        for raw in [
            "https://github.com/example/repo/issues/12",
            "https://dev.azure.com/example/Project/_workitems/edit/7",
        ] {
            validate_link(raw).unwrap();
        }
        for raw in [
            "https://github.com.evil.test/a/b/issues/1",
            "https://token@github.com/a/b/issues/1",
            "https://github.com/a/b/issues/1?token=x",
            "https://dev.azure.com/a/b/_workitems/edit/0",
        ] {
            assert!(validate_link(raw).is_err(), "{raw}");
        }
    }
}
