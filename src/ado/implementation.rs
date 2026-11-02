//! Child creation is authorized only by the implementation flow, never by activity plans.
use super::*;
use crate::tools::todo::{Detail, validate_link};
use crate::work_items::Guide;

fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>")
}

/// One field of a work item type as the provider defines it.
struct LiveField {
    kind: String,
    locked: bool,
    allowed: Vec<String>,
    /// The list only suggests values; others are accepted.
    open: bool,
}
/// The fields of a type in a project, with their kinds and allowed values.
async fn live_fields(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    kind: &str,
) -> Result<BTreeMap<String, LiveField>> {
    let organization = request(
        client,
        key,
        Method::GET,
        organization_url(source, &["_apis", "wit", "fields"])?,
        None,
    )
    .await?;
    let mut url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "wit", "workitemtypes", kind, "fields"],
    )?;
    url.query_pairs_mut()
        .append_pair("$expand", "allowedValues");
    let typed = request(client, key, Method::GET, url, None).await?;
    let definitions: BTreeMap<_, _> = organization["value"]
        .as_array()
        .context("Organization fields unavailable.")?
        .iter()
        .filter_map(|f| Some((f["referenceName"].as_str()?, f)))
        .collect();
    Ok(typed["value"]
        .as_array()
        .context("Work item fields unavailable.")?
        .iter()
        .filter_map(|f| {
            let reference = f["referenceName"].as_str()?;
            let definition = definitions.get(reference)?;
            Some((
                reference.to_owned(),
                LiveField {
                    kind: definition["type"].as_str().unwrap_or_default().to_owned(),
                    locked: definition["readOnly"] == true || definition["isIdentity"] == true,
                    allowed: f["allowedValues"]
                        .as_array()
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default(),
                    open: definition["isPicklistSuggested"] == true,
                },
            ))
        })
        .collect())
}
/// A proposed value in the form the field takes, or why it cannot be written.
fn field_value(reference: &str, field: &LiveField, value: &str) -> Result<Value> {
    let invalid = || anyhow::anyhow!("The value for {reference} does not fit the field.");
    let value = match field.allowed.iter().find(|v| v.eq_ignore_ascii_case(value)) {
        Some(allowed) => allowed.as_str(),
        None => {
            ensure!(
                field.allowed.is_empty() || field.open,
                "The value for {reference} is not one of its allowed values."
            );
            value
        }
    };
    Ok(match field.kind.as_str() {
        "string" => {
            ensure!(value.chars().count() <= 255, invalid());
            json!(value)
        }
        "plainText" => json!(value),
        "html" => json!(html(value)),
        "integer" => json!(value.parse::<i64>().map_err(|_| invalid())?),
        "double" => {
            let number = value
                .replace(',', ".")
                .parse::<f64>()
                .map_err(|_| invalid())?;
            ensure!(number.is_finite(), invalid());
            json!(number)
        }
        "boolean" => json!(match value.to_lowercase().as_str() {
            "true" | "1" | "yes" | "si" | "sí" => true,
            "false" | "0" | "no" => false,
            _ => return Err(invalid()),
        }),
        "dateTime" => {
            let date = chrono::DateTime::parse_from_rfc3339(value)
                .map(|d| d.with_timezone(&chrono::Utc))
                .or_else(|_| {
                    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                        .map(|d| d.and_hms_opt(12, 0, 0).unwrap().and_utc())
                })
                .map_err(|_| invalid())?;
            json!(date.to_rfc3339())
        }
        _ => anyhow::bail!("{reference} cannot be written from a work item guide."),
    })
}
/// The patch operations for the field values an agent proposed: only fields the type's guide
/// lets the agents fill, that the type has and that accept the value.
fn field_operations(
    guide: Option<&Guide>,
    kind: &str,
    proposed: &Value,
    live: &BTreeMap<String, LiveField>,
    written: &[&str],
) -> Result<Vec<Value>> {
    let mut seen = BTreeSet::new();
    let mut operations = Vec::new();
    for pair in proposed.as_array().into_iter().flatten() {
        let reference = pair["reference"].as_str().context("Invalid field.")?;
        let value = pair["value"].as_str().context("Invalid field.")?.trim();
        if value.is_empty() {
            continue;
        }
        ensure!(
            seen.insert(reference) && !written.contains(&reference),
            "{reference} is given twice."
        );
        ensure!(
            guide.is_some_and(|g| g.fields.iter().any(|f| f.fill && f.reference == reference))
                && !crate::work_items::reserved(reference),
            "The work item guide for {kind} does not let the agents fill {reference}."
        );
        let field = live
            .get(reference)
            .filter(|f| !f.locked)
            .with_context(|| format!("{kind} has no writable field {reference}."))?;
        operations.push(json!({"op":"add","path":format!("/fields/{reference}"),"value":field_value(reference, field, value)?}));
    }
    Ok(operations)
}
fn has_fields(proposed: &Value) -> bool {
    proposed.as_array().is_some_and(|fields| {
        fields
            .iter()
            .any(|f| f["value"].as_str().is_some_and(|v| !v.trim().is_empty()))
    })
}

