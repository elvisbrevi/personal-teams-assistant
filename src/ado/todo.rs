//! The user's HUs and bugs by process-specific assignment, independent of activity windows.
use super::*;
use crate::tools::todo::{Date, Filters, Item, List};
use sha2::{Digest, Sha256};

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}
fn developer(field: &Value) -> bool {
    let name = field["name"].as_str().unwrap_or_default().to_lowercase();
    field["isIdentity"] == true && (name.contains("desarrollador") || name.contains("developer"))
}
pub(super) fn date_field(field: &Value) -> bool {
    if field["type"] != "dateTime" {
        return false;
    }
    let name = format!(
        "{} {}",
        field["name"].as_str().unwrap_or_default(),
        field["referenceName"].as_str().unwrap_or_default()
    )
    .to_lowercase();
    !name.contains("statechange")
        && !name.contains("state change")
        && [
            "start", "inicio", "finish", "end date", "enddate", "termin", "términ", "target",
            "due", "closed",
        ]
        .iter()
        .any(|word| name.contains(word))
}
pub(super) fn timestamp(value: &Value) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|date| {
            date.with_timezone(&Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
}

pub(super) async fn metadata(client: &Client, key: &str, cache: &Store, url: Url) -> Result<Value> {
    // Metadata only, five minutes; the credential fingerprint prevents reuse after an account change.
    let cache_key = format!("todo-schema:{:x}:{url}", Sha256::digest(key.as_bytes()));
    cached_read(
        cache,
        &cache_key,
        300,
        request(client, key, Method::GET, url, None),
    )
    .await
}

struct Kind {
    name: String,
    assignments: Vec<Value>,
}
struct Schema {
    kinds: Vec<Kind>,
    states: BTreeSet<String>,
    assignments: Vec<Value>,
    dates: Vec<Value>,
}
async fn schema(
    client: &Client,
    key: &str,
    cache: &Store,
    source: &Source,
    project: &str,
    filter: Option<&str>,
) -> Result<Schema> {
    let fields = metadata(
        client,
        key,
        cache,
        organization_url(source, &["_apis", "wit", "fields"])?,
    )
    .await?;
    let fields = fields["value"]
        .as_array()
        .context("invalid organization fields")?;
    let mut names = BTreeSet::new();
    for (category, group) in [
        ("Microsoft.RequirementCategory", "hu"),
        ("Microsoft.BugCategory", "bug"),
        // This process classifies its development bug as a task.
        ("Microsoft.TaskCategory", "bug"),
    ] {
        if filter.is_some_and(|filter| filter != group) {
            continue;
        }
        let reply = metadata(
            client,
            key,
            cache,
            project_url(
                source,
                project,
                "dev.azure.com",
                &["_apis", "wit", "workitemtypecategories", category],
            )?,
        )
        .await;
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) if error.to_string().ends_with("HTTP 404") => continue,
            Err(error) => return Err(error),
        };
        for kind in reply["workItemTypes"]
            .as_array()
            .context("invalid work item category")?
        {
            let Some(name) = kind["name"].as_str() else {
                continue;
            };
            if category == "Microsoft.TaskCategory" && name != "Bug Desarrollo" {
                continue;
            }
            names.insert(name.to_owned());
        }
    }
    ensure!(names.len() <= 40, "invalid work item types");
    let mut kinds = Vec::new();
    let mut states = BTreeSet::new();
    let mut used = BTreeSet::new();
    for kind in names {
        let definition = metadata(
            client,
            key,
            cache,
            project_url(
                source,
                project,
                "dev.azure.com",
                &["_apis", "wit", "workitemtypes", &kind],
            )?,
        )
        .await?;
        for state in definition["states"]
            .as_array()
            .context("invalid HU states")?
        {
            if let Some(state) = state["name"].as_str() {
                states.insert(state.to_owned());
            }
        }
        let type_fields: BTreeSet<_> = definition["fields"]
            .as_array()
            .context("invalid work item fields")?
            .iter()
            .filter_map(|f| f["referenceName"].as_str().filter(|name| reference(name)))
            .collect();
        let mut assignments: Vec<Value> = fields
            .iter()
            .filter(|f| {
                developer(f)
                    && f["referenceName"]
                        .as_str()
                        .is_some_and(|r| type_fields.contains(r))
            })
            .cloned()
            .collect();
        // Assignment is process-specific per type: bugs may lack the HU's developer roles.
        if assignments.is_empty() {
            assignments.push(json!({"name":"Assigned To","referenceName":"System.AssignedTo"}));
        }
        used.extend(type_fields.into_iter().map(str::to_owned));
        kinds.push(Kind {
            name: kind,
            assignments,
        });
    }
    let assignments = kinds
        .iter()
        .flat_map(|k| k.assignments.iter().cloned())
        .collect();
    let dates = fields
        .iter()
        .filter(|f| {
            f["referenceName"]
                .as_str()
                .is_some_and(|r| used.contains(r))
                && date_field(f)
        })
        .cloned()
        .collect();
    Ok(Schema {
        kinds,
        states,
        assignments,
        dates,
    })
}

