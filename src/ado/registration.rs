//! Bounded reads for the daily effort budget and process schema; one completed-task write.
use super::{
    Catalog, field, item_reference,
    link::{LinkStatus, Linkable, Target},
    project_url, request, same_user, wiql, work_items,
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Field {
    pub name: String,
    pub reference: String,
    pub required: bool,
    pub allowed: Vec<Value>,
}
impl TaskSchema {
    pub fn issue(&self, task: &TaskFields) -> Option<String> {
        if !self.completed_states.contains(&task.state) {
            return Some(format!(
                "Choose a completed state. Available: {}.",
                self.completed_states.join(", ")
            ));
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
        if self
            .fields
            .iter()
            .any(|f| f.reference == "Microsoft.VSTS.Scheduling.RemainingWork")
        {
            values.insert("Microsoft.VSTS.Scheduling.RemainingWork".into(), json!(0));
        }
        // System identity, path IDs and timestamps are supplied/derived by Azure DevOps.
        let missing: Vec<_> = self
            .fields
            .iter()
            .filter(|f| {
                f.required
                    && !f.reference.starts_with("System.")
                    && values
                        .get(&f.reference)
                        .is_none_or(|v| v.is_null() || v.as_str() == Some(""))
            })
            .map(|f| f.reference.as_str())
            .collect();
        if !missing.is_empty() {
            return Some(format!(
                "Required fields: {}. Add their values under Additional fields.",
                missing.join(", ")
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
                return Some(format!("Field {key} is unavailable for this task type."));
            };
            if !field.allowed.is_empty() && !field.allowed.contains(value) {
                return Some(format!(
                    "Choose an allowed value for {key}: {}.",
                    field
                        .allowed
                        .iter()
                        .map(Value::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
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
    let mut out = DailyWork {
        organization: source.organization.clone(),
        day: day.into(),
        ..Default::default()
    };
    for project in &source.projects {
        let query = format!(
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND [System.ChangedDate] >= '{}' AND ([System.WorkItemType] = 'Task' OR [System.WorkItemType] = 'Tarea') ORDER BY [System.ChangedDate] DESC",
            start.to_rfc3339()
        );
        let ids = wiql(client, key, source, project, &query).await?;
        ensure!(ids.len() < 80, "daily effort query reached its limit");
        let items = work_items(client, key, source, project, &ids.into_iter().collect()).await?;
        for item in items {
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
        let query = "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND ([System.WorkItemType] = 'User Story' OR [System.WorkItemType] = 'Product Backlog Item' OR [System.WorkItemType] = 'HU' OR [System.WorkItemType] = 'Requirement') ORDER BY [System.ChangedDate] DESC";
        let ids = wiql(client, key, source, project, query).await?;
        for item in work_items(client, key, source, project, &ids.into_iter().collect()).await? {
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
            })
        })
        .collect();
    let completed_states = states["value"]
        .as_array()
        .context("invalid task states")?
        .iter()
        .filter(|s| s["category"].as_str() == Some("Completed"))
        .filter_map(|s| s["name"].as_str().map(str::to_owned))
        .collect();
    Ok(TaskSchema {
        fields,
        completed_states,
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
pub async fn write_task(
    client: &Client,
    key: &str,
    catalog_text: &str,
    parent: &Target,
    task: &TaskFields,
    activities: &[Linkable],
    validate: bool,
) -> Result<(LinkStatus, Option<u64>)> {
    ensure!(
        parent.holds_tasks() && super::link::TASK_KINDS.contains(&task.kind.as_str()),
        "invalid task parent or type"
    );
    let catalog = Catalog::parse(catalog_text)?;
    let source = catalog
        .sources
        .iter()
        .find(|s| {
            s.organization == parent.organization
                && activities.iter().all(|a| a.organization == s.organization)
        })
        .context("task outside scope")?;
    let schema = task_schema(client, key, catalog_text, parent, &task.kind).await?;
    ensure!(schema.issue(task).is_none(), "invalid task fields");
    let mut url = project_url(
        source,
        &parent.project,
        "dev.azure.com",
        &["_apis", "wit", "workitems", &format!("${}", task.kind)],
    )?;
    if validate {
        url.query_pairs_mut().append_pair("validateOnly", "true");
    }
    let body = task_patch(parent, &source.author_email, task, activities, &schema);
    let Ok(response) = client
        .post(url)
        .basic_auth("", Some(key))
        .header("Content-Type", "application/json-patch+json")
        .body(body.to_string())
        .send()
        .await
    else {
        return Ok((LinkStatus::Uncertain, None));
    };
    if !response.status().is_success() {
        return Ok((LinkStatus::Failed, None));
    }
    let Ok(created) = crate::adapters::bounded_json::<Value>(response, 1_000_000).await else {
        return Ok((LinkStatus::Uncertain, None));
    };
    let id = created["id"].as_u64().filter(|id| *id > 0);
    if !validate && !completed_response(&created, task) {
        return Ok((LinkStatus::Uncertain, id));
    }
    Ok((LinkStatus::Linked, id))
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