/// Explicit requests may select any accessible project in the configured organizations.
pub(crate) async fn request_targets(
    client: &Client,
    key: &str,
    catalog: &str,
) -> Result<Vec<Value>> {
    let catalog = Catalog::parse(catalog)?;
    let mut targets = Vec::new();
    for mut source in catalog.sources {
        source.projects = vec!["*".into()];
        for project in projects(client, key, &source).await? {
            ensure!(
                !project.trim().is_empty()
                    && project.len() <= 120
                    && !project.chars().any(char::is_control),
                "Invalid Azure project."
            );
            let target = json!({"organization":source.organization,"project":project});
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
    }
    Ok(targets)
}

pub(crate) async fn request_target(
    client: &Client,
    key: &str,
    catalog: &str,
    target: &Value,
    kind: &str,
) -> Result<Value> {
    let category = match kind {
        "hu" => "Microsoft.RequirementCategory",
        "bug" => "Microsoft.BugCategory",
        _ => anyhow::bail!("Choose HU or bug."),
    };
    let available = catalog;
    let catalog = Catalog::parse(catalog)?;
    let source = catalog
        .sources
        .iter()
        .find(|s| target["organization"] == s.organization)
        .context("Project outside the Azure source catalog.")?;
    ensure!(
        request_targets(client, key, available)
            .await?
            .iter()
            .any(|t| t["organization"] == target["organization"]
                && t["project"] == target["project"]),
        "Project outside the Azure request catalog."
    );
    let project = target["project"].as_str().context("Choose a project.")?;
    let value = request(
        client,
        key,
        Method::GET,
        project_url(
            source,
            project,
            "dev.azure.com",
            &["_apis", "wit", "workitemtypecategories", category],
        )?,
        None,
    )
    .await?;
    let name = value["defaultWorkItemType"]["name"]
        .as_str()
        .context("Default request type unavailable.")?;
    ensure!(
        value["workItemTypes"]
            .as_array()
            .is_some_and(|types| types.iter().any(|t| t["name"] == name)),
        "Invalid process request type."
    );
    let fields = request(
        client,
        key,
        Method::GET,
        organization_url(source, &["_apis", "wit", "fields"])?,
        None,
    )
    .await?;
    let definition = request(
        client,
        key,
        Method::GET,
        project_url(
            source,
            project,
            "dev.azure.com",
            &["_apis", "wit", "workitemtypes", name],
        )?,
        None,
    )
    .await?;
    let type_fields: BTreeSet<_> = definition["fields"]
        .as_array()
        .context("Work item fields unavailable.")?
        .iter()
        .filter_map(|f| f["referenceName"].as_str())
        .collect();
    let content_field = if kind == "bug" && type_fields.contains("Microsoft.VSTS.TCM.ReproSteps") {
        "Microsoft.VSTS.TCM.ReproSteps"
    } else {
        "System.Description"
    };
    ensure!(
        type_fields.contains(content_field),
        "Request description field unavailable."
    );
    let assignments = todo::assignment_fields(
        fields["value"]
            .as_array()
            .context("Organization fields unavailable.")?,
        &type_fields,
    );
    let assignment = assignments[0]["referenceName"]
        .as_str()
        .context("Assignment field unavailable.")?;
    ensure!(
        !assignment.is_empty()
            && assignment.len() <= 200
            && assignment
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)),
        "Invalid assignment field."
    );
    Ok(
        json!({"organization":source.organization,"project":project,"kind":name,"assignee":source.author_email,"assignment":assignment,"content_field":content_field}),
    )
}

