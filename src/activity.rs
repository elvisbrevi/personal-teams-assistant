//! Scheduled registration of verified work, with durable clarification and write records.
pub mod store;
#[cfg(test)]
mod tests;
use crate::{
    ado::{
        link::{LinkStatus, Linkable, Target},
        registration::{DailyWork, TaskFields},
    },
    config::Config,
    evidence::Evidence,
    knowledge::{Access, KnowledgeMap},
    llm::LlmProvider,
    security::Redactor,
    tools::{ReadOnlyTool, ToolSpec},
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Datelike, FixedOffset, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

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
            auto_register: true,
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
pub fn valid_field(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 150
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._".contains(c))
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
                .is_none_or(|s| s.len() <= 2000 && !s.chars().any(|c| c.is_control() && c != '\n')),
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
        if self.enabled {
            ensure!(
                !self.write_source.is_empty() && self.sources.contains(&self.write_source),
                "select a registration write source"
            );
        }
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
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
}
impl Resolution {
    pub fn validate(&self, redactor: &Redactor) -> Result<()> {
        ensure!(
            ["create", "refine", "dismiss"].contains(&self.action.as_str()),
            "invalid resolution action"
        );
        ensure!(
            self.context.len() <= 6000 && redactor.clean(&self.context),
            "invalid context"
        );
        validate_fields(&self.fields)?;
        ensure!(
            redactor.clean(&serde_json::to_string(&self.fields)?),
            "task fields contain sensitive data"
        );
        Ok(())
    }
}

fn already_linked(activity: &Linkable, work: &DailyWork) -> bool {
    work.linked_urls.contains(&activity.url)
        || activity
            .artifact
            .as_ref()
            .is_some_and(|url| work.linked_urls.contains(url))
}

pub fn validate_sources(settings: &Settings, map: &KnowledgeMap) -> Result<()> {
    if !settings.enabled {
        return Ok(());
    }
    for id in &settings.sources {
        let source = map
            .resources
            .iter()
            .find(|s| s.id == *id)
            .context("unknown registration source")?;
        ensure!(
            source.enabled && source.external_processing,
            "registration sources must be enabled and allow external processing"
        );
        ensure!(
            matches!(&source.access, Access::Tool { tool } if tool.reviews_activity()),
            "registration needs activity sources"
        );
    }
    if !settings.write_source.is_empty() {
        ensure!(
            map.resources.iter().any(|r| r.id == settings.write_source
                && settings.sources.contains(&r.id)
                && matches!(
                    &r.access,
                    Access::Tool {
                        tool: ToolSpec::AzureDevopsStatus {
                            link_secret_ref: Some(_),
                            ..
                        }
                    }
                )),
            "registration needs a separate Azure DevOps write credential"
        );
    }
    Ok(())
}

pub struct Recorder {
    pub config: Config,
    pub map: KnowledgeMap,
    pub db: Arc<store::Database>,
    pub tools: Arc<dyn ReadOnlyTool>,
    pub llm: Arc<dyn LlmProvider>,
    pub redactor: Arc<Redactor>,
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

impl Recorder {
    fn writer(&self) -> Result<&ToolSpec> {
        let s = &self.config.activity_registration;
        let mut scope = s.clone();
        scope.enabled = true;
        scope.validate()?;
        validate_sources(&scope, &self.map)?;
        let source = self
            .map
            .resources
            .iter()
            .find(|r| r.id == s.write_source)
            .context("write source unavailable")?;
        let Access::Tool { tool } = &source.access else {
            anyhow::bail!("invalid write source");
        };
        Ok(tool)
    }
    pub async fn daily_work(&self, day: NaiveDate) -> Result<DailyWork> {
        let settings = &self.config.activity_registration;
        let (start, end) = settings.bounds(day)?;
        self.tools
            .daily_work(
                self.writer()?,
                start,
                end.min(Utc::now()),
                &settings.effort_field,
                &day.to_string(),
            )
            .await
    }
    fn hours(&self, day: &str, work: &DailyWork) -> Result<f64> {
        let local = self
            .db
            .entries(Some(day))?
            .into_iter()
            .filter(|e| ["done", "uncertain", "sending"].contains(&e.state.as_str()))
            .filter(|e| e.task_id.is_none_or(|id| !work.task_ids.contains(&id)))
            .map(|e| e.draft.hours.unwrap_or(0.))
            .sum::<f64>();
        Ok(work.hours + local)
    }
    fn valid_draft(&self, draft: &Draft, parents: &[Target]) -> bool {
        draft.confidence.is_finite()
            && (0.0..=1.0).contains(&draft.confidence)
            && draft.title.chars().count() <= 160
            && draft.description.chars().count() <= 6000
            && draft.question.len() <= 1000
            && draft.suggested_parents.len() <= 5
            && draft
                .hours
                .is_none_or(|h| h.is_finite() && (0.01..=24.).contains(&h))
            && draft
                .parent
                .is_none_or(|id| parents.iter().any(|p| p.id == id))
            && draft
                .suggested_parents
                .iter()
                .all(|id| parents.iter().any(|p| p.id == *id))
            && [&draft.title, &draft.description, &draft.question]
                .iter()
                .all(|t| self.redactor.clean(t))
            && !draft.description.contains("://")
    }
    pub async fn run(&self, run: &str, day: NaiveDate) -> Result<Summary> {
        let writer = self.writer()?;
        let settings = &self.config.activity_registration;
        let work = self.daily_work(day).await?;
        let mut summary = Summary {
            recorded_hours: self.hours(&day.to_string(), &work)?,
            ..Default::default()
        };
        summary.remaining_hours = (settings.daily_hours - summary.recorded_hours).max(0.);
        let (start, end) = settings.bounds(day)?;
        let days = (Utc::now() - start).num_days() + 1;
        let mut offers = Vec::new();
        let mut evidence = String::new();
        for id in &settings.sources {
            let Some(source) = self.map.resources.iter().find(|r| r.id == *id) else {
                continue;
            };
            let Access::Tool { tool } = &source.access else {
                continue;
            };
            match self.tools.registration_review(tool, days).await {
                Ok(text) => {
                    let mut found: Evidence = serde_json::from_str(&text)?;
                    found.sanitize(&self.redactor);
                    summary.partial |= found.partial;
                    evidence.push_str(&found.text);
                    if let Some(offer) = found.links {
                        offers.push(offer);
                    }
                }
                Err(_) => {
                    summary.partial = true;
                    summary
                        .warnings
                        .push(format!("Could not read source {id}."));
                }
            }
        }
        let mut offer = crate::ado::link::LinkOffer::merge(offers);
        offer.activities.retain(|a| {
            let today = DateTime::parse_from_rfc3339(&a.occurred_at).map_or_else(
                |_| a.date == day.to_string(),
                |d| d >= start && d < end && d <= Utc::now(),
            );
            today && !already_linked(a, &work)
        });
        for a in &mut offer.activities {
            a.date = day.to_string();
        }
        // Recheck the local-day key after conversion from the source's UTC date.
        offer
            .activities
            .retain(|a| !self.db.claimed(&activity_key(a)).unwrap_or(true));
        let parents: Vec<_> = offer
            .work_items
            .into_iter()
            .chain(work.parents.clone())
            .filter(|p| p.holds_tasks() && p.organization == work.organization)
            .fold(BTreeMap::new(), |mut m, mut p| {
                p.title = self.redactor.redact(&p.title);
                m.insert(p.id, p);
                m
            })
            .into_values()
            .collect();
        if offer.activities.is_empty() {
            summary.pending = self
                .db
                .pending()?
                .iter()
                .filter(|e| e.day == day.to_string())
                .count();
            self.db.save_summary(&day.to_string(), &summary)?;
            return Ok(summary);
        }
        let evidence = self.redactor.redact(
            &evidence
                .chars()
                .take(self.config.policy.max_context_chars)
                .collect::<String>(),
        );
        let drafts = self
            .llm
            .plan_activity_tasks(
                &offer.activities,
                &parents,
                &evidence,
                summary.remaining_hours,
                "",
            )
            .await
            .unwrap_or_default();
        let mut used = BTreeSet::new();
        for draft in drafts {
            if !self.valid_draft(&draft, &parents)
                || draft.activities.is_empty()
                || draft.activities.len() > 20
                || draft.activities.iter().collect::<BTreeSet<_>>().len() != draft.activities.len()
            {
                continue;
            }
            let activities: Vec<_> = draft
                .activities
                .iter()
                .filter_map(|k| offer.activities.iter().find(|a| a.key == *k))
                .cloned()
                .collect();
            if activities.len() != draft.activities.len()
                || activities.iter().any(|a| used.contains(&a.key))
            {
                continue;
            }
            if draft.ignore
                && activities.iter().any(|a| {
                    !a.url.starts_with("https://teams.microsoft.com/") || a.context_required
                })
            {
                continue;
            }
            used.extend(activities.iter().map(|a| a.key.clone()));
            let mut entry = self.entry(run, day, draft, activities, &parents, &evidence);
            if entry.draft.ignore {
                entry.state = "dismissed".into();
            }
            if !self.db.insert(&entry)? {
                continue;
            }
            if settings.auto_register
                && !self.config.policy.dry_run
                && entry.draft.confidence >= 0.85
                && entry.draft.question.is_empty()
                && !entry.activities.iter().any(|a| a.context_required)
                && entry.state == "pending"
                && self.apply(&mut entry, writer).await.is_err()
                && entry.state == "pending"
            {
                entry.issue="Could not verify the task fields or HU. Review the source and try the pending activity again.".into();
                self.db.update(&entry, "pending")?;
            }
        }
        for a in offer
            .activities
            .into_iter()
            .filter(|a| !used.contains(&a.key))
        {
            // A plain chat message is context unless the model identifies concrete work.
            if a.url.starts_with("https://teams.microsoft.com/") && !a.context_required {
                continue;
            }
            let draft = Draft {activities:vec![a.key.clone()],parent:None,title:a.short.clone(),description:String::new(),hours:None,confidence:0.,question:"Which HU does this activity belong to, what did you do, and how long did it take?".into(),suggested_parents:parents.iter().take(3).map(|p|p.id).collect(),ignore:false};
            self.db
                .insert(&self.entry(run, day, draft, vec![a], &parents, &evidence))?;
        }
        let entries = self.db.entries(Some(&day.to_string()))?;
        summary.pending = entries.iter().filter(|e| e.state == "pending").count();
        summary.created = entries
            .iter()
            .filter(|e| e.run == run && e.state == "done")
            .count();
        summary.recorded_hours = self.hours(&day.to_string(), &work)?;
        summary.remaining_hours = (settings.daily_hours - summary.recorded_hours).max(0.);
        self.db.save_summary(&day.to_string(), &summary)?;
        Ok(summary)
    }
    fn entry(
        &self,
        run: &str,
        day: NaiveDate,
        draft: Draft,
        activities: Vec<Linkable>,
        parents: &[Target],
        evidence: &str,
    ) -> Entry {
        Entry {
            id: uuid::Uuid::new_v4().to_string(),
            run: run.into(),
            day: day.to_string(),
            state: "pending".into(),
            draft,
            activities,
            parents: parents.to_vec(),
            evidence: evidence.into(),
            context: String::new(),
            fields: self.config.activity_registration.extra_fields.clone(),
            issue: String::new(),
            task_id: None,
            task_url: None,
            updated_at: Utc::now().timestamp(),
        }
    }
    pub async fn resolve(&self, id: &str, resolution: Resolution) -> Result<Entry> {
        resolution.validate(&self.redactor)?;
        let mut entry = self.db.entry(id)?.context("activity entry not found")?;
        ensure!(entry.state == "pending", "activity is no longer pending");
        if resolution.action == "dismiss" {
            entry.state = "dismissed".into();
            self.db.update(&entry, "pending")?;
            if let Some(mut summary) = self.db.summary(&entry.day)? {
                summary.pending = self
                    .db
                    .pending()?
                    .iter()
                    .filter(|e| e.day == entry.day)
                    .count();
                self.db.save_summary(&entry.day, &summary)?;
            }
            return Ok(entry);
        }
        if let Some(parent) = resolution.parent {
            if !entry.parents.iter().any(|p| p.id == parent) {
                let found = self
                    .tools
                    .work_item(self.writer()?, parent)
                    .await?
                    .context("HU could not be verified")?;
                ensure!(found.holds_tasks(), "choose an open HU");
                entry.parents.push(found);
            }
            entry.draft.parent = Some(parent);
        }
        if let Some(title) = resolution.title {
            entry.draft.title = title;
        }
        if let Some(description) = resolution.description {
            entry.draft.description = description;
        }
        if let Some(hours) = resolution.hours {
            entry.draft.hours = Some(hours);
        }
        entry.fields.extend(resolution.fields);
        if !resolution.context.trim().is_empty() {
            entry
                .context
                .push_str(&format!("\n{}", resolution.context.trim()));
            ensure!(entry.context.len() <= 12000, "context too large");
        }
        ensure!(
            self.valid_draft(&entry.draft, &entry.parents),
            "invalid task details"
        );
        if resolution.action == "refine" {
            ensure!(
                !entry.context.trim().is_empty(),
                "provide context for the model"
            );
            let work = self.daily_work(entry.day.parse()?).await?;
            let remaining = (self.config.activity_registration.daily_hours
                - self.hours(&entry.day, &work)?)
            .max(0.);
            entry.state = "refining".into();
            self.db.update(&entry, "pending")?;
            let context =
                serde_json::json!({"user_context":entry.context,"current_draft":entry.draft})
                    .to_string();
            let result = self
                .llm
                .plan_activity_tasks(
                    &entry.activities,
                    &entry.parents,
                    &entry.evidence,
                    remaining,
                    &context,
                )
                .await;
            entry.state = "pending".into();
            if let Ok(drafts) = result
                && let Some(draft) = drafts.into_iter().find(|d| {
                    !d.ignore
                        && d.activities.iter().collect::<BTreeSet<_>>()
                            == entry
                                .activities
                                .iter()
                                .map(|a| &a.key)
                                .collect::<BTreeSet<_>>()
                        && self.valid_draft(d, &entry.parents)
                })
            {
                entry.draft = draft;
                entry.issue.clear();
            } else {
                entry.issue = "The model could not resolve the activity. Edit the details or add more context.".into();
            }
            self.db.update(&entry, "refining")?;
        } else {
            // A user's explicit resolution confirms purpose/attendance as well as effort.
            if self.apply(&mut entry, self.writer()?).await.is_err() && entry.state == "pending" {
                entry.issue="Could not verify the task fields or HU. Check the source and required fields before proceeding.".into();
                self.db.update(&entry, "pending")?;
            }
        }
        Ok(entry)
    }
    async fn apply(&self, entry: &mut Entry, writer: &ToolSpec) -> Result<()> {
        if self.config.policy.dry_run {
            entry.issue = "Observation mode: no task was created.".into();
            self.db.update(entry, "pending")?;
            return Ok(());
        }
        let Some(parent_id) = entry.draft.parent else {
            entry.issue = "Choose an HU before creating the task.".into();
            self.db.update(entry, "pending")?;
            return Ok(());
        };
        let Some(hours) = entry.draft.hours else {
            entry.issue = "Provide the effort in hours before creating the task.".into();
            self.db.update(entry, "pending")?;
            return Ok(());
        };
        let settings = &self.config.activity_registration;
        let work = self.daily_work(entry.day.parse()?).await?;
        let remaining = (settings.daily_hours - self.hours(&entry.day, &work)?).max(0.);
        if hours > remaining + 0.0001
            || entry.draft.title.trim().is_empty()
            || entry.draft.description.trim().is_empty()
        {
            entry.issue = format!(
                "Provide a title, description and effort within the remaining {remaining:.2} hours."
            );
            self.db.update(entry, "pending")?;
            return Ok(());
        }
        if entry.activities.iter().any(|a| already_linked(a, &work)) {
            entry.state = "already_registered".into();
            self.db.update(entry, "pending")?;
            return Ok(());
        }
        let parent = self
            .tools
            .work_item(writer, parent_id)
            .await?
            .context("HU could not be verified")?;
        ensure!(
            parent.holds_tasks() && parent.organization == work.organization,
            "parent is not an open HU in the authorized organization"
        );
        let fields = TaskFields {
            title: entry.draft.title.clone(),
            description: entry.draft.description.clone(),
            hours,
            day: entry.day.clone(),
            kind: settings.task_kind.clone(),
            state: settings.done_state.clone(),
            effort_field: settings.effort_field.clone(),
            extra: entry.fields.clone(),
        };
        let schema = self
            .tools
            .task_fields(writer, &parent, &settings.task_kind)
            .await?;
        if let Some(issue) = schema.issue(&fields) {
            entry.issue = issue;
            self.db.update(entry, "pending")?;
            return Ok(());
        }
        if !self.redactor.clean(&serde_json::to_string(&fields.extra)?) {
            anyhow::bail!("task fields contain sensitive data");
        }
        let mut activities = entry.activities.clone();
        for a in &mut activities {
            if a.organization == "https://teams.microsoft.com" {
                a.organization = parent.organization.clone();
            }
        }
        // Server-side conditional rules are checked without writing before the single write.
        if !self
            .tools
            .validate_task(writer, &parent, &fields, &activities)
            .await?
        {
            entry.issue="Azure DevOps rejected the proposed fields. Check the completed state and required fields; add them under Additional fields.".into();
            self.db.update(entry, "pending")?;
            return Ok(());
        }
        entry.state = "sending".into();
        entry.issue.clear();
        self.db.update(entry, "pending")?;
        let result = self
            .tools
            .register_task(writer, &parent, &fields, &activities)
            .await;
        entry.state = match result {
            Ok((LinkStatus::Linked, Some(id))) => {
                entry.task_id = Some(id);
                entry.task_url = Some(format!(
                    "{}/{}/_workitems/edit/{id}",
                    parent.organization.trim_end_matches('/'),
                    url::form_urlencoded::byte_serialize(parent.project.as_bytes())
                        .collect::<String>()
                        .replace('+', "%20")
                ));
                "done"
            }
            Ok((LinkStatus::Failed, _)) => "failed",
            // No successful response/body or a deadline means human inspection, never retry.
            _ => "uncertain",
        }
        .into();
        self.db.update(entry, "sending")?;
        let mut summary = self.db.summary(&entry.day)?.unwrap_or_default();
        summary.recorded_hours = self.hours(&entry.day, &work)?;
        summary.remaining_hours = (settings.daily_hours - summary.recorded_hours).max(0.);
        summary.pending = self
            .db
            .pending()?
            .iter()
            .filter(|e| e.day == entry.day)
            .count();
        self.db.save_summary(&entry.day, &summary)?;
        Ok(())
    }
}