async fn in_project(
    client: &Client,
    key: &str,
    cache: &Store,
    source: &Source,
    project: &str,
    filters: &Filters,
    result: &mut List,
) -> Result<()> {
    let schema = schema(client, key, cache, source, project, filters.kind.as_deref()).await?;
    result.states.extend(schema.states.iter().cloned());
    result.assignment_fields.extend(
        schema
            .assignments
            .iter()
            .filter_map(|f| f["name"].as_str().map(str::to_owned)),
    );
    if filters
        .state
        .as_ref()
        .is_some_and(|state| !schema.states.contains(state))
    {
        return Ok(());
    }
    if schema.kinds.is_empty() {
        return Ok(());
    }
    let assigned = schema
        .kinds
        .iter()
        .map(|kind| {
            let roles = kind
                .assignments
                .iter()
                .map(|f| {
                    format!(
                        "[{}] = {}",
                        f["referenceName"].as_str().unwrap_or_default(),
                        literal(&source.author_email)
                    )
                })
                .collect::<Vec<_>>()
                .join(" OR ");
            format!(
                "([System.WorkItemType] = {} AND ({roles}))",
                literal(&kind.name)
            )
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let state = filters
        .state
        .as_ref()
        .map(|s| format!(" AND [System.State] = {}", literal(s)))
        .unwrap_or_default();
    let query = format!(
        "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = {} AND ({assigned}){state} ORDER BY [System.CreatedDate] DESC, [System.Id] DESC",
        literal(project)
    );
    let mut url = project_url(source, project, "dev.azure.com", &["_apis", "wit", "wiql"])?;
    url.query_pairs_mut()
        .append_pair("$top", &(filters.limit + 1).to_string());
    let found = request(client, key, Method::POST, url, Some(json!({"query":query}))).await?;
    let ids: BTreeSet<u64> = found["workItems"]
        .as_array()
        .context("invalid HU query")?
        .iter()
        .filter_map(|v| v["id"].as_u64().filter(|id| *id > 0))
        .take(filters.limit + 1)
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let mut fields = vec![
        "System.Id",
        "System.Title",
        "System.TeamProject",
        "System.WorkItemType",
        "System.State",
        "System.CreatedDate",
    ];
    fields.extend(
        schema
            .assignments
            .iter()
            .chain(&schema.dates)
            .filter_map(|field| field["referenceName"].as_str()),
    );
    fields.sort_unstable();
    fields.dedup();
    let mut url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "wit", "workitems"],
    )?;
    url.query_pairs_mut()
        .append_pair(
            "ids",
            &ids.iter().map(u64::to_string).collect::<Vec<_>>().join(","),
        )
        .append_pair("fields", &fields.join(","))
        .append_pair("errorPolicy", "Omit");
    let reply = request(client, key, Method::GET, url, None).await?;
    let items = reply["value"]
        .as_array()
        .context("invalid HU fields reply")?;
    if items.len() < ids.len() {
        result.warn(format!("Some HUs could not be read in {project}."));
    }
    for item in items {
        // Validate the response too: a WIQL result alone is not evidence of developer assignment.
        if !item["id"].as_u64().is_some_and(|id| ids.contains(&id))
            || !field(item, "System.TeamProject").eq_ignore_ascii_case(project)
            || !schema
                .kinds
                .iter()
                .any(|k| k.name == field(item, "System.WorkItemType"))
            || !schema
                .kinds
                .iter()
                .filter(|k| k.name == field(item, "System.WorkItemType"))
                .flat_map(|k| &k.assignments)
                .any(|f| {
                    same_user(
                        &item["fields"][f["referenceName"].as_str().unwrap_or_default()],
                        &source.author_email,
                    )
                })
            || filters
                .state
                .as_ref()
                .is_some_and(|s| s != field(item, "System.State"))
        {
            continue;
        }
        let link = item_reference(source, project, item)?;
        result.items.push(Item {
            id: item["id"].as_u64().context("invalid HU ID")?,
            title: field(item, "System.Title").into(),
            url: link.url,
            project: project.into(),
            state: field(item, "System.State").into(),
            kind: field(item, "System.WorkItemType").into(),
            created_at: timestamp(&item["fields"]["System.CreatedDate"]),
            dates: schema
                .dates
                .iter()
                .map(|f| {
                    let name = f["referenceName"].as_str().unwrap_or_default();
                    Date {
                        field: name.into(),
                        name: f["name"].as_str().unwrap_or(name).into(),
                        value: timestamp(&item["fields"][name]),
                    }
                })
                .collect(),
        });
    }
    Ok(())
}

