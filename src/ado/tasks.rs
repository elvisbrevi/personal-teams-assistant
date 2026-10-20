//! Daily task reads within the configured catalog, including historical assignments.
use super::*;
use crate::{
    activity::Settings,
    tools::{
        tasks::{Day, Query, Task},
        todo::{Date, Item},
    },
};
use chrono::DateTime;

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn item(source: &Source, project: &str, value: &Value) -> Result<Item> {
    let reference = item_reference(source, project, value)?;
    Ok(Item {
        id: value["id"].as_u64().context("missing task ID")?,
        title: field(value, "System.Title").into(),
        url: reference.url,
        project: project.into(),
        state: field(value, "System.State").into(),
        kind: field(value, "System.WorkItemType").into(),
        created_at: todo::timestamp(&value["fields"]["System.CreatedDate"]),
        dates: [
            "Microsoft.VSTS.Scheduling.StartDate",
            "Microsoft.VSTS.Scheduling.FinishDate",
            "Microsoft.VSTS.Common.ClosedDate",
        ]
        .into_iter()
        .filter_map(|name| {
            Some(Date {
                field: name.into(),
                name: name.rsplit('.').next()?.into(),
                value: Some(todo::timestamp(&value["fields"][name])?),
            })
        })
        .collect(),
    })
}

