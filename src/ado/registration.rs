//! Bounded reads for the daily effort budget and process schema; one completed-task write.
use super::{
    Catalog, field, item_reference,
    link::{LinkStatus, Linkable, Target},
    project_url, request, same_user, wiql, work_items,
};
use crate::config::Language;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const LOOKBACK_DAYS: i64 = 14;

#[derive(Clone, Debug, Default)]
pub struct DailyWork {
    pub organization: String,
    pub hours: f64,
    pub day: String,
    pub task_ids: BTreeSet<u64>,
    pub linked_urls: BTreeSet<String>,
    pub parents: Vec<Target>,
}
#[derive(Clone, Debug, Serialize)]
pub struct TaskFields {
    pub title: String,
    pub description: String,
    pub hours: f64,
    pub day: String,
    pub kind: String,
    pub state: String,
    pub effort_field: String,
    pub extra: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TaskSchema {
    pub fields: Vec<Field>,
    pub completed_states: Vec<String>,
    /// States a new task may be created in (the process transitions from none); empty when
    /// unknown, which allows creating it completed.
    #[serde(default)]
    pub initial_states: Vec<String>,
}
/// What Azure DevOps rules ask before a completed task can be registered, found without
/// writing (`validateOnly`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Check {
    /// Fields still empty that the rules require, with their metadata for the form.
    pub missing: Vec<Field>,
    /// Other rule errors, as Azure DevOps words them.
    pub errors: Vec<String>,
    /// The state a new task starts in when the process cannot create it completed; the
    /// registration then moves it to the completed state.
    pub through: Option<String>,
    /// Fields the completed state requires empty, such as remaining work.
    pub clear: Vec<String>,
}
/// The outcome of a task registration's writes.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    pub status: LinkStatus,
    pub id: Option<u64>,
    /// Azure DevOps' own explanation of a rejected write, when it gave one.
    pub detail: String,
}
/// One rule error of a rejected work item write.
struct Rule {
    field: String,
    flags: String,
    message: String,
}
/// The rule errors in a rejected write's body, or its message.
fn rules(body: &Value, status: u16) -> Vec<Rule> {
    let text = |value: &Value| {
        value
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect::<String>()
    };
    let custom = &body["customProperties"];
    match custom["RuleValidationErrors"]
        .as_array()
        .filter(|list| !list.is_empty())
    {
        Some(list) => list
            .iter()
            .take(10)
            .map(|rule| Rule {
                field: text(&rule["fieldReferenceName"]),
                flags: text(&rule["fieldStatusFlags"]).to_lowercase(),
                message: text(&rule["errorMessage"]),
            })
            .collect(),
        None => vec![Rule {
            field: text(&custom["FieldReferenceName"]),
            flags: text(&custom["FieldStatusFlags"]).to_lowercase(),
            message: Some(text(&body["message"]))
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| format!("HTTP {status}")),
        }],
    }
}
fn explain(rules: &[Rule]) -> String {
    rules
        .iter()
        .map(|r| r.message.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}
/// Sends one JSON Patch; a rejection returns its rule errors, a transport failure an error.
async fn send_patch(
    client: &Client,
    key: &str,
    method: Method,
    url: url::Url,
    patch: Value,
) -> Result<std::result::Result<Value, Vec<Rule>>> {
    let response = client
        .request(method, url)
        .basic_auth("", Some(key))
        .header("Content-Type", "application/json-patch+json")
        .body(patch.to_string())
        .send()
        .await?;
    let status = response.status();
    let body = crate::adapters::bounded_json::<Value>(response, 1_000_000).await;
    if status.is_success() {
        return body.map(Ok);
    }
    Ok(Err(rules(&body.unwrap_or(Value::Null), status.as_u16())))
}
/// A missing value (`invalidEmpty`) of a user field is asked for; anything else is an error.
fn classify(rule: Rule, missing: &mut BTreeSet<String>, errors: &mut Vec<String>) {
    if rule.flags.contains("invalidempty")
        && !rule.flags.contains("invalidnotempty")
        && !rule.field.is_empty()
        && !rule.field.starts_with("System.")
    {
        missing.insert(rule.field);
    } else if !errors.contains(&rule.message) {
        errors.push(rule.message);
    }
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Field {
    pub name: String,
    pub reference: String,
    pub required: bool,
    pub allowed: Vec<Value>,
    /// Azure DevOps data type (`string`, `integer`, `double`, `dateTime`, `boolean`,
    /// `picklistString`…), read only for the fields the user must fill; empty when unknown.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub help: String,
    /// The process default, which Azure DevOps applies when the task is created.
    #[serde(default)]
    pub default: Value,
}
const REMAINING_WORK: &str = "Microsoft.VSTS.Scheduling.RemainingWork";
/// The process fills a field with a default when the task is created without it.
fn has_default(field: &Field) -> bool {
    !field.default.is_null() && field.default.as_str() != Some("")
}
/// A required field the user supplies: Azure DevOps does not derive it and has no default for it.
fn user_supplied(field: &Field) -> bool {
    field.required && !field.reference.starts_with("System.") && !has_default(field)
}
impl TaskSchema {
    /// The configured effort field is not in this task type: name the hour fields it has, real
    /// or completed work first, so the user can pick the one their tasks use.
    fn missing_effort_field(&self, effort: &str, language: Language) -> String {
        let hours = |f: &&Field| {
            let text = format!("{} {}", f.name, f.reference).to_lowercase();
            ["work", "effort", "esfuerzo", "hora", "hour", "hh", "tiempo"]
                .iter()
                .any(|w| text.contains(w))
        };
        let mut candidates: Vec<_> = self
            .fields
            .iter()
            .filter(|f| !f.reference.starts_with("System.") && f.reference != REMAINING_WORK)
            .filter(hours)
            .collect();
        candidates.sort_by_key(|f| {
            let text = format!("{} {}", f.name, f.reference).to_lowercase();
            !["real", "complet", "actual"]
                .iter()
                .any(|w| text.contains(w))
        });
        let list = candidates
            .iter()
            .take(5)
            .map(|f| format!("{} ({})", f.name, f.reference))
            .collect::<Vec<_>>()
            .join(", ");
        let (english, spanish) = if list.is_empty() {
            (String::new(), String::new())
        } else {
            (
                format!(" This type has: {list}."),
                format!(" Este tipo tiene: {list}."),
            )
        };
        language.say(
            format!("This task type has no {effort} field, where the effort hours are written. In Settings → Activity registration → Effort field, set the field your tasks use for real hours.{english}"),
            format!("Este tipo de tarea no tiene el campo {effort}, donde se escriben las horas de esfuerzo. En Ajustes → Registro de actividades → Campo de esfuerzo, indica el campo que usan tus tareas para las horas reales.{spanish}"),
        )
    }
    /// The required fields the user fills in: not the system fields, the effort field or the
    /// remaining work that code sets, nor those the process gives a default.
    pub fn inputs(&self, effort_field: &str) -> Vec<Field> {
        self.fields
            .iter()
            .filter(|f| {
                user_supplied(f) && f.reference != effort_field && f.reference != REMAINING_WORK
            })
            .cloned()
            .collect()
    }
    /// What the configured fields still lack for this task type, told in `language`.
    pub fn issue(&self, task: &TaskFields, language: Language) -> Option<String> {
        if !self.completed_states.contains(&task.state) {
            let states = self.completed_states.join(", ");
            return Some(language.say(
                format!("Choose a completed state in Settings. Available: {states}."),
                format!("Elige un estado completado en Ajustes. Disponibles: {states}."),
            ));
        }
        if !self.fields.iter().any(|f| f.reference == task.effort_field) {
            return Some(self.missing_effort_field(&task.effort_field, language));
        }
        let mut values = task.extra.clone();
        values.insert(task.effort_field.clone(), json!(task.hours));
        for (key, value) in [
            ("System.Title", json!(task.title)),
            ("System.Description", json!(task.description)),
            ("System.State", json!(task.state)),
            ("System.AssignedTo", json!("configured author")),
            ("System.AreaPath", json!("parent area")),
            ("System.IterationPath", json!("parent iteration")),
        ] {
            values.insert(key.into(), value);
        }
        if self.fields.iter().any(|f| f.reference == REMAINING_WORK) {
            values.insert(REMAINING_WORK.into(), json!(0));
        }
        // System identity, path IDs and timestamps are supplied/derived by Azure DevOps, and
        // process defaults are applied on creation.
        let missing: Vec<_> = self
            .fields
            .iter()
            .filter(|f| {
                user_supplied(f)
                    && values
                        .get(&f.reference)
                        .is_none_or(|v| v.is_null() || v.as_str() == Some(""))
            })
            .map(|f| {
                if f.name.is_empty() {
                    &f.reference
                } else {
                    &f.name
                }
                .as_str()
            })
            .collect();
        if !missing.is_empty() {
            let missing = missing.join(", ");
            return Some(language.say(
                format!("Azure DevOps requires these fields: {missing}. Fill them in and create the task again."),
                format!("Azure DevOps exige estos campos: {missing}. Complétalos y vuelve a crear la tarea."),
            ));
        }
        for (key, value) in &values {
            if [
                "System.AssignedTo",
                "System.AreaPath",
                "System.IterationPath",
            ]
            .contains(&key.as_str())
            {
                continue;
            }
            let Some(field) = self.fields.iter().find(|f| f.reference == *key) else {
                return Some(language.say(
                    format!("Field {key} is unavailable for this task type."),
                    format!("El campo {key} no está disponible para este tipo de tarea."),
                ));
            };
            if !field.allowed.is_empty() && !field.allowed.contains(value) {
                let key = if field.name.is_empty() {
                    key
                } else {
                    &field.name
                };
                let allowed = field
                    .allowed
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Some(language.say(
                    format!("Choose an allowed value for {key}: {allowed}."),
                    format!("Elige un valor permitido para {key}: {allowed}."),
                ));
            }
        }
        None
    }
}
/// Effort increases within this day, not a task's cumulative effort from earlier days.
pub fn revision_hours(
    revisions: &[Value],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    effort: &str,
    author: &str,
) -> f64 {
    let mut previous = 0.;
    let mut total = 0.;
    for r in revisions {
        let hours = r["fields"][effort].as_f64().unwrap_or(0.);
        if let Ok(at) = DateTime::parse_from_rfc3339(field(r, "System.ChangedDate"))
            && at >= start
            && at < end
            && same_user(&r["fields"]["System.AssignedTo"], author)
        {
            total += hours - previous;
        }
        previous = hours;
    }
    total.max(0.)
}
pub async fn daily_work(
    client: &Client,
    key: &str,
    catalog_text: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    effort: &str,
    day: &str,
) -> Result<DailyWork> {
    let catalog = Catalog::parse(catalog_text)?.own(client, key).await?;
    ensure!(
        catalog.sources.len() == 1,
        "registration write source must select one organization"
    );
    let source = &catalog.sources[0];
    daily_work_in_source(client, key, source, start, end, effort, day).await
}
async fn daily_work_in_source(
    client: &Client,
    key: &str,
    source: &super::Source,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    effort: &str,
    day: &str,
) -> Result<DailyWork> {
    let now = Utc::now();
    let since = now - chrono::Duration::days(LOOKBACK_DAYS);
    let email = &source.author_email;
    let personal_scope = format!(
        "[System.ChangedDate] >= '{}' AND [System.ChangedDate] <= '{}' AND ([System.AssignedTo] = '{email}' OR [System.ChangedBy] = '{email}' OR [System.CreatedBy] = '{email}')",
        since.to_rfc3339(),
        now.to_rfc3339()
    );
    let mut out = DailyWork {
        organization: source.organization.clone(),
        day: day.into(),
        ..Default::default()
    };
    for project in &source.projects {
        let mut parent_ids = BTreeSet::new();
        let query = format!(
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND {personal_scope} AND ([System.WorkItemType] = 'Task' OR [System.WorkItemType] = 'Tarea') ORDER BY [System.ChangedDate] DESC"
        );
        let ids = wiql(client, key, source, project, &query).await?;
        ensure!(ids.len() < 80, "daily effort query reached its limit");
        let items = work_items(client, key, source, project, &ids.into_iter().collect()).await?;
        for item in items {
            parent_ids.extend(super::relation_ids(
                &item,
                "System.LinkTypes.Hierarchy-Reverse",
            ));
            let id = item["id"].as_u64().context("missing task id")?;
            for r in item["relations"].as_array().into_iter().flatten() {
                if let Some(url) = r["url"].as_str() {
                    out.linked_urls.insert(url.into());
                }
            }
            // Links on another user's task also prove the activity was registered;
            // only this user's effort contributes to their daily budget.
            if !same_user(&item["fields"]["System.AssignedTo"], &source.author_email) {
                continue;
            }
            let mut revisions = Vec::new();
            for page in 0..5 {
                let mut url = project_url(
                    source,
                    project,
                    "dev.azure.com",
                    &["_apis", "wit", "workitems", &id.to_string(), "revisions"],
                )?;
                url.query_pairs_mut()
                    .append_pair("$top", "200")
                    .append_pair("$skip", &(page * 200).to_string());
                let reply = request(client, key, Method::GET, url, None).await?;
                let rows = reply["value"].as_array().context("invalid revisions")?;
                revisions.extend(rows.iter().cloned());
                if rows.len() < 200 {
                    break;
                }
                ensure!(page < 4, "task revision limit reached");
            }
            let registered_day = field(&item, "System.Tags")
                .split(';')
                .find_map(|s| s.trim().strip_prefix("PTA-activity:"));
            if let Some(day) = registered_day {
                if day == out.day {
                    out.hours += item["fields"][effort].as_f64().unwrap_or(0.);
                }
            } else {
                out.hours += revision_hours(&revisions, start, end, effort, &source.author_email);
            }
            out.task_ids.insert(id);
        }
        let query = format!(
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND {personal_scope} AND ([System.WorkItemType] = 'User Story' OR [System.WorkItemType] = 'Product Backlog Item' OR [System.WorkItemType] = 'HU' OR [System.WorkItemType] = 'Requirement' OR [System.WorkItemType] = 'Issue' OR [System.WorkItemType] = 'Bug') ORDER BY [System.ChangedDate] DESC"
        );
        // Older HUs are context only when reached through the user's recent tasks.
        let ids = wiql(client, key, source, project, &query)
            .await?
            .into_iter()
            .chain(parent_ids.into_iter().take(40))
            .collect();
        for item in work_items(client, key, source, project, &ids).await? {
            if let Ok(reference) = item_reference(source, project, &item)
                && let Some(parent) = Target::from_item(&item, reference.url, &source.organization)
                && parent.holds_tasks()
            {
                out.parents.push(parent);
            }
        }
    }
    Ok(out)
}
pub async fn task_schema(
    client: &Client,
    key: &str,
    catalog_text: &str,
    parent: &Target,
    kind: &str,
) -> Result<TaskSchema> {
    let catalog = Catalog::parse(catalog_text)?;
    let source = catalog
        .sources
        .iter()
        .find(|s| s.organization == parent.organization)
        .context("task outside scope")?;
    let mut fields_url = project_url(
        source,
        &parent.project,
        "dev.azure.com",
        &["_apis", "wit", "workitemtypes", kind, "fields"],
    )?;
    fields_url.query_pairs_mut().append_pair("$expand", "All");
    let states_url = project_url(
        source,
        &parent.project,
        "dev.azure.com",
        &["_apis", "wit", "workitemtypes", kind, "states"],
    )?;
    let (fields, states) = tokio::try_join!(
        request(client, key, Method::GET, fields_url, None),
        request(client, key, Method::GET, states_url, None)
    )?;
    let fields = fields["value"]
        .as_array()
        .context("invalid task fields")?
        .iter()
        .filter_map(|f| {
            Some(Field {
                name: f["name"].as_str()?.into(),
                reference: f["referenceName"].as_str()?.into(),
                required: f["alwaysRequired"].as_bool().unwrap_or(false),
                allowed: f["allowedValues"].as_array().cloned().unwrap_or_default(),
                help: f["helpText"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(300)
                    .collect(),
                default: f["defaultValue"].clone(),
                kind: String::new(),
            })
        })
        .collect::<Vec<_>>();
    // The data type tells the GUI which input to show; a failed read only leaves it unknown.
    let mut fields = fields;
    for field in fields.iter_mut().filter(|f| user_supplied(f)).take(10) {
        let url = project_url(
            source,
            &parent.project,
            "dev.azure.com",
            &["_apis", "wit", "fields", &field.reference],
        )?;
        if let Ok(found) = request(client, key, Method::GET, url, None).await {
            field.kind = found["type"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(40)
                .collect();
        }
    }
    let completed_states = states["value"]
        .as_array()
        .context("invalid task states")?
        .iter()
        .filter(|s| s["category"].as_str() == Some("Completed"))
        .filter_map(|s| s["name"].as_str().map(str::to_owned))
        .collect();
    // Processes may only create a task in its first state; then it moves to the completed one.
    let type_url = project_url(
        source,
        &parent.project,
        "dev.azure.com",
        &["_apis", "wit", "workitemtypes", kind],
    )?;
    let initial_states = request(client, key, Method::GET, type_url, None)
        .await
        .ok()
        .and_then(|found| found["transitions"][""].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|t| t["to"].as_str())
        .filter(|state| !state.is_empty())
        .take(10)
        .map(str::to_owned)
        .collect();
    Ok(TaskSchema {
        fields,
        completed_states,
        initial_states,
    })
}
fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\n', "<br>")
}
pub fn task_patch(
    parent: &Target,
    author: &str,
    task: &TaskFields,
    activities: &[Linkable],
    schema: &TaskSchema,
) -> Value {
    let mut fields = task.extra.clone();
    fields.insert("System.Title".into(), json!(task.title));
    let links = activities
        .iter()
        .map(|a| {
            format!(
                "<li><a href=\"{}\">{}</a></li>",
                html(&a.url),
                html(&a.label)
            )
        })
        .collect::<String>();
    fields.insert(
        "System.Description".into(),
        json!(format!(
            "<p>{}</p><ul>{links}</ul>",
            html(&task.description)
        )),
    );
    fields.insert("System.AssignedTo".into(), json!(author));
    fields.insert("System.State".into(), json!(task.state));
    fields.insert(task.effort_field.clone(), json!(task.hours));
    if schema.fields.iter().any(|f| f.reference == "System.Tags") {
        let existing = fields
            .get("System.Tags")
            .and_then(Value::as_str)
            .unwrap_or("");
        fields.insert(
            "System.Tags".into(),
            json!(format!("{existing}; PTA-activity:{}", task.day).trim_start_matches("; ")),
        );
    }
    if schema
        .fields
        .iter()
        .any(|f| f.reference == "Microsoft.VSTS.Scheduling.RemainingWork")
    {
        fields.insert("Microsoft.VSTS.Scheduling.RemainingWork".into(), json!(0));
    }
    if !parent.area.is_empty() {
        fields.insert("System.AreaPath".into(), json!(parent.area));
    }
    if !parent.iteration.is_empty() {
        fields.insert("System.IterationPath".into(), json!(parent.iteration));
    }
    let mut patch: Vec<Value> = fields
        .into_iter()
        .map(|(field, value)| json!({"op":"add","path":format!("/fields/{field}"),"value":value}))
        .collect();
    patch.push(json!({"op":"add","path":"/relations/-","value":{"rel":"System.LinkTypes.Hierarchy-Reverse","url":format!("{}/_apis/wit/workItems/{}",parent.organization.trim_end_matches('/'),parent.id)}}));
    for a in activities {
        let relation = match (&a.artifact, &a.link_name) {
            (Some(url), Some(name)) => {
                json!({"rel":"ArtifactLink","url":url,"attributes":{"name":name}})
            }
            _ => {
                json!({"rel":"Hyperlink","url":a.url,"attributes":{"comment":"Registered by Personal Teams Assistant"}})
            }
        };
        patch.push(json!({"op":"add","path":"/relations/-","value":relation}));
    }
    Value::Array(patch)
}
fn task_source<'a>(
    catalog: &'a Catalog,
    parent: &Target,
    task: &TaskFields,
    activities: &[Linkable],
) -> Result<&'a super::Source> {
    ensure!(
        parent.holds_tasks() && super::link::TASK_KINDS.contains(&task.kind.as_str()),
        "invalid task parent or type"
    );
    catalog
        .sources
        .iter()
        .find(|s| {
            s.organization == parent.organization
                && activities.iter().all(|a| a.organization == s.organization)
        })
        .context("task outside scope")
}
fn create_url(
    source: &super::Source,
    parent: &Target,
    kind: &str,
    validate: bool,
) -> Result<url::Url> {
    let mut url = project_url(
        source,
        &parent.project,
        "dev.azure.com",
        &["_apis", "wit", "workitems", &format!("${kind}")],
    )?;
    if validate {
        url.query_pairs_mut().append_pair("validateOnly", "true");
    }
    Ok(url)
}
/// Asks Azure DevOps, without writing, what the completed task still needs. When the process
/// only creates tasks in an initial state, the completed state's rules are read from a
/// validation in that state (its state error is expected) and the creation is validated in the
/// initial state.
pub async fn check_task(
    client: &Client,
    key: &str,
    catalog_text: &str,
    parent: &Target,
    task: &TaskFields,
    activities: &[Linkable],
) -> Result<Check> {
    let catalog = Catalog::parse(catalog_text)?;
    let source = task_source(&catalog, parent, task, activities)?;
    let schema = task_schema(client, key, catalog_text, parent, &task.kind).await?;
    check_with(client, key, source, &schema, parent, task, activities).await
}
async fn check_with(
    client: &Client,
    key: &str,
    source: &super::Source,
    schema: &TaskSchema,
    parent: &Target,
    task: &TaskFields,
    activities: &[Linkable],
) -> Result<Check> {
    ensure!(
        schema.issue(task, Language::En).is_none(),
        "invalid task fields"
    );
    let mut check = Check {
        through: schema
            .initial_states
            .first()
            .filter(|_| !schema.initial_states.contains(&task.state))
            .cloned(),
        ..Default::default()
    };
    let mut missing = BTreeSet::new();
    let url = create_url(source, parent, &task.kind, true)?;
    let completed = task_patch(parent, &source.author_email, task, activities, schema);
    let rejected = send_patch(client, key, Method::POST, url.clone(), completed).await?;
    for rule in rejected.err().unwrap_or_default() {
        if check.through.is_some() {
            if rule.field == "System.State" || rule.message.contains("'State'") {
                continue;
            }
            if rule.flags.contains("invalidnotempty") && !rule.field.is_empty() {
                check.clear.push(rule.field);
                continue;
            }
        }
        classify(rule, &mut missing, &mut check.errors);
    }
    if let Some(initial) = &check.through {
        let first = TaskFields {
            state: initial.clone(),
            ..task.clone()
        };
        let created = task_patch(parent, &source.author_email, &first, activities, schema);
        let rejected = send_patch(client, key, Method::POST, url, created).await?;
        for rule in rejected.err().unwrap_or_default() {
            classify(rule, &mut missing, &mut check.errors);
        }
    }
    for reference in missing.into_iter().take(10) {
        let mut field = schema
            .fields
            .iter()
            .find(|f| f.reference == reference)
            .cloned()
            .unwrap_or_else(|| Field {
                name: reference.clone(),
                reference: reference.clone(),
                ..Default::default()
            });
        field.required = true;
        if field.kind.is_empty() {
            let url = project_url(
                source,
                &parent.project,
                "dev.azure.com",
                &["_apis", "wit", "fields", &reference],
            )?;
            if let Ok(found) = request(client, key, Method::GET, url, None).await {
                field.kind = found["type"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(40)
                    .collect();
            }
        }
        check.missing.push(field);
    }
    Ok(check)
}
/// The registration's writes: one creation, in the completed state or in the initial state the
/// check found, and then one move to the completed state that also clears the fields it requires
/// empty. Nothing is retried: a creation whose move fails is reported with its ID.
pub async fn write_task(
    client: &Client,
    key: &str,
    catalog_text: &str,
    parent: &Target,
    task: &TaskFields,
    activities: &[Linkable],
    check: &Check,
) -> Result<Written> {
    let catalog = Catalog::parse(catalog_text)?;
    let source = task_source(&catalog, parent, task, activities)?;
    let schema = task_schema(client, key, catalog_text, parent, &task.kind).await?;
    write_with(
        client, key, source, &schema, parent, task, activities, check,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
async fn write_with(
    client: &Client,
    key: &str,
    source: &super::Source,
    schema: &TaskSchema,
    parent: &Target,
    task: &TaskFields,
    activities: &[Linkable],
    check: &Check,
) -> Result<Written> {
    ensure!(
        schema.issue(task, Language::En).is_none(),
        "invalid task fields"
    );
    let written = |status, id, detail: String| Ok(Written { status, id, detail });
    let first = TaskFields {
        state: check.through.clone().unwrap_or_else(|| task.state.clone()),
        ..task.clone()
    };
    let url = create_url(source, parent, &task.kind, false)?;
    let patch = task_patch(parent, &source.author_email, &first, activities, schema);
    let created = match send_patch(client, key, Method::POST, url, patch).await {
        Err(_) => return written(LinkStatus::Uncertain, None, String::new()),
        Ok(Err(rules)) => return written(LinkStatus::Failed, None, explain(&rules)),
        Ok(Ok(created)) => created,
    };
    let Some(id) = created["id"].as_u64().filter(|id| *id > 0) else {
        return written(LinkStatus::Uncertain, None, String::new());
    };
    let completed = if check.through.is_some() {
        let mut ops = vec![json!({"op":"add","path":"/fields/System.State","value":task.state})];
        ops.extend(
            check
                .clear
                .iter()
                .map(|field| json!({"op":"remove","path":format!("/fields/{field}")})),
        );
        let url = project_url(
            source,
            &parent.project,
            "dev.azure.com",
            &["_apis", "wit", "workitems", &id.to_string()],
        )?;
        match send_patch(client, key, Method::PATCH, url, Value::Array(ops)).await {
            Err(_) => return written(LinkStatus::Uncertain, Some(id), String::new()),
            Ok(Err(rules)) => return written(LinkStatus::Uncertain, Some(id), explain(&rules)),
            Ok(Ok(updated)) => updated,
        }
    } else {
        created
    };
    if !completed_response(&completed, task) {
        return written(LinkStatus::Uncertain, Some(id), String::new());
    }
    written(LinkStatus::Linked, Some(id), String::new())
}

fn completed_response(created: &Value, task: &TaskFields) -> bool {
    created["id"].as_u64().is_some_and(|id| id > 0)
        && created["fields"]["System.State"].as_str() == Some(&task.state)
        && created["fields"][&task.effort_field]
            .as_f64()
            .is_some_and(|hours| (hours - task.hours).abs() < 0.0001)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn daily_search_is_personal_and_recent_with_related_parents() {
        use wiremock::{
            Mock, MockServer, Request, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        let source = super::super::Source {
            organization: server.uri(),
            projects: vec!["Payments".into()],
            author_email: "me@example.com".into(),
        };
        let start = (Utc::now() - chrono::Duration::days(1))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let end = start + chrono::Duration::days(1);
        let earliest = Utc::now() - chrono::Duration::days(14);
        Mock::given(method("POST"))
            .and(path("/Payments/_apis/wit/wiql"))
            .respond_with(|request: &Request| {
                let body: Value = request.body_json().unwrap();
                let id = if body["query"].as_str().unwrap().contains("= 'Task'") {
                    7
                } else {
                    40
                };
                ResponseTemplate::new(200).set_body_json(json!({"workItems":[{"id":id}]}))
            })
            .expect(2)
            .mount(&server)
            .await;
        let organization = server.uri();
        Mock::given(method("GET"))
            .and(path("/Payments/_apis/wit/workitems"))
            .respond_with(move |request: &Request| {
                let ids = request
                    .url
                    .query_pairs()
                    .find(|(name, _)| name == "ids")
                    .unwrap()
                    .1;
                let items: Vec<_> = ids
                    .split(',')
                    .map(|id| {
                        let id: u64 = id.parse().unwrap();
                        json!({"id":id,"fields":{
                        "System.TeamProject":"Payments","System.Title":"Verified work",
                        "System.WorkItemType":if id == 7 { "Task" } else { "User Story" },
                        "System.State":"Active","System.AssignedTo":{"uniqueName":"me@example.com"}
                    },"relations":if id == 7 { vec![json!({
                        "rel":"System.LinkTypes.Hierarchy-Reverse",
                        "url":format!("{organization}/_apis/wit/workItems/41")
                    })] } else { vec![] }})
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({"value":items}))
            })
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Payments/_apis/wit/workitems/7/revisions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"fields":{"System.ChangedDate":(start - chrono::Duration::days(1)).to_rfc3339(),
                    "System.AssignedTo":{"uniqueName":"me@example.com"},
                    "Microsoft.VSTS.Scheduling.CompletedWork":5.0}},
                {"fields":{"System.ChangedDate":(start + chrono::Duration::hours(9)).to_rfc3339(),
                    "System.AssignedTo":{"uniqueName":"me@example.com"},
                    "Microsoft.VSTS.Scheduling.CompletedWork":6.5}}
            ]})))
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::builder()
            .no_proxy()
            .resolve("dev.azure.com", *server.address())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let work = daily_work_in_source(
            &client,
            "test-token",
            &source,
            start,
            end,
            "Microsoft.VSTS.Scheduling.CompletedWork",
            &start.date_naive().to_string(),
        )
        .await
        .unwrap();
        assert_eq!(work.hours, 1.5);
        let latest = Utc::now() - chrono::Duration::days(14);
        let requests = server.received_requests().await.unwrap();
        let pattern = regex::Regex::new(r"\[System\.ChangedDate\] >= '([^']+)'").unwrap();
        for request in requests.iter().filter(|r| r.method == "POST") {
            assert!(
                request
                    .url
                    .query_pairs()
                    .any(|(k, v)| k == "timePrecision" && v == "true")
            );
            let body: Value = request.body_json().unwrap();
            let query = body["query"].as_str().unwrap();
            for field in ["System.AssignedTo", "System.ChangedBy", "System.CreatedBy"] {
                assert!(query.contains(&format!("[{field}] = 'me@example.com'")));
            }
            let captured = pattern.captures(query).unwrap();
            let since = DateTime::parse_from_rfc3339(&captured[1]).unwrap();
            assert!(
                since >= earliest && since <= latest,
                "exactly fourteen days of context"
            );
        }
        assert!(
            requests
                .iter()
                .any(|r| r.url.query_pairs().any(|(k, v)| k == "ids" && v == "40,41")),
            "fetch the related HU by ID even when it is outside the recent personal search"
        );
    }
    #[test]
    fn the_user_fills_only_required_fields_without_a_default_or_code_value() {
        let field = |reference: &str, name: &str, default: Value| Field {
            name: name.into(),
            reference: reference.into(),
            required: true,
            default,
            ..Default::default()
        };
        let effort = "Microsoft.VSTS.Scheduling.CompletedWork";
        let schema = TaskSchema {
            fields: vec![
                field("System.Title", "Title", Value::Null),
                field(effort, "Completed Work", Value::Null),
                field(
                    "Microsoft.VSTS.Scheduling.RemainingWork",
                    "Remaining Work",
                    Value::Null,
                ),
                field("Microsoft.VSTS.Common.Activity", "Activity", Value::Null),
                field("Custom.Mes", "Mes", json!("")),
                field("Custom.Area", "Área", json!("Pagos")),
            ],
            completed_states: vec!["Done".into()],
            initial_states: Vec::new(),
        };
        let inputs: Vec<_> = schema
            .inputs(effort)
            .into_iter()
            .map(|f| f.reference)
            .collect();
        assert_eq!(inputs, ["Microsoft.VSTS.Common.Activity", "Custom.Mes"]);
        let task = TaskFields {
            title: "Diagnose".into(),
            description: "Diagnosed it.".into(),
            hours: 1.,
            day: "2026-10-13".into(),
            kind: "Task".into(),
            state: "Done".into(),
            effort_field: effort.into(),
            extra: BTreeMap::new(),
        };
        // Friendly names, as the form shows them; the defaulted field is not asked for.
        assert_eq!(
            schema.issue(&task, Language::En).unwrap(),
            "Azure DevOps requires these fields: Activity, Mes. Fill them in and create the task again."
        );
    }

    #[test]
    fn a_missing_effort_field_names_the_hour_fields_the_type_has() {
        let field = |reference: &str, name: &str| Field {
            name: name.into(),
            reference: reference.into(),
            ..Default::default()
        };
        let schema = TaskSchema {
            fields: vec![
                field("System.Title", "Title"),
                field("Microsoft.VSTS.Scheduling.RemainingWork", "Remaining Work"),
                field("Custom.Estimatedwork", "Estimated work"),
                field("Custom.EsfuerzoReal", "Esfuerzo Real"),
                field("Custom.Mes", "Mes"),
            ],
            completed_states: vec!["Done".into()],
            initial_states: Vec::new(),
        };
        let task = TaskFields {
            title: "Diagnose".into(),
            description: "Diagnosed it.".into(),
            hours: 2.,
            day: "2026-10-13".into(),
            kind: "Task".into(),
            state: "Done".into(),
            effort_field: "Microsoft.VSTS.Scheduling.CompletedWork".into(),
            extra: BTreeMap::new(),
        };
        let issue = schema.issue(&task, Language::Es).unwrap();
        assert!(
            issue.contains("Microsoft.VSTS.Scheduling.CompletedWork"),
            "{issue}"
        );
        assert!(issue.contains("Campo de esfuerzo"), "{issue}");
        assert!(
            issue.ends_with("Este tipo tiene: Esfuerzo Real (Custom.EsfuerzoReal), Estimated work (Custom.Estimatedwork)."),
            "{issue}"
        );
    }

    #[tokio::test]
    async fn a_process_that_creates_tasks_waiting_completes_them_in_a_second_write() {
        use wiremock::{
            Mock, MockServer, Request, ResponseTemplate,
            matchers::{method, path, path_regex},
        };
        let server = MockServer::start().await;
        let source = super::super::Source {
            organization: server.uri(),
            projects: vec!["Payments".into()],
            author_email: "me@example.com".into(),
        };
        // Tasks start «En espera»; Done needs URL Commit and an empty Remaining Work.
        Mock::given(method("POST"))
            .and(path_regex(r"/Payments/_apis/wit/workitems/.*Task$"))
            .respond_with(|request: &Request| {
                let patch: Value = request.body_json().unwrap();
                let value = |field: &str| {
                    patch.as_array().unwrap().iter()
                        .find(|op| op["path"] == format!("/fields/{field}"))
                        .map(|op| op["value"].clone())
                };
                let validate = request.url.query_pairs().any(|(k, v)| k == "validateOnly" && v == "true");
                if value("System.State") == Some(json!("Done")) {
                    let mut rules = vec![
                        json!({"fieldReferenceName":null,"fieldStatusFlags":"none","errorMessage":"The field 'State' contains the value 'Done' that is not in the list of supported values"}),
                        json!({"fieldReferenceName":"Microsoft.VSTS.Scheduling.RemainingWork","fieldStatusFlags":"required, invalidNotEmpty","errorMessage":"TF401320: Rule Error for field Remaining Work. Error code: Required, InvalidNotEmpty."}),
                    ];
                    if value("Custom.URLCommit").is_none() {
                        rules.push(json!({"fieldReferenceName":"Custom.URLCommit","fieldStatusFlags":"required, invalidEmpty","errorMessage":"TF401320: Rule Error for field URL Commit. Error code: Required, InvalidEmpty."}));
                    }
                    return ResponseTemplate::new(400).set_body_json(json!({"message":"The field 'State' contains the value 'Done' that is not in the list of supported values.","customProperties":{"RuleValidationErrors":rules}}));
                }
                assert_eq!(value("System.State"), Some(json!("En espera")));
                assert_eq!(value("Microsoft.VSTS.Scheduling.RemainingWork"), Some(json!(0)));
                let id = if validate { 0 } else { 900 };
                ResponseTemplate::new(200).set_body_json(json!({"id":id,"fields":{"System.State":"En espera"}}))
            })
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Payments/_apis/wit/fields/Custom.URLCommit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"type":"string"})))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/Payments/_apis/wit/workitems/900"))
            .respond_with(|request: &Request| {
                let patch: Value = request.body_json().unwrap();
                assert_eq!(
                    patch,
                    json!([
                        {"op":"add","path":"/fields/System.State","value":"Done"},
                        {"op":"remove","path":"/fields/Microsoft.VSTS.Scheduling.RemainingWork"}
                    ])
                );
                ResponseTemplate::new(200).set_body_json(
                    json!({"id":900,"fields":{"System.State":"Done","Custom.EsfuerzoReal":2.0}}),
                )
            })
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::builder()
            .no_proxy()
            .resolve("dev.azure.com", *server.address())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let field = |reference: &str, name: &str| Field {
            name: name.into(),
            reference: reference.into(),
            ..Default::default()
        };
        let schema = TaskSchema {
            fields: [
                "System.Title",
                "System.Description",
                "System.State",
                "System.Tags",
            ]
            .iter()
            .map(|r| field(r, r))
            .chain([
                field("Custom.EsfuerzoReal", "Esfuerzo Real"),
                field("Microsoft.VSTS.Scheduling.RemainingWork", "Remaining Work"),
                field("Custom.URLCommit", "URL Commit"),
            ])
            .collect(),
            completed_states: vec!["Done".into()],
            initial_states: vec!["En espera".into()],
        };
        let parent = Target {
            id: 26321,
            title: "Servicio no disponible".into(),
            organization: server.uri(),
            project: "Payments".into(),
            kind: "Bug".into(),
            state: "En progreso".into(),
            ..Default::default()
        };
        let mut task = TaskFields {
            title: "Diagnose the payment query".into(),
            description: "Diagnosed the failure.".into(),
            hours: 2.,
            day: "2026-10-13".into(),
            kind: "Task".into(),
            state: "Done".into(),
            effort_field: "Custom.EsfuerzoReal".into(),
            extra: BTreeMap::new(),
        };
        let check = check_with(&client, "k", &source, &schema, &parent, &task, &[])
            .await
            .unwrap();
        assert_eq!(check.through.as_deref(), Some("En espera"));
        assert_eq!(check.clear, ["Microsoft.VSTS.Scheduling.RemainingWork"]);
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        assert_eq!(check.missing.len(), 1);
        assert_eq!(check.missing[0].name, "URL Commit");
        assert_eq!(check.missing[0].kind, "string");

        task.extra.insert("Custom.URLCommit".into(), json!("N/A"));
        let check = check_with(&client, "k", &source, &schema, &parent, &task, &[])
            .await
            .unwrap();
        assert!(check.missing.is_empty() && check.errors.is_empty());
        let written = write_with(&client, "k", &source, &schema, &parent, &task, &[], &check)
            .await
            .unwrap();
        assert_eq!(
            written,
            Written {
                status: LinkStatus::Linked,
                id: Some(900),
                detail: String::new()
            }
        );
    }

    #[test]
    fn daily_effort_counts_changes_only_inside_the_day() {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let revision = |date: &str, hours: f64| json!({"fields":{"System.ChangedDate":date,"System.AssignedTo":{"uniqueName":"me@example.com"},"Microsoft.VSTS.Scheduling.CompletedWork":hours}});
        let revisions = vec![
            revision("2026-10-06T14:00:00Z", 5.),
            revision("2026-10-07T09:00:00Z", 7.),
            revision("2026-10-07T12:00:00Z", 6.),
            revision("2026-10-08T10:00:00Z", 9.),
        ];
        assert_eq!(
            revision_hours(
                &revisions,
                at("2026-10-07T00:00:00Z"),
                at("2026-10-08T00:00:00Z"),
                "Microsoft.VSTS.Scheduling.CompletedWork",
                "me@example.com"
            ),
            1.
        );
    }
    #[test]
    fn completed_patch_keeps_effort_required_fields_and_verified_links() {
        let parent = Target {
            id: 40,
            organization: "https://dev.azure.com/example".into(),
            area: "P\\Team".into(),
            iteration: "P\\Sprint".into(),
            ..Default::default()
        };
        let task = TaskFields {
            title: "Document the API".into(),
            description: "Created <script>documentation</script>".into(),
            hours: 2.,
            day: "2026-10-07".into(),
            kind: "Task".into(),
            state: "Done".into(),
            effort_field: "Microsoft.VSTS.Scheduling.CompletedWork".into(),
            extra: BTreeMap::from([("Custom.Activity".into(), json!("Documentation"))]),
        };
        let schema = TaskSchema {
            fields: ["System.Tags", "Microsoft.VSTS.Scheduling.RemainingWork"]
                .into_iter()
                .map(|name| Field {
                    reference: name.into(),
                    ..Default::default()
                })
                .collect(),
            completed_states: vec!["Done".into()],
            initial_states: Vec::new(),
        };
        let activity = Linkable {
            url: "https://dev.azure.com/example/P/_wiki/wikis/docs?pagePath=api".into(),
            label: "API guide".into(),
            ..Default::default()
        };
        let patch = task_patch(&parent, "me@example.com", &task, &[activity], &schema);
        let value = |field: &str| {
            patch
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["path"] == format!("/fields/{field}"))
                .unwrap()["value"]
                .clone()
        };
        assert_eq!(value("System.State"), "Done");
        assert_eq!(value("Microsoft.VSTS.Scheduling.CompletedWork"), 2.);
        assert_eq!(value("Microsoft.VSTS.Scheduling.RemainingWork"), 0);
        assert_eq!(value("Custom.Activity"), "Documentation");
        assert_eq!(value("System.AssignedTo"), "me@example.com");
        assert_eq!(value("System.Tags"), "PTA-activity:2026-10-07");
        let response = json!({"id":501,"fields":{"System.State":"Done","Microsoft.VSTS.Scheduling.CompletedWork":2.}});
        assert!(completed_response(&response, &task));
        let mut changed = response.clone();
        changed["fields"]["System.State"] = json!("Active");
        assert!(!completed_response(&changed, &task));
        changed["fields"]["System.State"] = json!("Done");
        changed["fields"]["Microsoft.VSTS.Scheduling.CompletedWork"] = json!(1.);
        assert!(!completed_response(&changed, &task));
        let description = value("System.Description");
        assert!(!description.as_str().unwrap().contains("<script>"));
        assert!(description.as_str().unwrap().contains("API guide"));
        assert!(
            patch
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["value"]["rel"] == "System.LinkTypes.Hierarchy-Reverse")
        );
    }
}
