//! Scheduled registration of verified work, with durable clarification and write records.
pub mod store;
#[cfg(test)]
mod tests;
use crate::{
    ado::{
        link::{LinkStatus, Linkable, Target},
        registration::{DailyWork, LOOKBACK_DAYS, TaskFields},
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
        validate_context(&self.context, redactor)?;
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
    /// Text written for the user with an entry, in `llm.language` like the drafts.
    fn say(&self, english: impl Into<String>, spanish: impl Into<String>) -> String {
        self.config.llm.language.say(english, spanish)
    }
    pub fn error_detail(&self, error: &anyhow::Error) -> String {
        self.redactor
            .redact(&format!("{error:#}"))
            .chars()
            .take(4000)
            .collect()
    }
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
        let today = settings.zoned(Utc::now()).date_naive();
        ensure!(
            day <= today && (today - day).num_days() < LOOKBACK_DAYS,
            "choose a day within the last 14 days"
        );
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
            .context("Reading Azure DevOps daily effort and open HUs")
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
    /// Why a draft cannot be kept, told so the user can fix it; `None` when it is valid.
    fn draft_problem(&self, draft: &Draft, parents: &[Target]) -> Option<String> {
        let problem = if draft.title.chars().count() > 160 {
            self.say(
                "The title can have at most 160 characters.",
                "El título puede tener como máximo 160 caracteres.",
            )
        } else if draft.description.chars().count() > 6000 {
            self.say(
                "The description can have at most 6000 characters.",
                "La descripción puede tener como máximo 6000 caracteres.",
            )
        } else if draft
            .hours
            .is_some_and(|h| !h.is_finite() || !(0.01..=24.).contains(&h))
        {
            self.say(
                "The effort must be between 0.01 and 24 hours.",
                "El esfuerzo debe estar entre 0,01 y 24 horas.",
            )
        } else if draft.description.contains("://") {
            self.say(
                "Remove the links from the description: the activity's links are added to the task automatically.",
                "Quita los enlaces de la descripción: los enlaces de la actividad se agregan a la tarea automáticamente.",
            )
        } else if ![&draft.title, &draft.description, &draft.question]
            .iter()
            .all(|t| self.redactor.clean(t))
        {
            self.say(
                "The title or description contains data that looks sensitive (an email address, phone number, ID number, token or password). Remove it and try again.",
                "El título o la descripción contiene datos que parecen sensibles (un correo, un teléfono, un RUT, un token o una contraseña). Quítalos e inténtalo de nuevo.",
            )
        } else if draft
            .parent
            .is_some_and(|id| !parents.iter().any(|p| p.id == id))
        {
            self.say(
                "Choose one of the listed HUs or enter the ID of another HU or bug that is not closed.",
                "Elige una de las HU de la lista o ingresa el ID de otra HU o bug que no esté cerrado.",
            )
        } else if !draft.confidence.is_finite()
            || !(0.0..=1.0).contains(&draft.confidence)
            || draft.question.len() > 1000
            || draft.suggested_parents.len() > 5
            || !draft
                .suggested_parents
                .iter()
                .all(|id| parents.iter().any(|p| p.id == *id))
        {
            self.say(
                "The proposed task details are invalid.",
                "Los detalles propuestos para la tarea no son válidos.",
            )
        } else {
            return None;
        };
        Some(problem)
    }
    fn valid_draft(&self, draft: &Draft, parents: &[Target]) -> bool {
        self.draft_problem(draft, parents).is_none()
    }
    pub async fn run(&self, run: &str, day: NaiveDate, context: &str) -> Result<Summary> {
        validate_context(context, &self.redactor)?;
        self.db.describe_run(
            run,
            &serde_json::json!({
                "context":context,"settings":self.config.activity_registration,
                "provider":self.llm.name(),"observation_mode":self.config.policy.dry_run,
                "stage":"Checking registration settings"
            }),
        )?;
        let result = self.review(run, day, context).await;
        let error = result
            .as_ref()
            .err()
            .map(|e| self.error_detail(e))
            .unwrap_or_default();
        self.db.finish_run(run, result.as_ref().ok(), &error)?;
        result
    }
    async fn review(&self, run: &str, day: NaiveDate, context: &str) -> Result<Summary> {
        let writer = self.writer()?;
        let settings = &self.config.activity_registration;
        self.db
            .stage_run(run, "Reading daily effort and open HUs")?;
        let work = self.daily_work(day).await?;
        let mut summary = Summary {
            recorded_hours: self.hours(&day.to_string(), &work)?,
            ..Default::default()
        };
        summary.remaining_hours = (settings.daily_hours - summary.recorded_hours).max(0.);
        let (start, end) = settings.bounds(day)?;
        let days = LOOKBACK_DAYS;
        let mut offers = Vec::new();
        let mut evidence = String::new();
        for id in &settings.sources {
            let Some(source) = self.map.resources.iter().find(|r| r.id == *id) else {
                continue;
            };
            let Access::Tool { tool } = &source.access else {
                continue;
            };
            self.db.stage_run(run, &format!("Reading source {id}"))?;
            match self.tools.registration_review(tool, days).await {
                Ok(text) => {
                    let mut found: Evidence = serde_json::from_str(&text)
                        .with_context(|| format!("Invalid evidence from source {id}"))?;
                    found.sanitize(&self.redactor);
                    summary.partial |= found.partial;
                    if found.partial {
                        summary.warnings.push(self.say(
                            format!("Source {id} has incomplete coverage."),
                            format!("La fuente {id} tiene cobertura incompleta."),
                        ));
                    }
                    evidence.push_str(&found.text);
                    if let Some(offer) = found.links {
                        offers.push(offer);
                    }
                }
                Err(error) => {
                    summary.partial = true;
                    let detail = self.error_detail(&error);
                    summary.warnings.push(self.say(
                        format!("Could not read source {id}: {detail}"),
                        format!("No se pudo leer la fuente {id}: {detail}"),
                    ));
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
            self.db.stage_run(run, "Saving review results")?;
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
        self.db
            .stage_run(run, "Planning tasks with the configured agents")?;
        let drafts = self
            .llm
            .plan_activity_tasks(
                &offer.activities,
                &parents,
                &evidence,
                summary.remaining_hours,
                context,
            )
            .await;
        let drafts = match drafts {
            Ok(drafts) => drafts,
            Err(error) => {
                summary.partial = true;
                let (agents, detail) = (self.llm.name(), self.error_detail(&error));
                summary.warnings.push(self.say(
                    format!("Task planning failed ({agents}): {detail}. Activities with verified artifacts remain pending for manual review."),
                    format!("La planificación de tareas falló ({agents}): {detail}. Las actividades con artefactos verificados quedan pendientes para revisión manual."),
                ));
                Vec::new()
            }
        };
        self.db.stage_run(run, "Validating proposed tasks")?;
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
            entry.context = context.into();
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
                && let Err(error) = self.apply(&mut entry, writer).await
                && entry.state == "pending"
            {
                entry.issue = self.unverified(&error);
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
            let question = self.say(
                "Which HU does this activity belong to, what did you do, and how long did it take?",
                "¿A qué HU corresponde esta actividad, qué hiciste y cuánto tiempo te tomó?",
            );
            let draft = Draft {
                activities: vec![a.key.clone()],
                parent: None,
                title: a.short.clone(),
                description: String::new(),
                hours: None,
                confidence: 0.,
                question,
                suggested_parents: parents.iter().take(3).map(|p| p.id).collect(),
                ignore: false,
            };
            let mut entry = self.entry(run, day, draft, vec![a], &parents, &evidence);
            entry.context = context.into();
            self.db.insert(&entry)?;
        }
        self.db.stage_run(run, "Saving review results")?;
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
        // The user's edits stay with the entry, even when its HU still has to change.
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
            ensure!(
                entry.context.len() <= 12000,
                self.say(
                    "The context added to this activity is too long (at most 12000 bytes).",
                    "El contexto agregado a esta actividad es demasiado largo (máximo 12000 bytes).",
                )
            );
        }
        if let Some(parent) = resolution.parent {
            if !entry.parents.iter().any(|p| p.id == parent)
                && let Some(issue) = self.add_parent(&mut entry, parent).await?
            {
                if let Some(problem) = self.draft_problem(&entry.draft, &entry.parents) {
                    anyhow::bail!(problem);
                }
                entry.issue = issue;
                self.db.update(&entry, "pending")?;
                return Ok(entry);
            }
            entry.draft.parent = Some(parent);
        }
        if let Some(problem) = self.draft_problem(&entry.draft, &entry.parents) {
            anyhow::bail!(problem);
        }
        if resolution.action == "refine" {
            ensure!(
                !entry.context.trim().is_empty(),
                self.say(
                    "Write the context for the assistant first.",
                    "Primero escribe el contexto para el asistente.",
                )
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
            let error = result.as_ref().err().map(|e| self.error_detail(e));
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
                entry.issue = match error {
                    Some(e) => self.say(
                        format!("The assistant could not resolve the activity: {e}"),
                        format!("El asistente no pudo resolver la actividad: {e}"),
                    ),
                    None => self.say(
                        "The assistant could not resolve the activity. Edit the details or add more context.",
                        "El asistente no pudo resolver la actividad. Edita los detalles o agrega más contexto.",
                    ),
                };
            }
            self.db.update(&entry, "refining")?;
        } else {
            // A user's explicit resolution confirms purpose/attendance as well as effort.
            if let Err(error) = self.apply(&mut entry, self.writer()?).await
                && entry.state == "pending"
            {
                entry.issue = self.unverified(&error);
                self.db.update(&entry, "pending")?;
            }
        }
        Ok(entry)
    }
    fn unverified(&self, error: &anyhow::Error) -> String {
        let detail = self.error_detail(error);
        self.say(
            format!("Could not verify the task fields or HU: {detail}"),
            format!("No se pudieron verificar los campos de la tarea o la HU: {detail}"),
        )
    }
    /// Reads an HU or bug the user chose outside the offered ones and adds it. A work item that
    /// cannot hold tasks (a task, a feature, a closed HU or bug…) is explained instead, and its
    /// own parent, when that one can hold tasks, is offered and preselected; nothing is written.
    async fn add_parent(&self, entry: &mut Entry, id: u64) -> Result<Option<String>> {
        let writer = self.writer()?;
        let Some(item) = self.tools.work_item(writer, id).await? else {
            return Ok(Some(self.say(
                format!("Work item #{id} was not found in the authorized organization and projects. Check the ID or choose one of the listed HUs."),
                format!("No se encontró el work item #{id} en la organización y los proyectos autorizados. Revisa el ID o elige una de las HU de la lista."),
            )));
        };
        if item.holds_tasks() {
            entry.parents.push(item);
            return Ok(None);
        }
        let title = self.redactor.redact(&item.title);
        let mut issue = self.say(
            format!("#{id} «{title}» is a {} in state «{}», which cannot hold tasks. Tasks go under a user story, backlog item or bug that is not closed (new, active or in development); if it is related, mention #{id} in the description.", item.kind, item.state),
            format!("#{id} «{title}» es un {} en estado «{}», que no puede contener tareas. Las tareas van bajo una historia de usuario, un backlog item o un bug que no esté cerrado (nuevo, activo o en desarrollo); si está relacionado, menciona #{id} en la descripción.", item.kind, item.state),
        );
        if let Some(parent) = item.parent
            && let Ok(Some(mut hu)) = self.tools.work_item(writer, parent).await
            && hu.holds_tasks()
            && hu.organization == item.organization
        {
            hu.title = self.redactor.redact(&hu.title);
            issue.push(' ');
            issue.push_str(&self.say(
                format!(
                    "Its parent HU #{} «{}» is now selected: check it and create the task again.",
                    hu.id, hu.title
                ),
                format!(
                    "Quedó seleccionada su HU padre #{} «{}»: revísala y vuelve a crear la tarea.",
                    hu.id, hu.title
                ),
            ));
            entry.draft.parent = Some(hu.id);
            entry.draft.suggested_parents.retain(|p| *p != hu.id);
            entry.draft.suggested_parents.insert(0, hu.id);
            entry.draft.suggested_parents.truncate(5);
            if !entry.parents.iter().any(|p| p.id == hu.id) {
                entry.parents.push(hu);
            }
        }
        Ok(Some(issue))
    }
    /// Keeps the entry pending with an explanation of what is still missing.
    fn hold(&self, entry: &mut Entry, issue: String) -> Result<()> {
        entry.issue = issue;
        self.db.update(entry, "pending")
    }
    async fn apply(&self, entry: &mut Entry, writer: &ToolSpec) -> Result<()> {
        if self.config.policy.dry_run {
            let issue = self.say(
                "Observation mode is on, so no task was created. Turn it off in Settings → Answers to create tasks.",
                "El modo observación está activo, así que no se creó ninguna tarea. Desactívalo en Ajustes → Respuestas para crear tareas.",
            );
            return self.hold(entry, issue);
        }
        let Some(parent_id) = entry.draft.parent else {
            let issue = self.say(
                "Choose the HU or bug the task goes under before creating it.",
                "Elige la HU o el bug donde va la tarea antes de crearla.",
            );
            return self.hold(entry, issue);
        };
        let Some(hours) = entry.draft.hours else {
            let issue = self.say(
                "Enter the effort in hours before creating the task.",
                "Ingresa el esfuerzo en horas antes de crear la tarea.",
            );
            return self.hold(entry, issue);
        };
        if entry.draft.title.trim().is_empty() {
            let issue = self.say("Write the task title.", "Escribe el título de la tarea.");
            return self.hold(entry, issue);
        }
        if entry.draft.description.trim().is_empty() {
            let issue = self.say(
                "Describe the work done before creating the task.",
                "Describe el trabajo realizado antes de crear la tarea.",
            );
            return self.hold(entry, issue);
        }
        let settings = &self.config.activity_registration;
        let work = self.daily_work(entry.day.parse()?).await?;
        let remaining = (settings.daily_hours - self.hours(&entry.day, &work)?).max(0.);
        if hours > remaining + 0.0001 {
            let (target, day) = (settings.daily_hours, &entry.day);
            let issue = self.say(
                format!("The effort ({hours:.2} h) exceeds the {remaining:.2} h left of the {target} h daily target for {day}. Lower the effort or raise the daily target in Settings."),
                format!("El esfuerzo ({hours:.2} h) supera las {remaining:.2} h que quedan de la meta diaria de {target} h del {day}. Reduce el esfuerzo o aumenta la meta diaria en Ajustes."),
            );
            return self.hold(entry, issue);
        }
        if entry.activities.iter().any(|a| already_linked(a, &work)) {
            entry.state = "already_registered".into();
            let issue = self.say(
                "This activity is already linked to a task in Azure DevOps, so nothing was created.",
                "Esta actividad ya está vinculada a una tarea en Azure DevOps, así que no se creó nada.",
            );
            return self.hold(entry, issue);
        }
        let parent = self.tools.work_item(writer, parent_id).await?.with_context(|| {
            self.say(
                format!("HU #{parent_id} could not be read in the authorized organization and projects"),
                format!("No se pudo leer la HU #{parent_id} en la organización y los proyectos autorizados"),
            )
        })?;
        ensure!(
            parent.holds_tasks() && parent.organization == work.organization,
            self.say(
                format!("#{parent_id} can no longer hold tasks in the authorized organization (it is a {} in state «{}»)", parent.kind, parent.state),
                format!("#{parent_id} ya no puede contener tareas en la organización autorizada (es un {} en estado «{}»)", parent.kind, parent.state),
            )
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
        if let Some(issue) = schema.issue(&fields, self.config.llm.language) {
            return self.hold(entry, issue);
        }
        if !self.redactor.clean(&serde_json::to_string(&fields.extra)?) {
            anyhow::bail!(self.say(
                "the additional fields contain sensitive data",
                "los campos adicionales contienen datos sensibles",
            ));
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
            let issue = self.say(
                "Azure DevOps rejected the proposed fields. Check the completed state and required fields; add them under Additional fields.",
                "Azure DevOps rechazó los campos propuestos. Revisa el estado completado y los campos obligatorios; agrégalos en Campos adicionales.",
            );
            return self.hold(entry, issue);
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
            Ok((LinkStatus::Failed, _)) => {
                entry.issue = self.say(
                    "Azure DevOps rejected the task creation request. Inspect the completed state, required fields and write credential permissions before deciding how to proceed.",
                    "Azure DevOps rechazó la creación de la tarea. Revisa el estado completado, los campos obligatorios y los permisos de la credencial de escritura antes de decidir cómo seguir.",
                );
                "failed"
            }
            // No successful response/body or a deadline means human inspection, never retry.
            result => {
                entry.issue = match result {
                    Err(error) => {
                        let detail = self.error_detail(&error);
                        self.say(
                            format!("Task creation result is uncertain: {detail}. Inspect Azure DevOps; this write will not be retried."),
                            format!("El resultado de la creación de la tarea es incierto: {detail}. Revisa Azure DevOps; esta escritura no se reintentará."),
                        )
                    }
                    _ => self.say(
                        "Task creation result is uncertain. Inspect Azure DevOps; this write will not be retried.",
                        "El resultado de la creación de la tarea es incierto. Revisa Azure DevOps; esta escritura no se reintentará.",
                    ),
                };
                "uncertain"
            }
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
