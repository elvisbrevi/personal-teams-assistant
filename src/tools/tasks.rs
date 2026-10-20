//! Existing Azure tasks grouped by the day their work was recorded. No writes or model.
use super::todo::{Filters, Item};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub day: String,
    pub user: Option<String>,
    pub source: Option<String>,
    pub project: Option<String>,
}
impl Query {
    pub fn validate(&self) -> Result<chrono::NaiveDate> {
        Filters {
            source: self.source.clone(),
            project: self.project.clone(),
            ..Default::default()
        }
        .validate()?;
        if let Some(user) = &self.user {
            ensure!(
                user.contains('@')
                    && user.len() <= 254
                    && user
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || ".@_+-".contains(c)),
                "invalid task user: use the Azure email (uniqueName)"
            );
        }
        let day = self.day.parse::<chrono::NaiveDate>()?;
        ensure!(day.to_string() == self.day, "invalid day: use YYYY-MM-DD");
        Ok(day)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Task {
    pub item: Item,
    pub source: String,
    pub organization: String,
    pub user: String,
    pub description: String,
    pub hours: f64,
    pub parent: Option<Item>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Day {
    pub day: String,
    pub time_zone: String,
    pub tasks: Vec<Task>,
    pub hours: f64,
    pub target_hours: f64,
    pub saved_at: i64,
    pub partial: bool,
    pub warnings: Vec<String>,
}

impl Day {
    /// Adapt snapshots to the existing, verified activity-record evidence path.
    pub fn entries(&self) -> Vec<crate::activity::Entry> {
        use crate::{
            activity::{Draft, Entry},
            ado::link::Target,
        };
        let entry = |task: Option<&Task>| Entry {
            id: task
                .map(|t| t.item.url.clone())
                .unwrap_or_else(|| format!("empty:{}", self.day)),
            run: String::new(),
            day: self.day.clone(),
            state: if task.is_some() {
                "recorded"
            } else {
                "recorded_empty"
            }
            .into(),
            draft: Draft {
                activities: vec![],
                parent: task.and_then(|t| t.parent.as_ref().map(|p| p.id)),
                title: task.map(|t| t.item.title.clone()).unwrap_or_default(),
                description: task.map(|t| t.description.clone()).unwrap_or_default(),
                evidence: String::new(),
                hours: task.map(|t| t.hours),
                confidence: 1.,
                question: String::new(),
                suggested_parents: vec![],
                ignore: false,
            },
            activities: vec![],
            parents: task
                .map(|t| {
                    t.parent
                        .iter()
                        .chain(std::iter::once(&t.item))
                        .map(|p| Target {
                            id: p.id,
                            title: p.title.clone(),
                            url: p.url.clone(),
                            organization: t.organization.clone(),
                            project: p.project.clone(),
                            kind: p.kind.clone(),
                            state: p.state.clone(),
                            ..Default::default()
                        })
                        .collect()
                })
                .unwrap_or_default(),
            evidence: String::new(),
            context: String::new(),
            fields: Default::default(),
            issue: String::new(),
            task_id: task.map(|t| t.item.id),
            task_url: task.map(|t| t.item.url.clone()),
            updated_at: self.saved_at,
            required: vec![],
            date_attribution: String::new(),
        };
        if self.tasks.is_empty() {
            vec![entry(None)]
        } else {
            self.tasks.iter().map(|t| entry(Some(t))).collect()
        }
    }
}