pub async fn list(
    client: &Client,
    key: &str,
    catalog: &str,
    cache: &Store,
    query: &Query,
    settings: &Settings,
) -> Result<Day> {
    let day = query.validate()?;
    let (start, end) = settings.bounds(day)?;
    ensure!(start <= Utc::now(), "invalid future day");
    let catalog = Catalog::parse(catalog)?.own(client, key).await?;
    let mut out = Day {
        day: query.day.clone(),
        time_zone: settings.time_zone.clone(),
        target_hours: settings.daily_hours,
        ..Default::default()
    };
    let mut matched_project = query.project.is_none();
    for source in &catalog.sources {
        let user = query.user.as_deref().unwrap_or(&source.author_email);
        for project in &source.projects {
            if query
                .project
                .as_ref()
                .is_some_and(|p| !p.eq_ignore_ascii_case(project))
            {
                continue;
            }
            matched_project = true;
            let category = todo::metadata(
                client,
                key,
                cache,
                project_url(
                    source,
                    project,
                    "dev.azure.com",
                    &[
                        "_apis",
                        "wit",
                        "workitemtypecategories",
                        "Microsoft.TaskCategory",
                    ],
                )?,
            )
            .await?;
            let kinds: Vec<_> = category["workItemTypes"]
                .as_array()
                .context("invalid task category")?
                .iter()
                .filter_map(|k| k["name"].as_str())
                .collect();
            ensure!(!kinds.is_empty() && kinds.len() <= 40, "invalid task types");
            let mut completed = BTreeSet::new();
            for kind in &kinds {
                let definition = todo::metadata(
                    client,
                    key,
                    cache,
                    project_url(
                        source,
                        project,
                        "dev.azure.com",
                        &["_apis", "wit", "workitemtypes", kind],
                    )?,
                )
                .await?;
                for state in definition["states"]
                    .as_array()
                    .context("invalid task states")?
                {
                    if state["category"] == "Completed" {
                        completed.insert(
                            state["name"]
                                .as_str()
                                .context("invalid completed state")?
                                .to_owned(),
                        );
                    }
                }
            }
            // EVER keeps tasks handed to someone else later. Exact day and assignment are
            // checked against revisions below; a later edit cannot move work to another day.
            let wiql = format!(
                "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = {} AND [System.WorkItemType] IN ({}) AND [System.AssignedTo] EVER {} AND [System.ChangedDate] >= {} ORDER BY [System.Id]",
                literal(project),
                kinds
                    .iter()
                    .map(|k| literal(k))
                    .collect::<Vec<_>>()
                    .join(","),
                literal(user),
                literal(&start.to_rfc3339())
            );
            let mut url = project_url(source, project, "dev.azure.com", &["_apis", "wit", "wiql"])?;
            url.query_pairs_mut()
                .append_pair("$top", "201")
                .append_pair("timePrecision", "true");
            let found =
                request(client, key, Method::POST, url, Some(json!({"query":wiql}))).await?;
            let ids: BTreeSet<_> = found["workItems"]
                .as_array()
                .context("invalid task query")?
                .iter()
                .filter_map(|v| v["id"].as_u64())
                .collect();
            ensure!(
                ids.len() <= 200,
                "daily task query reached its limit; select a project"
            );
            for id in ids {
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
                        .append_pair("$skip", &(page * 200).to_string())
                        .append_pair("$expand", "all");
                    let reply = request(client, key, Method::GET, url, None).await?;
                    let rows = reply["value"]
                        .as_array()
                        .context("invalid task revisions")?;
                    revisions.extend(rows.iter().cloned());
                    if rows.len() < 200 {
                        break;
                    }
                    ensure!(page < 4, "task revision limit reached");
                }
                let mut last = None;
                let mut finished = false;
                let mut previous_state = "";
                for revision in &revisions {
                    let at = DateTime::parse_from_rfc3339(field(revision, "System.ChangedDate"))?;
                    let state = field(revision, "System.State");
                    if at >= start
                        && at < end
                        && same_user(&revision["fields"]["System.AssignedTo"], user)
                        && completed.contains(state)
                        && !completed.contains(previous_state)
                    {
                        finished = true;
                    }
                    if at < end {
                        last = Some(revision);
                    }
                    previous_state = state;
                }
                let hours = registration::revision_hours(
                    &revisions,
                    start,
                    end,
                    &settings.effort_field,
                    user,
                );
                ensure!(hours.is_finite(), "invalid daily task effort");
                if hours <= 0. && !finished {
                    continue;
                }
                let mut value = last.context("missing daily task revision")?.clone();
                value["id"] = json!(id);
                ensure!(
                    field(&value, "System.TeamProject").eq_ignore_ascii_case(project)
                        && kinds.contains(&field(&value, "System.WorkItemType")),
                    "invalid daily task project or type"
                );
                let parent_id = relation_ids(&value, "System.LinkTypes.Hierarchy-Reverse")
                    .into_iter()
                    .next();
                let parent = if let Some(parent) = parent_id {
                    let parents =
                        work_items(client, key, source, project, &BTreeSet::from([parent])).await?;
                    parents
                        .first()
                        .map(|v| item(source, field(v, "System.TeamProject"), v))
                        .transpose()?
                } else {
                    None
                };
                out.tasks.push(Task {
                    item: item(source, project, &value)?,
                    source: query.source.clone().unwrap_or_default(),
                    organization: source.organization.clone(),
                    user: user.into(),
                    description: scraper::Html::parse_fragment(field(&value, "System.Description"))
                        .root_element()
                        .text()
                        .collect::<Vec<_>>()
                        .join(" "),
                    hours,
                    parent,
                });
                out.hours += hours;
            }
        }
    }
    ensure!(
        matched_project,
        "invalid project: outside the configured Azure catalog"
    );
    out.tasks.sort_by(|a, b| {
        a.item
            .project
            .cmp(&b.item.project)
            .then(a.item.id.cmp(&b.item.id))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn daily_tasks_use_history_local_dates_and_actual_work_without_writes() {
        let server = MockServer::start().await;
        TEST_SERVER.with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        let effort = "Custom.RealHours";
        for (endpoint, value) in [
            (
                "/example/Payments/_apis/wit/workitemtypecategories/Microsoft.TaskCategory",
                json!({"workItemTypes":[{"name":"Custom Task"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypes/Custom%20Task",
                json!({"states":[{"name":"Working","category":"InProgress"},{"name":"Finished","category":"Completed"}]}),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_json(value))
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/example/Payments/_apis/wit/wiql"))
            .respond_with(|r: &wiremock::Request| {
                let query = r.body_json::<Value>().unwrap()["query"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                assert!(query.contains("[System.AssignedTo] EVER 'me@example.test'"));
                assert!(query.contains("2026-10-14T03:00:00+00:00"));
                assert!(query.contains("'Custom Task'"));
                ResponseTemplate::new(200)
                    .set_body_json(json!({"workItems":[{"id":1},{"id":2},{"id":3},{"id":4}]}))
            })
            .mount(&server)
            .await;
        let revision = |date: &str, hours: f64, state: &str, user: &str| {
            json!({"fields":{
                "System.TeamProject":"Payments","System.WorkItemType":"Custom Task","System.Title":"Daily task","System.Description":"<p>Verified work</p>",
                "System.State":state,"System.ChangedDate":date,"System.CreatedDate":"2026-10-01T12:00:00Z","System.AssignedTo":{"uniqueName":user},effort:hours
            }})
        };
        for (id, revisions) in [
            (
                1,
                vec![
                    revision("2026-10-14T02:30:00Z", 2., "Working", "me@example.test"),
                    revision("2026-10-15T02:30:00Z", 32., "Working", "me@example.test"),
                    revision("2026-10-15T14:00:00Z", 40., "Working", "other@example.test"),
                ],
            ),
            (
                2,
                vec![
                    revision("2026-10-14T10:00:00Z", 0., "Working", "me@example.test"),
                    revision("2026-10-14T12:00:00Z", 0., "Finished", "me@example.test"),
                ],
            ),
            (
                3,
                vec![revision(
                    "2026-10-14T12:00:00Z",
                    8.,
                    "Finished",
                    "other@example.test",
                )],
            ),
            (
                4,
                vec![
                    revision("2026-10-13T12:00:00Z", 5., "Working", "me@example.test"),
                    revision("2026-10-14T12:00:00Z", 5., "Working", "me@example.test"),
                ],
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/example/Payments/_apis/wit/workitems/{id}/revisions"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":revisions})))
                .mount(&server)
                .await;
        }
        let dir = tempfile::tempdir().unwrap();
        let cache = Store::open(&dir.path().join("cache.db")).unwrap();
        let settings = Settings {
            time_zone: "America/Santiago".into(),
            effort_field: effort.into(),
            daily_hours: 1.,
            ..Default::default()
        };
        let query = Query {
            day: "2026-10-14".into(),
            user: None,
            source: Some("ado".into()),
            project: None,
        };
        let catalog = "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='me@example.test'";
        let day = list(
            &Client::new(),
            "test-key",
            catalog,
            &cache,
            &query,
            &settings,
        )
        .await
        .unwrap();
        assert_eq!(
            day.tasks.iter().map(|t| t.item.id).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            day.hours, 30.,
            "daily effort is a delta, never capped to the target"
        );
        assert_eq!(day.tasks[0].item.state, "Working");
        assert_eq!(day.tasks[0].description, "Verified work");
        assert!(
            day.tasks
                .iter()
                .all(|t| t.parent.is_none() && t.source == "ado")
        );
        assert!(
            day.tasks[0]
                .item
                .url
                .ends_with("/Payments/_workitems/edit/1")
        );
        assert!(
            list(
                &Client::new(),
                "test-key",
                catalog,
                &cache,
                &Query {
                    project: Some("Outside".into()),
                    ..query
                },
                &settings
            )
            .await
            .is_err()
        );
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method == Method::GET || r.url.path().ends_with("/wiql")),
            "there are no work item writes"
        );
        TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }
}
