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
pub(super) fn item(source: &Source, project: &str, value: &Value) -> Result<Item> {
    let reference = item_reference(source, project, value)?;
    Ok(Item {
        id: value["id"].as_u64().context("missing task ID")?,
        title: field(value, "System.Title").into(),
        url: reference.url,
        project: project.into(),
        state: field(value, "System.State").into(),
        kind: field(value, "System.WorkItemType").into(),
        category: "task".into(),
        created_at: todo::timestamp(&value["fields"]["System.CreatedDate"]),
        changed_at: None,
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

/// Whether `item` has a child of a task type, by the links it had at the day's end or has now.
async fn has_tasks(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    item: &Value,
    task_kinds: &BTreeSet<String>,
) -> Result<bool> {
    const CHILD: &str = "System.LinkTypes.Hierarchy-Forward";
    let id = item["id"].as_u64().context("missing task ID")?;
    let mut children: BTreeSet<u64> = relation_ids(item, CHILD).into_iter().collect();
    for current in work_items(client, key, source, project, &BTreeSet::from([id])).await? {
        children.extend(relation_ids(&current, CHILD));
    }
    // One read: Azure DevOps takes up to 200 IDs.
    let children: BTreeSet<u64> = children.into_iter().take(200).collect();
    Ok(work_items(client, key, source, project, &children)
        .await?
        .iter()
        .any(|child| task_kinds.contains(field(child, "System.WorkItemType"))))
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
            // Bugs carry actual hours like tasks. A process without a bug category
            // answers it with 404, which leaves only the tasks; any other error fails.
            let mut kinds: Vec<String> = Vec::new();
            // The task category's types; the others are bugs.
            let mut task_kinds = BTreeSet::new();
            for name in ["Microsoft.TaskCategory", "Microsoft.BugCategory"] {
                let url = project_url(
                    source,
                    project,
                    "dev.azure.com",
                    &["_apis", "wit", "workitemtypecategories", name],
                )?;
                let category = match todo::metadata(client, key, cache, url).await {
                    Ok(category) => category,
                    Err(error)
                        if name == "Microsoft.BugCategory"
                            && error.to_string().ends_with("returned HTTP 404") =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                for kind in category["workItemTypes"]
                    .as_array()
                    .context("invalid task category")?
                    .iter()
                    .filter_map(|k| k["name"].as_str())
                {
                    if name == "Microsoft.TaskCategory" {
                        task_kinds.insert(kind.to_owned());
                    }
                    if !kinds.iter().any(|k| k == kind) {
                        kinds.push(kind.to_owned());
                    }
                }
            }
            ensure!(!kinds.is_empty() && kinds.len() <= 40, "invalid task types");
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
                for revision in &revisions {
                    let at = DateTime::parse_from_rfc3339(field(revision, "System.ChangedDate"))?;
                    if at < end {
                        last = Some(revision);
                    }
                }
                let Some(last) = last else {
                    continue;
                };
                let kind = field(last, "System.WorkItemType");
                let hours = registration::revision_hours(
                    &revisions,
                    start,
                    end,
                    settings.effort_field_for(kind),
                    user,
                );
                ensure!(hours.is_finite(), "invalid daily task effort");
                // Only actual hours put work on a day: a close or another change alone does not.
                if hours <= 0. {
                    continue;
                }
                let mut value = last.clone();
                value["id"] = json!(id);
                ensure!(
                    field(&value, "System.TeamProject").eq_ignore_ascii_case(project)
                        && kinds.iter().any(|k| k == kind),
                    "invalid daily task project or type"
                );
                // A bug whose work is in its tasks is their parent, not work of its own: its
                // hours are already theirs, and counting it too would add them twice.
                if !task_kinds.contains(kind)
                    && has_tasks(client, key, source, project, &value, &task_kinds).await?
                {
                    continue;
                }
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
        // Task 2 only closed, task 3 is someone else's and task 4 kept its hours: a day holds
        // actual hours, never a row of 0 h.
        assert_eq!(day.tasks.iter().map(|t| t.item.id).collect::<Vec<_>>(), [1]);
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

    #[tokio::test]
    async fn daily_tasks_include_bugs_with_actual_work() {
        let server = MockServer::start().await;
        TEST_SERVER.with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        let effort = "Custom.RealHours";
        for (endpoint, value) in [
            (
                "/example/Payments/_apis/wit/workitemtypecategories/Microsoft.TaskCategory",
                json!({"workItemTypes":[{"name":"Task"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypecategories/Microsoft.BugCategory",
                json!({"workItemTypes":[{"name":"Bug"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypes/Task",
                json!({"states":[{"name":"Done","category":"Completed"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypes/Bug",
                json!({"states":[{"name":"In review","category":"InProgress"},{"name":"Done","category":"Completed"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitems",
                json!({"value":[{"id":7,"fields":{"System.TeamProject":"Payments","System.WorkItemType":"Bug"}}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitems/7/revisions",
                json!({"value":[{"fields":{
                    "System.TeamProject":"Payments","System.WorkItemType":"Bug","System.Title":"Fix import",
                    "System.State":"In review","System.ChangedDate":"2026-10-14T15:00:00Z","System.CreatedDate":"2026-10-14T15:00:00Z",
                    "System.AssignedTo":{"uniqueName":"me@example.test"},effort:6.
                }}]}),
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
                assert!(query.contains("IN ('Task','Bug')"));
                ResponseTemplate::new(200).set_body_json(json!({"workItems":[{"id":7}]}))
            })
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Store::open(&dir.path().join("cache.db")).unwrap();
        let settings = Settings {
            time_zone: "America/Santiago".into(),
            effort_field: effort.into(),
            daily_hours: 9.,
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
        assert_eq!(day.tasks.len(), 1);
        assert_eq!(day.tasks[0].item.kind, "Bug");
        assert_eq!(day.hours, 6.);
        TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }

    /// A bug closed today whose hours are in its task, a bug without tasks that keeps its hours
    /// in its own field, and a task: the day counts each hour once.
    #[tokio::test]
    async fn bugs_with_tasks_are_their_parent_and_bugs_without_use_their_own_field() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        TEST_SERVER.with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        const WORK: &str = "Microsoft.VSTS.Scheduling.CompletedWork";
        const BUG: &str = "Custom.EsfuerzoReal";
        let base = "/example/Payments/_apis/wit";
        for (endpoint, value) in [
            (
                "workitemtypecategories/Microsoft.TaskCategory",
                json!({"workItemTypes":[{"name":"Task"}]}),
            ),
            (
                "workitemtypecategories/Microsoft.BugCategory",
                json!({"workItemTypes":[{"name":"Bug"}]}),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("{base}/{endpoint}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(value))
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path(format!("{base}/wiql")))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"workItems":[{"id":10},{"id":20},{"id":21},{"id":30},{"id":31},{"id":32}]}),
            ))
            .mount(&server)
            .await;
        let link = |rel: &str, id: u64| json!({"rel":rel,"url":format!("https://dev.azure.com/example/_apis/wit/workItems/{id}")});
        let child = |id| link("System.LinkTypes.Hierarchy-Forward", id);
        let parent = |id| link("System.LinkTypes.Hierarchy-Reverse", id);
        let revision = |kind: &str,
                        at: &str,
                        state: &str,
                        effort: (&str, Option<f64>),
                        relations: Vec<Value>| {
            let mut fields = json!({
                "System.TeamProject":"Payments","System.WorkItemType":kind,"System.Title":format!("{kind} work"),
                "System.State":state,"System.ChangedDate":at,"System.CreatedDate":"2026-11-01T12:00:00Z",
                "System.AssignedTo":{"uniqueName":"me@example.test"}
            });
            if let Some(hours) = effort.1 {
                fields[effort.0] = json!(hours);
            }
            json!({"fields":fields,"relations":relations})
        };
        // Day 2026-11-11 in Santiago starts at 03:00 UTC.
        let (yesterday, today) = ("2026-11-10T15:00:00Z", "2026-11-11T15:00:00Z");
        for (id, revisions) in [
            // A task: 7 h today.
            (
                10,
                vec![
                    revision("Task", yesterday, "Active", (WORK, Some(0.)), vec![]),
                    revision("Task", today, "Done", (WORK, Some(7.)), vec![]),
                ],
            ),
            // A bug closed today, without completed work: its 6 h are in task 21 (yesterday)
            // and in its own field since yesterday.
            (
                20,
                vec![
                    revision("Bug", yesterday, "Active", (BUG, Some(6.)), vec![child(21)]),
                    revision("Bug", today, "Done", (BUG, Some(6.)), vec![child(21)]),
                ],
            ),
            (
                21,
                vec![
                    revision(
                        "Task",
                        "2026-11-10T12:00:00Z",
                        "Active",
                        (WORK, Some(0.)),
                        vec![parent(20)],
                    ),
                    revision(
                        "Task",
                        yesterday,
                        "Done",
                        (WORK, Some(6.)),
                        vec![parent(20)],
                    ),
                ],
            ),
            // A bug without tasks: 3 h today in its own field.
            (
                30,
                vec![
                    revision("Bug", yesterday, "Active", (BUG, Some(1.)), vec![]),
                    revision("Bug", today, "Done", (BUG, Some(4.)), vec![]),
                ],
            ),
            // A bug whose field changed today but whose work is in a task: not counted twice.
            (
                31,
                vec![revision("Bug", today, "Active", (BUG, Some(5.)), vec![])],
            ),
            // A bug with hours only in the default field: not its configured field, so nothing.
            (
                32,
                vec![revision("Bug", today, "Active", (WORK, Some(2.)), vec![])],
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("{base}/workitems/{id}/revisions")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":revisions})))
                .mount(&server)
                .await;
        }
        // Current items: bug 31 got its task after the day's last revision.
        let item = |id: u64, kind: &str, relations: Vec<Value>| json!({"id":id,"fields":{"System.TeamProject":"Payments","System.WorkItemType":kind},"relations":relations});
        for (ids, value) in [
            ("20", vec![item(20, "Bug", vec![child(21)])]),
            ("21", vec![item(21, "Task", vec![parent(20)])]),
            ("30", vec![item(30, "Bug", vec![])]),
            ("31", vec![item(31, "Bug", vec![child(33)])]),
            ("33", vec![item(33, "Task", vec![parent(31)])]),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("{base}/workitems")))
                .and(query_param("ids", ids))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":value})))
                .mount(&server)
                .await;
        }
        let dir = tempfile::tempdir().unwrap();
        let cache = Store::open(&dir.path().join("cache.db")).unwrap();
        let mut settings = Settings {
            time_zone: "America/Santiago".into(),
            ..Default::default()
        };
        settings.effort_fields.insert("Bug".into(), BUG.into());
        let day = list(
            &Client::new(),
            "test-key",
            "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='me@example.test'",
            &cache,
            &Query {
                day: "2026-11-11".into(),
                user: None,
                source: Some("ado".into()),
                project: None,
            },
            &settings,
        )
        .await
        .unwrap();
        let rows: Vec<(u64, f64)> = day.tasks.iter().map(|t| (t.item.id, t.hours)).collect();
        assert_eq!(rows, [(10, 7.), (30, 3.)]);
        assert_eq!(day.hours, 10., "every hour counts once");
        assert!(day.tasks.iter().all(|t| t.hours > 0.), "no row of 0 h");
        // Yesterday the task carries the bug's 6 h, with the bug as its parent; the bug itself
        // is not counted.
        let day = list(
            &Client::new(),
            "test-key",
            "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='me@example.test'",
            &cache,
            &Query {
                day: "2026-11-10".into(),
                user: None,
                source: Some("ado".into()),
                project: None,
            },
            &settings,
        )
        .await
        .unwrap();
        let rows: Vec<(u64, f64)> = day.tasks.iter().map(|t| (t.item.id, t.hours)).collect();
        assert_eq!(rows, [(21, 6.), (30, 1.)]);
        assert_eq!(day.tasks[0].parent.as_ref().map(|p| p.id), Some(20));
        assert_eq!(day.hours, 7.);
        TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }
}