/// The field values of a generated request, checked before anything is written.
pub(crate) async fn request_fields(
    client: &Client,
    read_key: &str,
    catalog: &str,
    guides: &[Guide],
    target: &Value,
    draft: &Value,
) -> Result<Vec<Value>> {
    if !has_fields(&draft["fields"]) {
        return Ok(Vec::new());
    }
    let catalog = Catalog::parse(catalog)?;
    let source = catalog
        .sources
        .iter()
        .find(|s| target["organization"] == s.organization)
        .context("Project outside the Azure source catalog.")?;
    let project = target["project"].as_str().context("Missing project.")?;
    let kind = target["kind"].as_str().context("Missing type.")?;
    let content = target["content_field"].as_str().unwrap_or_default();
    let assignment = target["assignment"].as_str().unwrap_or_default();
    field_operations(
        crate::work_items::for_item(guides, "azure", kind, project),
        kind,
        &draft["fields"],
        &live_fields(client, read_key, source, project, kind).await?,
        &[content, assignment],
    )
}

pub(crate) async fn create_request(
    client: &Client,
    write_token: &str,
    target: &Value,
    draft: &Value,
    fields: Vec<Value>,
) -> Result<Value> {
    let source = Source {
        organization: target["organization"]
            .as_str()
            .context("Missing organization.")?
            .into(),
        projects: vec![],
        author_email: String::new(),
    };
    let project = target["project"].as_str().context("Missing project.")?;
    let kind = target["kind"].as_str().context("Missing type.")?;
    let url = project_url(
        &source,
        project,
        "dev.azure.com",
        &["_apis", "wit", "workitems", &format!("${kind}")],
    )?;
    #[cfg(test)]
    let url = super::test_url(url);
    let mut body = vec![
        json!({"op":"add","path":"/fields/System.Title","value":draft["title"]}),
        json!({"op":"add","path":format!("/fields/{}",target["content_field"].as_str().context("Missing content field.")?),"value":html(draft["description"].as_str().unwrap_or_default())}),
        json!({"op":"add","path":"/fields/System.AssignedTo","value":target["assignee"]}),
    ];
    if target["assignment"] != "System.AssignedTo" {
        body.push(json!({"op":"add","path":format!("/fields/{}",target["assignment"].as_str().context("Missing assignment field.")?),"value":target["assignee"]}));
    }
    body.extend(fields);
    let response = client
        .post(url)
        .bearer_auth(write_token)
        .header("Content-Type", "application/json-patch+json")
        .json(&body)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "Azure request creation did not complete."
    );
    let value: Value = crate::adapters::bounded_json(response, 1_000_000).await?;
    let id = value["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .context("Created request ID unavailable.")?;
    ensure!(
        field(&value, "System.TeamProject") == project
            && field(&value, "System.WorkItemType") == kind,
        "Invalid created request."
    );
    let mut url = project_url(
        &source,
        project,
        "dev.azure.com",
        &["_workitems", "edit", &id.to_string()],
    )?;
    url.set_query(None);
    Ok(
        json!({"id":id,"title":draft["title"],"url":validate_link(url.as_str())?.as_str(),"kind":kind,"project":project,"state":"created"}),
    )
}