pub async fn list(
    client: &Client,
    key: &str,
    catalog: &str,
    cache: &Store,
    filters: &Filters,
) -> Result<List> {
    filters.validate()?;
    let catalog = Catalog::parse(catalog)?.own(client, key).await?;
    let mut result = List::default();
    for source in &catalog.sources {
        result.projects.extend(source.projects.iter().cloned());
        for project in &source.projects {
            if filters
                .project
                .as_ref()
                .is_some_and(|p| !p.eq_ignore_ascii_case(project))
            {
                continue;
            }
            if in_project(client, key, cache, source, project, filters, &mut result)
                .await
                .is_err()
            {
                result.warn(format!("HUs or process fields could not be read in {project}; check the Azure read credential."));
            }
        }
    }
    ensure!(
        filters
            .project
            .as_ref()
            .is_none_or(|p| result.projects.iter().any(|s| s.eq_ignore_ascii_case(p))),
        "invalid project: outside the configured Azure scope"
    );
    ensure!(
        result.partial
            || filters
                .state
                .as_ref()
                .is_none_or(|s| result.states.contains(s)),
        "invalid state: choose a state returned by this tool"
    );
    result.finish(filters.limit);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn developer_roles_states_dates_and_order_are_read_from_the_process() {
        let server = MockServer::start().await;
        TEST_SERVER.with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        let fields = json!([
            {"referenceName":"System.AssignedTo","name":"Assigned To","isIdentity":true,"type":"string"},
            {"referenceName":"Custom.Developer1","name":"Developer 1","isIdentity":true,"type":"string"},
            {"referenceName":"Custom.Developer2","name":"Developer 2","isIdentity":true,"type":"string"},
            {"referenceName":"Microsoft.VSTS.Scheduling.StartDate","name":"Start Date","type":"dateTime"},
            {"referenceName":"Custom.DevStart","name":"Development start","type":"dateTime"},
            {"referenceName":"Custom.DevFinish","name":"Development finish","type":"dateTime"},
            {"referenceName":"Custom.Finish","name":"Finish Date","type":"dateTime"}
        ]);
        for (endpoint, value) in [
            ("/example/_apis/wit/fields", json!({"value":fields})),
            (
                "/example/Payments/_apis/wit/workitemtypecategories/Microsoft.RequirementCategory",
                json!({"workItemTypes":[{"name":"Custom HU"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypecategories/Microsoft.BugCategory",
                json!({"workItemTypes":[{"name":"Bug"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypecategories/Microsoft.TaskCategory",
                json!({"workItemTypes":[{"name":"Bug Desarrollo"},{"name":"Task"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypes/Bug",
                json!({"fields":[{"referenceName":"System.AssignedTo"}],"states":[{"name":"Resolved"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypes/Bug%20Desarrollo",
                json!({"fields":[{"referenceName":"System.AssignedTo"}],"states":[{"name":"New"},{"name":"Done"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypes/Custom%20HU",
                json!({"fields":fields.as_array().unwrap().iter().filter(|f| f["referenceName"] != "System.AssignedTo").collect::<Vec<_>>(),"states":[{"name":"Developing"},{"name":"Finished"},{"name":"Unused state"}]}),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_json(value))
                .mount(&server)
                .await;
        }
        Mock::given(method("POST")).and(path("/example/Payments/_apis/wit/wiql")).respond_with(|r: &wiremock::Request| {
            let query = r.body_json::<Value>().unwrap()["query"].as_str().unwrap().to_owned();
            if query.contains("Custom HU") { assert!(query.contains("[Custom.Developer1] = 'me@example.test' OR [Custom.Developer2] = 'me@example.test'"), "{query}"); }
            if query.contains("[System.WorkItemType] = 'Bug' AND") { assert!(query.contains("[System.WorkItemType] = 'Bug' AND ([System.AssignedTo] = 'me@example.test')")); }
            if query.contains("Bug Desarrollo") {
                assert!(query.contains("[System.WorkItemType] = 'Bug Desarrollo' AND ([System.AssignedTo] = 'me@example.test')"), "{query}");
                assert!(!query.contains("[System.WorkItemType] = 'Task'"), "{query}");
            }
            assert!(query.contains("[System.CreatedDate] DESC"));
            assert!(r.url.query_pairs().any(|(k,v)| k == "$top" && v == "3"));
            ResponseTemplate::new(200).set_body_json(json!({"workItems":[{"id":1},{"id":2},{"id":3}]}))
        }).mount(&server).await;
        let item = |id, assigned: &str| {
            json!({"id":id,"fields":{
                "System.TeamProject":"Payments","System.WorkItemType":match id { 2 => "Bug", 3 => "Bug Desarrollo", _ => "Custom HU" },"System.Title":format!("HU {id}"),"System.State":"Developing",
                "System.CreatedDate":format!("2026-10-0{id}T12:00:00Z"),"System.AssignedTo":{"uniqueName":if id == 2 || id == 3 { "me@example.test" } else { "owner@example.test" }},
                "Custom.Developer2":{"uniqueName":assigned},"Custom.DevStart":"2026-10-04T12:00:00Z","Custom.DevFinish":"2026-10-05T12:00:00Z"
            }})
        };
        Mock::given(method("GET")).and(path("/example/Payments/_apis/wit/workitems")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[item(1,"me@example.test"),item(3,"me@example.test"),item(2,"other@example.test")]}))).mount(&server).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Store::open(&dir.path().join("cache.db")).unwrap();
        let result = list(&Client::new(), "test-key", "[[sources]]\norganization='https://dev.azure.com/example/'\nprojects=['Payments']\nauthor_email='me@example.test'", &cache, &Filters { limit:2, ..Default::default() }).await.unwrap();
        assert!(!result.partial, "{:?}", result.warnings);
        assert_eq!(
            result.items.iter().map(|i| i.id).collect::<Vec<_>>(),
            [3, 2]
        );
        assert!(result.states.contains(&"Unused state".into()));
        assert_eq!(result.items[0].dates.len(), 4);
        assert_eq!(
            result.items[0]
                .dates
                .iter()
                .filter(|d| d.value.is_some())
                .count(),
            2
        );
        assert!(
            result.items[0]
                .url
                .starts_with("https://dev.azure.com/example/Payments/_workitems/edit/")
        );
        assert!(result.states.contains(&"Resolved".into()));
        for (kind, expected) in [("hu", vec![1]), ("bug", vec![3, 2])] {
            let filtered = list(&Client::new(), "test-key", "[[sources]]\norganization='https://dev.azure.com/example/'\nprojects=['Payments']\nauthor_email='me@example.test'", &cache, &Filters { limit: 2, kind: Some(kind.into()), ..Default::default() }).await.unwrap();
            assert!(!filtered.partial, "{:?}", filtered.warnings);
            assert_eq!(
                filtered.items.iter().map(|i| i.id).collect::<Vec<_>>(),
                expected
            );
            if kind == "bug" {
                assert_eq!(filtered.states, ["Done", "New", "Resolved"]);
                assert_eq!(filtered.assignment_fields, ["Assigned To"]);
            }
        }
        TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }
}
