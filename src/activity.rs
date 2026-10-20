//! Scheduled snapshots of existing tasks; legacy records remain readable.
pub(crate) mod context;
pub mod recorded;
pub mod store;
#[cfg(test)]
mod tests;
use crate::{
    ado::{
        link::{Linkable, Target},
        registration::Field,
    },
    knowledge::{Access, KnowledgeMap},
    security::Redactor,
    tools::ToolSpec,
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Datelike, FixedOffset, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub auto_register: bool,
    pub daily_hours: f64,
    /// `daily` or `interval`; interval slots are anchored to window_start.
    pub schedule: String,
    pub at: String,
    pub interval_minutes: u32,
    pub window_start: String,
    pub window_end: String,
    pub weekdays: Vec<u32>,
    /// `local` follows the computer, otherwise an IANA time zone.
    pub time_zone: String,
    pub sources: Vec<String>,
    pub write_source: String,
    pub task_kind: String,
    pub done_state: String,
    pub effort_field: String,
    pub extra_fields: BTreeMap<String, Value>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_register: false,
            daily_hours: 9.,
            schedule: "daily".into(),
            at: "18:00".into(),
            interval_minutes: 60,
            window_start: "09:00".into(),
            window_end: "18:00".into(),
            weekdays: vec![1, 2, 3, 4, 5],
            time_zone: "local".into(),
            sources: vec![],
            write_source: String::new(),
            task_kind: "Task".into(),
            done_state: "Done".into(),
            effort_field: "Microsoft.VSTS.Scheduling.CompletedWork".into(),
            extra_fields: BTreeMap::new(),
        }
    }
}
fn time(value: &str) -> Result<NaiveTime> {
    ensure!(value.len() == 5, "time must be HH:MM");
    Ok(NaiveTime::parse_from_str(value, "%H:%M")?)
}
/// An Azure DevOps field reference name. Inherited processes may name custom fields with a
/// GUID (`Custom.b505c83e-3745-…`), so hyphens are allowed after the first character.
pub fn valid_field(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 150
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}
pub fn validate_fields(fields: &BTreeMap<String, Value>) -> Result<()> {
    ensure!(fields.len() <= 30, "too many task fields");
    for (name, value) in fields {
        ensure!(
            valid_field(name)
                && ![
                    "System.Id",
                    "System.Rev",
                    "System.WorkItemType",
                    "System.TeamProject",
                    "System.AssignedTo",
                    "System.CreatedBy",
                    "System.CreatedDate",
                    "System.ChangedBy",
                    "System.ChangedDate",
                    "System.AreaPath",
                    "System.IterationPath",
                    "System.Title",
                    "System.Description",
                    "System.State"
                ]
                .contains(&name.as_str()),
            "invalid additional task field"
        );
        ensure!(
            value.is_string() || value.is_number() || value.is_boolean(),
            "task fields must be scalar values"
        );
        ensure!(
            value
                .as_str()
                .is_none_or(|s| s.len() <= 8000 && !s.chars().any(|c| c.is_control() && c != '\n')),
            "task field too large"
        );
    }
    Ok(())
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.daily_hours.is_finite() && (0.25..=24.).contains(&self.daily_hours),
            "invalid workday hours"
        );
        ensure!(
            ["daily", "interval"].contains(&self.schedule.as_str()),
            "invalid registration schedule"
        );
        time(&self.at)?;
        ensure!(
            time(&self.window_start)? < time(&self.window_end)?,
            "registration window must start before it ends"
        );
        ensure!(
            (5..=1440).contains(&self.interval_minutes),
            "invalid registration interval"
        );
        ensure!(
            !self.weekdays.is_empty()
                && self.weekdays.len() <= 7
                && self.weekdays.iter().all(|d| (1..=7).contains(d))
                && self.weekdays.iter().collect::<BTreeSet<_>>().len() == self.weekdays.len(),
            "invalid weekdays"
        );
        ensure!(
            self.time_zone == "local" || self.time_zone.parse::<chrono_tz::Tz>().is_ok(),
            "invalid time zone"
        );
        ensure!(
            crate::ado::link::TASK_KINDS
                .iter()
                .any(|k| *k == self.task_kind),
            "registration must create Task or Tarea items"
        );
        ensure!(
            !self.done_state.trim().is_empty()
                && self.done_state.len() <= 80
                && !self.done_state.chars().any(char::is_control),
            "invalid completed state"
        );
        ensure!(
            valid_field(&self.effort_field)
                && !self.effort_field.starts_with("System.")
                && self.effort_field != "Microsoft.VSTS.Scheduling.RemainingWork",
            "invalid effort field"
        );
        ensure!(
            self.sources.len() <= 10
                && self.sources.iter().collect::<BTreeSet<_>>().len() == self.sources.len(),
            "invalid registration sources"
        );
        validate_fields(&self.extra_fields)?;
        Ok(())
    }
    pub fn zoned(&self, now: DateTime<Utc>) -> DateTime<FixedOffset> {
        if self.time_zone == "local" {
            now.with_timezone(&Local).fixed_offset()
        } else {
            now.with_timezone(
                &self
                    .time_zone
                    .parse::<chrono_tz::Tz>()
                    .expect("validated time zone"),
            )
            .fixed_offset()
        }
    }
    pub fn bounds(&self, day: NaiveDate) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
        let start = day.and_hms_opt(0, 0, 0).context("invalid day")?;
        let end = day
            .succ_opt()
            .context("invalid day")?
            .and_hms_opt(0, 0, 0)
            .unwrap();
        if self.time_zone == "local" {
            Ok((
                Local
                    .from_local_datetime(&start)
                    .earliest()
                    .context("invalid local midnight")?
                    .with_timezone(&Utc),
                Local
                    .from_local_datetime(&end)
                    .earliest()
                    .context("invalid local midnight")?
                    .with_timezone(&Utc),
            ))
        } else {
            let zone: chrono_tz::Tz = self.time_zone.parse()?;
            Ok((
                zone.from_local_datetime(&start)
                    .earliest()
                    .context("invalid midnight")?
                    .with_timezone(&Utc),
                zone.from_local_datetime(&end)
                    .earliest()
                    .context("invalid midnight")?
                    .with_timezone(&Utc),
            ))
        }
    }
    /// Catch up today's latest due slot, once. Never replays missed historical slots.
    pub fn slot(&self, now: DateTime<Utc>) -> Result<Option<String>> {
        if !self.enabled {
            return Ok(None);
        }
        self.validate()?;
        let local = self.zoned(now);
        if !self.enabled
            || !self
                .weekdays
                .contains(&local.weekday().number_from_monday())
        {
            return Ok(None);
        }
        let due = if self.schedule == "daily" {
            if local.time() < time(&self.at)? {
                return Ok(None);
            }
            self.at.clone()
        } else {
            let start = time(&self.window_start)?;
            if local.time() < start || local.time() > time(&self.window_end)? {
                return Ok(None);
            }
            let minutes = (local.time() - start).num_minutes();
            (start
                + chrono::Duration::minutes(
                    minutes / i64::from(self.interval_minutes) * i64::from(self.interval_minutes),
                ))
            .format("%H:%M")
            .to_string()
        };
        Ok(Some(format!(
            "{}|{}|{}|{}",
            self.time_zone,
            local.date_naive(),
            self.schedule,
            due
        )))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub activities: Vec<String>,
    pub parent: Option<u64>,
    pub title: String,
    pub description: String,
    /// The agent's completion evidence: the result and the artifacts by name, never chat text.
    #[serde(default)]
    pub evidence: String,
    pub hours: Option<f64>,
    pub confidence: f64,
    pub question: String,
    pub suggested_parents: Vec<u64>,
    /// Only messages that do not describe performed work may be ignored.
    pub ignore: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub id: String,
    pub run: String,
    pub day: String,
    pub state: String,
    pub draft: Draft,
    pub activities: Vec<Linkable>,
    pub parents: Vec<Target>,
    pub evidence: String,
    pub context: String,
    pub fields: BTreeMap<String, Value>,
    pub issue: String,
    pub task_id: Option<u64>,
    pub task_url: Option<String>,
    pub updated_at: i64,
    /// Fields the task type requires from the user in the HU's project, learned from Azure
    /// DevOps when a task is first prepared there; their values go in `fields`.
    #[serde(default)]
    pub required: Vec<Field>,
    /// The user's explanation for attributing an explicitly selected other-day artifact here.
    #[serde(default)]
    pub date_attribution: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Summary {
    pub recorded_hours: f64,
    pub remaining_hours: f64,
    pub pending: usize,
    pub created: usize,
    pub partial: bool,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    pub action: String,
    #[serde(default)]
    pub parent: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub hours: Option<f64>,
    #[serde(default)]
    pub context: String,
    /// Explicit user confirmation of why an other-day artifact belongs to this work day.
    #[serde(default)]
    pub date_attribution: String,
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
}
impl Resolution {
    pub fn validate(&self, redactor: &Redactor) -> Result<()> {
        ensure!(
            ["create", "refine", "dismiss"].contains(&self.action.as_str()),
            "invalid resolution action"
        );
        validate_context(&self.context, redactor)?;
        validate_context(&self.date_attribution, redactor)?;
        validate_fields(&self.fields)?;
        ensure!(
            redactor.clean(&serde_json::to_string(&self.fields)?),
            "task fields contain sensitive data"
        );
        Ok(())
    }
}

pub fn validate_context(context: &str, redactor: &Redactor) -> Result<()> {
    ensure!(
        context.len() <= 6000 && redactor.clean(context),
        "context must be at most 6000 bytes and contain no sensitive data"
    );
    Ok(())
}

pub fn validate_sources(settings: &Settings, map: &KnowledgeMap) -> Result<()> {
    if !settings.enabled {
        return Ok(());
    }
    ensure!(
        map.resources.iter().any(|source| source.enabled
            && (settings.sources.is_empty() || settings.sources.contains(&source.id))
            && matches!(
                &source.access,
                Access::Tool {
                    tool: ToolSpec::AzureDevopsStatus { .. }
                }
            )),
        "select an enabled Azure DevOps source for daily tasks"
    );
    Ok(())
}

fn activity_key(activity: &Linkable) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!(
            "{}|{}|{}",
            activity.organization, activity.url, activity.date
        ))
    )
}