/// The patch that creates one reviewed child: its type (the process default or one the child
/// names from the task category) and its fields, checked against the live definition.
async fn child_body(
    client: &Client,
    read_key: &str,
    catalog: &str,
    guides: &[Guide],
    parent: &Detail,
    child: &Value,
) -> Result<(Source, String, Vec<Value>)> {
    let catalog = Catalog::parse(catalog)?.own(client, read_key).await?;
    let parent_url = validate_link(&parent.item.url)?;
    let source = catalog
        .sources
        .into_iter()
        .find(|source| {
            parent_url
                .as_str()
                .starts_with(&format!("{}/", source.organization.trim_end_matches('/')))
                && source
                    .projects
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(&parent.item.project))
        })
        .context("Parent outside the Azure source catalog.")?;
    let category = request(
        client,
        read_key,
        Method::GET,
        project_url(
            &source,
            &parent.item.project,
            "dev.azure.com",
            &[
                "_apis",
                "wit",
                "workitemtypecategories",
                "Microsoft.TaskCategory",
            ],
        )?,
        None,
    )
    .await?;
    let kinds = category["workItemTypes"]
        .as_array()
        .context("Task types are unavailable.")?;
    let requested = child["type"].as_str().unwrap_or_default().trim();
    let kind = if requested.is_empty() {
        category["defaultWorkItemType"]["name"]
            .as_str()
            .or_else(|| {
                kinds
                    .iter()
                    .find_map(|k| k["name"].as_str().filter(|n| *n == "Task"))
            })
            .context("Default task type is unavailable.")?
    } else {
        kinds
            .iter()
            .find_map(|k| {
                k["name"]
                    .as_str()
                    .filter(|n| n.eq_ignore_ascii_case(requested))
            })
            .with_context(|| format!("{requested} is not a task type of this project."))?
    }
    .to_owned();
    ensure!(
        kinds.iter().any(|k| k["name"] == kind.as_str()),
        "Invalid process task type."
    );
    let mut parent_api = project_url(
        &source,
        &parent.item.project,
        "dev.azure.com",
        &["_apis", "wit", "workitems", &parent.item.id.to_string()],
    )?;
    parent_api.set_query(None);
    let mut body = vec![
        json!({"op":"add","path":"/fields/System.Title","value":child["title"]}),
        json!({"op":"add","path":"/fields/System.Description","value":html(child["description"].as_str().unwrap_or_default())}),
        json!({"op":"add","path":"/fields/System.AssignedTo","value":source.author_email}),
        json!({"op":"add","path":"/relations/-","value":{"rel":"System.LinkTypes.Hierarchy-Reverse","url":parent_api.as_str()}}),
    ];
    for reference in ["System.AreaPath", "System.IterationPath"] {
        if let Some(field) = parent
            .fields
            .iter()
            .find(|f| f.reference == reference && f.value.is_string())
        {
            body.push(
                json!({"op":"add","path":format!("/fields/{reference}"),"value":field.value}),
            );
        }
    }
    if has_fields(&child["fields"]) {
        body.extend(field_operations(
            crate::work_items::for_item(guides, "azure", &kind, &parent.item.project),
            &kind,
            &child["fields"],
            &live_fields(client, read_key, &source, &parent.item.project, &kind).await?,
            &[],
        )?);
    }
    Ok((source, kind, body))
}

/// Check a reviewed child without writing anything.
pub(crate) async fn check_child(
    client: &Client,
    read_key: &str,
    catalog: &str,
    guides: &[Guide],
    parent: &Detail,
    child: &Value,
) -> Result<()> {
    child_body(client, read_key, catalog, guides, parent, child)
        .await
        .map(|_| ())
}

pub(crate) async fn create_child(
    client: &Client,
    read_key: &str,
    write_token: &str,
    catalog: &str,
    guides: &[Guide],
    parent: &Detail,
    child: &Value,
) -> Result<Value> {
    let (source, kind, body) = child_body(client, read_key, catalog, guides, parent, child).await?;
    let url = project_url(
        &source,
        &parent.item.project,
        "dev.azure.com",
        &["_apis", "wit", "workitems", &format!("${kind}")],
    )?;
    #[cfg(test)]
    let url = super::test_url(url);
    let response = client
        .post(url)
        .bearer_auth(write_token)
        .header("Content-Type", "application/json-patch+json")
        .json(&body)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "Azure child creation did not complete."
    );
    let value: Value = crate::adapters::bounded_json(response, 1_000_000).await?;
    let id = value["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .context("Created task ID unavailable.")?;
    ensure!(
        field(&value, "System.TeamProject").eq_ignore_ascii_case(&parent.item.project)
            && field(&value, "System.WorkItemType") == kind,
        "Invalid created task."
    );
    let mut url = project_url(
        &source,
        &parent.item.project,
        "dev.azure.com",
        &["_workitems", "edit", &id.to_string()],
    )?;
    url.set_query(None);
    Ok(
        json!({"id":id,"title":child["title"],"url":validate_link(url.as_str())?.as_str(),"kind":kind,"state":"created"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn requests_are_confined_to_the_catalog_and_use_process_types_without_retries() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        let catalog = "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='person@example.test'";
        let client = Client::new();
        assert!(
            request_target(
                &client,
                "read",
                catalog,
                &json!({"organization":"https://dev.azure.com/other","project":"Payments"}),
                "hu"
            )
            .await
            .is_err()
        );
        assert!(server.received_requests().await.unwrap().is_empty());
        Mock::given(method("GET"))
            .and(path("/example/_apis/projects"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"value":[{"name":"Payments"},{"name":"Other project"}]})),
            )
            .mount(&server)
            .await;
        let targets = request_targets(&client, "read", catalog).await.unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[1]["project"], "Other project");
        Mock::given(method("GET")).and(path("/example/Other%20project/_apis/wit/workitemtypecategories/Microsoft.RequirementCategory")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"defaultWorkItemType":{"name":"Product Backlog Item"},"workItemTypes":[{"name":"Product Backlog Item"}]}))).expect(1).mount(&server).await;
        Mock::given(method("GET")).and(path("/example/_apis/wit/fields")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[{"name":"Developer","referenceName":"Custom.Developer","isIdentity":true}]}))).mount(&server).await;
        Mock::given(method("GET"))
            .and(path(
                "/example/Other%20project/_apis/wit/workitemtypes/Product%20Backlog%20Item",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"fields":[{"referenceName":"Custom.Developer"},{"referenceName":"System.Description"}]})),
            )
            .mount(&server)
            .await;
        let target = request_target(
            &client,
            "read",
            catalog,
            &json!({"organization":"https://dev.azure.com/example","project":"Other project"}),
            "hu",
        )
        .await
        .unwrap();
        assert_eq!(target["kind"], "Product Backlog Item");
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            create_request(
                &client,
                "write",
                &target,
                &json!({"title":"Receipts","description":"Escape <script>"}),
                Vec::new()
            )
            .await
            .is_err()
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 6);
        let body: Value = serde_json::from_slice(&requests[5].body).unwrap();
        assert_eq!(body[1]["value"], "Escape &lt;script&gt;");
        assert_eq!(body[3]["path"], "/fields/Custom.Developer");
        assert_eq!(requests[5].headers["authorization"], "Bearer write");
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }
    #[tokio::test]
    async fn request_catalog_reads_every_page_without_narrowing_to_team_projects() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path, query_param},
        };
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        let first: Vec<_> = (0..100)
            .map(|n| json!({"name":format!("Project {n}")}))
            .collect();
        Mock::given(method("GET"))
            .and(path("/example/_apis/projects"))
            .and(query_param("$skip", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":first})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/example/_apis/projects"))
            .and(query_param("$skip", "100"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"value":[{"name":"Last project"}]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let targets = request_targets(&Client::new(), "read", "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='me@example.test'").await.unwrap();
        assert_eq!(targets.len(), 101);
        assert_eq!(targets.last().unwrap()["project"], "Last project");
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method == "GET")
        );
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }
    #[test]
    fn descriptions_do_not_inject_provider_html() {
        assert_eq!(html("<script>&\n"), "&lt;script&gt;&amp;<br>");
    }
    #[tokio::test]
    async fn reviewed_children_use_process_task_types_and_the_azure_cli_bearer_token() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        Mock::given(method("GET")).and(path("/example/Payments/_apis/wit/workitemtypecategories/Microsoft.TaskCategory")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"defaultWorkItemType":{"name":"Development Task"},"workItemTypes":[{"name":"Development Task"}]}))).mount(&server).await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":88,"fields":{"System.TeamProject":"Payments","System.WorkItemType":"Development Task"}}))).mount(&server).await;
        let parent:Detail=serde_json::from_value(json!({"item":{"id":7,"title":"Fix receipt","url":"https://dev.azure.com/example/Payments/_workitems/edit/7","project":"Payments","state":"Active","kind":"Bug","created_at":null,"dates":[]},"fields":[{"reference":"System.IterationPath","name":"Iteration Path","kind":"treePath","value":"Payments/Sprint 1"}],"comments":[],"relations":[],"history":[],"partial":false,"warnings":[]})).unwrap();
        let catalog = "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='person@example.test'";
        let created = create_child(
            &Client::new(),
            "synthetic-read",
            "synthetic-write",
            catalog,
            &[],
            &parent,
            &json!({"title":"Implement receipt fix","description":"Keep <script> as text.","type":"","fields":[]}),
        )
        .await
        .unwrap();
        assert_eq!(created["id"], 88);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].headers["authorization"],
            "Bearer synthetic-write"
        );
        let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert!(
            body.as_array()
                .unwrap()
                .iter()
                .any(|op| op["path"] == "/fields/System.IterationPath"
                    && op["value"] == "Payments/Sprint 1")
        );
        assert!(body.as_array().unwrap().iter().any(|op| {
            op["value"]
                .as_str()
                .is_some_and(|s| s.contains("&lt;script&gt;"))
        }));
        assert!(requests[1].url.path().contains("Development%20Task"));
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }
    fn live(kind: &str, allowed: &[&str]) -> LiveField {
        LiveField {
            kind: kind.into(),
            locked: false,
            allowed: allowed.iter().map(|v| v.to_string()).collect(),
            open: false,
        }
    }
    #[test]
    fn only_guide_fields_with_valid_values_are_written() {
        let guide: Guide = toml::from_str(
            "id='task'\ntype='Task'\n[[fields]]\nreference='Custom.Month'\nfill=true\n[[fields]]\nreference='Custom.Hours'\nfill=true\n[[fields]]\nreference='Custom.Ready'\nfill=true\n[[fields]]\nreference='Custom.Notas'\n[[fields]]\nreference='Custom.Owner'\nfill=true",
        )
        .unwrap();
        let mut fields = BTreeMap::from([
            ("Custom.Month".to_owned(), live("string", &["Octubre"])),
            ("Custom.Hours".to_owned(), live("double", &[])),
            ("Custom.Ready".to_owned(), live("boolean", &[])),
            ("Custom.Notas".to_owned(), live("html", &[])),
        ]);
        fields.insert(
            "Custom.Owner".into(),
            LiveField {
                locked: true,
                ..live("string", &[])
            },
        );
        let operations =
            |proposed: Value| field_operations(Some(&guide), "Task", &proposed, &fields, &[]);
        let written = operations(json!([{"reference":"Custom.Month","value":"octubre"},{"reference":"Custom.Hours","value":"2,5"},{"reference":"Custom.Ready","value":"no"},{"reference":"Custom.Notas","value":" "}])).unwrap();
        assert_eq!(
            written,
            [
                json!({"op":"add","path":"/fields/Custom.Month","value":"Octubre"}),
                json!({"op":"add","path":"/fields/Custom.Hours","value":2.5}),
                json!({"op":"add","path":"/fields/Custom.Ready","value":false})
            ]
        );
        // Not a value of its list, not marked fill, locked, unknown to the type, repeated.
        for proposed in [
            json!([{"reference":"Custom.Month","value":"Brumario"}]),
            json!([{"reference":"Custom.Notas","value":"x"}]),
            json!([{"reference":"Custom.Owner","value":"x"}]),
            json!([{"reference":"Custom.Hours","value":"dos"}]),
            json!([{"reference":"Custom.Month","value":"Octubre"},{"reference":"Custom.Month","value":"Octubre"}]),
        ] {
            assert!(operations(proposed).is_err());
        }
        assert!(
            field_operations(
                None,
                "Task",
                &json!([{"reference":"Custom.Month","value":"Octubre"}]),
                &fields,
                &[]
            )
            .is_err()
        );
        assert_eq!(
            field_value("Custom.Fecha", &live("dateTime", &[]), "2026-11-02").unwrap(),
            json!("2026-11-02T12:00:00+00:00")
        );
        assert_eq!(
            field_value("Custom.Notas", &live("html", &[]), "<b>").unwrap(),
            json!("&lt;b&gt;")
        );
    }
    #[tokio::test]
    async fn a_child_may_name_a_task_type_and_carry_its_guide_fields() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        Mock::given(method("GET")).and(path("/example/Payments/_apis/wit/workitemtypecategories/Microsoft.TaskCategory")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"defaultWorkItemType":{"name":"Task"},"workItemTypes":[{"name":"Task"},{"name":"Support"}]}))).mount(&server).await;
        Mock::given(method("GET")).and(path("/example/_apis/wit/fields")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[{"referenceName":"Custom.Kind","type":"string","isPicklist":true}]}))).mount(&server).await;
        Mock::given(method("GET")).and(path("/example/Payments/_apis/wit/workitemtypes/Support/fields")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[{"referenceName":"Custom.Kind","allowedValues":["Pull request review","Other"]}]}))).mount(&server).await;
        let parent:Detail=serde_json::from_value(json!({"item":{"id":7,"title":"Receipts","url":"https://dev.azure.com/example/Payments/_workitems/edit/7","project":"Payments","state":"Active","kind":"Product Backlog Item","created_at":null,"dates":[]},"fields":[],"comments":[],"relations":[],"history":[],"partial":false,"warnings":[]})).unwrap();
        let catalog = "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='person@example.test'";
        let guides: Vec<Guide> = vec![
            toml::from_str(
                "id='support'\ntype='Support'\n[[fields]]\nreference='Custom.Kind'\nfill=true",
            )
            .unwrap(),
        ];
        let child = |kind: &str, value: &str| json!({"title":"Review PR 12","description":"Review the receipt fix.","type":kind,"fields":[{"reference":"Custom.Kind","value":value}]});
        let client = Client::new();
        assert!(
            check_child(
                &client,
                "read",
                catalog,
                &guides,
                &parent,
                &child("support", "pull request review")
            )
            .await
            .is_ok()
        );
        assert!(
            check_child(
                &client,
                "read",
                catalog,
                &guides,
                &parent,
                &child("Support", "Deploy")
            )
            .await
            .is_err()
        );
        assert!(
            check_child(
                &client,
                "read",
                catalog,
                &guides,
                &parent,
                &child("Epic", "Other")
            )
            .await
            .is_err()
        );
        // Without a guide for the type, no field is written.
        assert!(
            check_child(
                &client,
                "read",
                catalog,
                &[],
                &parent,
                &child("Support", "Other")
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
                .all(|r| r.method == "GET")
        );
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }
}
