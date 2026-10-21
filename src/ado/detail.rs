//! Full, read-only work item detail within the configured To-Do catalog.
use super::*;
use crate::tools::todo::{
    Change, Comment, Date, Detail, DetailField, DetailRequest, Item, Relation, https_link,
};

fn identity(value: &Value) -> String {
    value["displayName"]
        .as_str()
        .or_else(|| value["uniqueName"].as_str())
        .or_else(|| value.as_str())
        .unwrap_or_default()
        .into()
}
fn detail_field(reference: &str, value: &Value, definitions: &[Value]) -> DetailField {
    let definition = definitions.iter().find(|f| f["referenceName"] == reference);
    DetailField {
        reference: reference.into(),
        name: definition
            .and_then(|f| f["name"].as_str())
            .unwrap_or(reference)
            .into(),
        kind: definition
            .and_then(|f| f["type"].as_str())
            .unwrap_or("string")
            .into(),
        value: value.clone(),
        content: None,
    }
}
fn web_url(source: &Source, project: &str, segments: &[&str]) -> Result<Url> {
    let mut url = project_url(source, project, "dev.azure.com", segments)?;
    url.set_query(None);
    Ok(url)
}
fn artifact_link(source: &Source, project: &str, raw: &str) -> Option<(String, String)> {
    let encoded = format!("v={}", raw.strip_prefix("vstfs:///")?.replace('+', "%2B"));
    let decoded = url::form_urlencoded::parse(encoded.as_bytes())
        .next()?
        .1
        .into_owned();
    let parts: Vec<_> = decoded.split('/').collect();
    let (mut url, label) = match parts.as_slice() {
        ["Git", "Ref", project, repo, branch @ ..] if !branch.is_empty() => {
            let version = branch.join("/");
            let label = version
                .strip_prefix("GB")
                .or_else(|| version.strip_prefix("GT"))?;
            let mut url = web_url(source, project, &["_git", repo]).ok()?;
            url.query_pairs_mut().append_pair("version", &version);
            (url, label.to_owned())
        }
        ["Git", "PullRequestId", project, repo, id] if id.parse::<u64>().is_ok_and(|n| n > 0) => (
            web_url(source, project, &["_git", repo, "pullrequest", id]).ok()?,
            format!("PR #{id}"),
        ),
        ["Git", "Commit", project, repo, sha]
            if sha.chars().all(|c| c.is_ascii_hexdigit()) && sha.len() >= 7 =>
        {
            (
                web_url(source, project, &["_git", repo, "commit", sha]).ok()?,
                (*sha).into(),
            )
        }
        ["Build", "Build", id] if id.parse::<u64>().is_ok_and(|n| n > 0) => {
            let mut url = web_url(source, project, &["_build", "results"]).ok()?;
            url.query_pairs_mut().append_pair("buildId", id);
            (url, format!("Build #{id}"))
        }
        _ => return None,
    };
    url.set_fragment(None);
    Some((url.into(), label))
}

async fn comments(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    id: u64,
    detail: &mut Detail,
) -> Result<()> {
    let mut continuation: Option<String> = None;
    for _ in 0..5 {
        let mut url = project_url(
            source,
            project,
            "dev.azure.com",
            &["_apis", "wit", "workitems", &id.to_string(), "comments"],
        )?;
        url.set_query(None);
        url.query_pairs_mut()
            .append_pair("api-version", "7.1-preview.4")
            .append_pair("$top", "100")
            .append_pair("order", "desc")
            .append_pair("$expand", "renderedText")
            .append_pair("includeDeleted", "false");
        if let Some(token) = &continuation {
            url.query_pairs_mut()
                .append_pair("continuationToken", token);
        }
        let reply = request(client, key, Method::GET, url, None).await?;
        for comment in reply["comments"]
            .as_array()
            .context("invalid comments reply")?
            .iter()
            .take(100)
        {
            if comment["isDeleted"] == true {
                continue;
            }
            detail.comments.push(Comment {
                content: None,
                author: identity(&comment["createdBy"]),
                created_at: todo::timestamp(&comment["createdDate"]),
                modified_at: todo::timestamp(&comment["modifiedDate"]),
                text: comment["renderedText"]
                    .as_str()
                    .or_else(|| comment["text"].as_str())
                    .unwrap_or_default()
                    .into(),
                format: if comment["renderedText"].is_string()
                    || comment["format"]
                        .as_str()
                        .is_some_and(|f| f.eq_ignore_ascii_case("html"))
                {
                    "html"
                } else {
                    "markdown"
                }
                .into(),
            });
        }
        continuation = reply["continuationToken"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if continuation.is_none() {
            return Ok(());
        }
    }
    detail
        .warn("Only the latest 500 comments are shown. Open the original item for older comments.");
    Ok(())
}

async fn history(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    id: u64,
    definitions: &[Value],
    detail: &mut Detail,
) -> Result<()> {
    // Updates are ordered oldest first. Keep a bounded, explicit history rather than claiming completeness.
    for page in 0..2 {
        let mut url = project_url(
            source,
            project,
            "dev.azure.com",
            &["_apis", "wit", "workitems", &id.to_string(), "updates"],
        )?;
        url.query_pairs_mut()
            .append_pair("$top", "100")
            .append_pair("$skip", &(page * 100).to_string());
        let reply = request(client, key, Method::GET, url, None).await?;
        let values = reply["value"].as_array().context("invalid history reply")?;
        for update in values.iter().take(100) {
            detail.history.push(Change {
                author: identity(&update["revisedBy"]),
                at: update["fields"]["System.ChangedDate"]
                    .get("newValue")
                    .and_then(todo::timestamp)
                    .or_else(|| todo::timestamp(&update["revisedDate"])),
                fields: update["fields"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(reference, change)| detail_field(reference, change, definitions))
                    .collect(),
            });
        }
        if values.len() < 100 {
            detail.history.reverse();
            return Ok(());
        }
    }
    detail.history.reverse();
    detail.warn(
        "Only the first 200 revisions are shown. Open the original item for the complete history.",
    );
    Ok(())
}

async fn relations(
    client: &Client,
    key: &str,
    cache: &Store,
    source: &Source,
    project: &str,
    item: &Value,
    detail: &mut Detail,
) -> Result<()> {
    let values = item["relations"].as_array().cloned().unwrap_or_default();
    let mut related = BTreeMap::new();
    let mut task_kinds = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let prefix = format!("{}/", source.organization.trim_end_matches('/'));
    for relation in &values {
        if let Some(raw) = relation["url"].as_str()
            && raw.starts_with(&prefix)
            && relation["rel"]
                .as_str()
                .is_some_and(|r| r.starts_with("System.LinkTypes."))
            && let Some(id) = raw
                .rsplit('/')
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n > 0)
        {
            ids.insert(id);
        }
    }
    if !ids.is_empty() {
        let mut url = organization_url(source, &["_apis", "wit", "workitems"])?;
        url.query_pairs_mut()
            .append_pair(
                "ids",
                &ids.iter()
                    .take(200)
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            )
            .append_pair(
                "fields",
                "System.Id,System.Title,System.TeamProject,System.WorkItemType,System.State,System.CreatedDate,Microsoft.VSTS.Scheduling.StartDate,Microsoft.VSTS.Scheduling.FinishDate,Microsoft.VSTS.Common.ClosedDate",
            )
            .append_pair("errorPolicy", "Omit");
        match request(client, key, Method::GET, url, None).await {
            Ok(reply) => {
                for item in reply["value"].as_array().into_iter().flatten() {
                    if let Some(id) = item["id"].as_u64().filter(|id| ids.contains(id))
                        && let Ok(reference) =
                            tasks::item(source, field(item, "System.TeamProject"), item)
                    {
                        related.insert(id, reference);
                    }
                }
            }
            Err(_) => detail.warn(
                "Related work item titles could not be read. Their original links are retained.",
            ),
        }
        if ids.len() > 200 {
            detail.warn("Only 200 related work item titles could be resolved.");
        }
        for project in related
            .values()
            .map(|item| &item.project)
            .collect::<BTreeSet<_>>()
        {
            let url = project_url(
                source,
                project,
                "dev.azure.com",
                &[
                    "_apis",
                    "wit",
                    "workitemtypecategories",
                    "Microsoft.TaskCategory",
                ],
            )?;
            match todo::metadata(client, key, cache, url).await {
                Ok(reply) => {
                    let kinds: BTreeSet<String> = reply["workItemTypes"].as_array().into_iter().flatten()
                        .filter_map(|kind| kind["name"].as_str().map(str::to_owned)).collect();
                    task_kinds.insert(project.clone(), kinds);
                }
                Err(_) => detail.warn("Related task types could not be read. Open the original items if their detail is unavailable."),
            }
        }
    }
    for relation in values {
        let raw = relation["url"].as_str().unwrap_or_default();
        let kind = relation["rel"].as_str().unwrap_or_default();
        let name = if kind == "AttachedFile" {
            Some("Attachment")
        } else {
            relation["attributes"]["name"].as_str()
        }
        .unwrap_or(match kind {
            "System.LinkTypes.Hierarchy-Forward" => "Child",
            "System.LinkTypes.Hierarchy-Reverse" => "Parent",
            "System.LinkTypes.Related" => "Related",
            "AttachedFile" => "Attachment",
            "Hyperlink" => "Link",
            _ => kind,
        });
        let resolved = raw
            .rsplit('/')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|id| related.get(&id))
            .filter(|_| raw.starts_with(&prefix) && kind.starts_with("System.LinkTypes."));
        let artifact = artifact_link(source, project, raw);
        detail.relations.push(Relation {
            item: resolved.cloned(),
            task: resolved.is_some_and(|item| {
                task_kinds
                    .get(&item.project)
                    .is_some_and(|kinds| kinds.contains(&item.kind))
            }),
            name: name.into(),
            url: resolved
                .map(|r| r.url.clone())
                .or_else(|| artifact.as_ref().map(|a| a.0.clone()))
                .or_else(|| https_link(raw)),
            detail: resolved
                .map(|r| format!("#{} {} — {}", r.id, r.title, r.state))
                .or_else(|| artifact.map(|a| a.1))
                .unwrap_or_else(|| {
                    (if kind == "AttachedFile" {
                        relation["attributes"]["name"].as_str()
                    } else {
                        relation["attributes"]["comment"].as_str()
                    })
                    .filter(|s| !s.is_empty())
                    .unwrap_or(raw)
                    .into()
                }),
        });
    }
    Ok(())
}

pub async fn get(
    client: &Client,
    key: &str,
    catalog: &str,
    cache: &Store,
    args: &DetailRequest,
) -> Result<Detail> {
    get_kind(client, key, catalog, cache, args, false).await
}

pub async fn get_task(
    client: &Client,
    key: &str,
    catalog: &str,
    cache: &Store,
    args: &DetailRequest,
) -> Result<Detail> {
    get_kind(client, key, catalog, cache, args, true).await
}

async fn get_kind(
    client: &Client,
    key: &str,
    catalog: &str,
    cache: &Store,
    args: &DetailRequest,
    task: bool,
) -> Result<Detail> {
    args.validate(false)?;
    let catalog = Catalog::parse(catalog)?.own(client, key).await?;
    let mut found = None;
    for source in &catalog.sources {
        for project in &source.projects {
            if args
                .project
                .as_ref()
                .is_some_and(|p| !p.eq_ignore_ascii_case(project))
            {
                continue;
            }
            let mut url = project_url(
                source,
                project,
                "dev.azure.com",
                &["_apis", "wit", "workitems", &args.id.to_string()],
            )?;
            url.query_pairs_mut().append_pair("$expand", "all");
            let item = match request(client, key, Method::GET, url, None).await {
                Ok(item) => item,
                Err(error) if error.to_string().ends_with("HTTP 404") => continue,
                Err(error) => return Err(error),
            };
            if item["id"].as_u64() != Some(args.id)
                || !field(&item, "System.TeamProject").eq_ignore_ascii_case(project)
            {
                continue;
            }
            // Verify the type against this project's process, including closed HUs and bugs.
            let mut kinds = BTreeSet::new();
            for category in if task {
                &["Microsoft.TaskCategory"][..]
            } else {
                &["Microsoft.RequirementCategory", "Microsoft.BugCategory"][..]
            } {
                match todo::metadata(
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
                .await
                {
                    Ok(reply) => {
                        kinds.extend(
                            reply["workItemTypes"]
                                .as_array()
                                .context("invalid work item category")?
                                .iter()
                                .filter_map(|k| k["name"].as_str().map(str::to_owned)),
                        );
                    }
                    Err(error) if error.to_string().ends_with("HTTP 404") => (),
                    Err(error) => return Err(error),
                }
            }
            ensure!(
                kinds.contains(field(&item, "System.WorkItemType")),
                if task {
                    "invalid work item type: select a task"
                } else {
                    "invalid work item type: select an HU or bug"
                }
            );
            ensure!(
                found.is_none(),
                "invalid ambiguous work item ID: select a project and an Azure source with one organization"
            );
            found = Some((source, project, item));
        }
    }
    let (source, project, item) =
        found.context("invalid work item: not found in the configured Azure projects")?;
    let reference = item_reference(source, project, &item)?;
    let metadata = todo::metadata(
        client,
        key,
        cache,
        organization_url(source, &["_apis", "wit", "fields"])?,
    )
    .await;
    let definitions = metadata
        .as_ref()
        .ok()
        .and_then(|v| v["value"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut fields: Vec<_> = item["fields"]
        .as_object()
        .context("invalid work item fields")?
        .iter()
        .map(|(name, value)| detail_field(name, value, &definitions))
        .collect();
    let type_metadata = todo::metadata(
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
                "workitemtypes",
                field(&item, "System.WorkItemType"),
            ],
        )?,
    )
    .await;
    let type_fields = type_metadata
        .as_ref()
        .ok()
        .and_then(|v| v["fields"].as_array());
    if let Some(type_fields) = type_fields {
        for field in type_fields {
            if let Some(reference) = field["referenceName"].as_str()
                && !fields.iter().any(|f| f.reference == reference)
            {
                fields.push(detail_field(reference, &Value::Null, &definitions));
            }
        }
    }
    let dates = fields
        .iter()
        .filter(|f| {
            todo::date_field(&json!({"type":f.kind,"name":f.name,"referenceName":f.reference}))
        })
        .map(|f| Date {
            field: f.reference.clone(),
            name: f.name.clone(),
            value: todo::timestamp(&f.value),
        })
        .collect();
    let mut detail = Detail {
        item: Item {
            id: args.id,
            title: field(&item, "System.Title").into(),
            url: reference.url,
            project: project.clone(),
            kind: field(&item, "System.WorkItemType").into(),
            state: field(&item, "System.State").into(),
            created_at: todo::timestamp(&item["fields"]["System.CreatedDate"]),
            dates,
        },
        fields,
        comments: Vec::new(),
        relations: Vec::new(),
        history: Vec::new(),
        partial: false,
        warnings: Vec::new(),
    };
    if type_fields.is_none() {
        detail.warn("Some empty process fields could not be read. Populated fields are retained.");
    }
    if definitions.is_empty() {
        detail.warn("Field labels could not be read. Original field references are shown.");
    }
    if comments(client, key, source, project, args.id, &mut detail)
        .await
        .is_err()
    {
        detail.warn("Comments could not be read. Open the original item or try again.");
    }
    if relations(client, key, cache, source, project, &item, &mut detail)
        .await
        .is_err()
    {
        detail.warn("Some related items could not be read. Open the original item or try again.");
    }
    if history(
        client,
        key,
        source,
        project,
        args.id,
        &definitions,
        &mut detail,
    )
    .await
    .is_err()
    {
        detail.warn("History could not be read. Open the original item or try again.");
    }
    for field in &mut detail.fields {
        if field.kind == "html"
            && let Some(raw) = field.value.as_str()
        {
            field.content = Some(crate::tools::todo::rich_nodes(raw, &detail.item.url));
        }
    }
    for comment in &mut detail.comments {
        if comment.format == "html" {
            comment.content = Some(crate::tools::todo::rich_nodes(
                &comment.text,
                &detail.item.url,
            ));
        }
    }

    Ok(detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    #[tokio::test]
    async fn detail_keeps_custom_fields_comments_artifacts_and_history_but_rejects_out_of_scope_ids()
     {
        let server = MockServer::start().await;
        TEST_SERVER.with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        let item = |id, project, kind| {
            json!({"id":id,"fields":{
            "System.TeamProject":project,"System.WorkItemType":kind,"System.Title":"Receipt downloads","System.State":"Active",
            "System.AssignedTo":{"displayName":"Alex Doe","uniqueName":"alex@example.test"},"System.CreatedDate":"2026-10-01T12:00:00Z",
            "Microsoft.VSTS.Common.AcceptanceCriteria":"<ul><li>Downloads the receipt</li></ul>","Custom.Evidence":"<p>Test results</p>"
        },"relations":[{"rel":"ArtifactLink","url":"vstfs:///Git/Ref/project%2Frepo%2FGBfeature%2Freceipt","attributes":{"name":"Branch"}},
            {"rel":"ArtifactLink","url":"vstfs:///Git/PullRequestId/project%2Frepo%2F12","attributes":{"name":"Pull Request"}},
            {"rel":"AttachedFile","url":"https://dev.azure.com/example/_apis/wit/attachments/file","attributes":{"name":"receipt.png"}},
            {"rel":"Hyperlink","url":"javascript:alert(1)"},
            {"rel":"System.LinkTypes.Hierarchy-Forward","url":"https://dev.azure.com/example/_apis/wit/workitems/20"},
            {"rel":"System.LinkTypes.Related","url":"https://dev.azure.com/example/_apis/wit/workitems/21"},
            {"rel":"System.LinkTypes.Hierarchy-Forward","url":"https://dev.azure.com/example/_apis/wit/workitems/22"},
            {"rel":"System.LinkTypes.Hierarchy-Forward","url":"https://dev.azure.com/other/_apis/wit/workitems/20"}]})
        };
        for (id, project, kind) in [
            (7, "Payments", "User Story"),
            (8, "Private", "User Story"),
            (9, "Payments", "Task"),
            (10, "Payments", "Bug"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/example/Payments/_apis/wit/workitems/{id}")))
                .and(query_param("$expand", "all"))
                .respond_with(ResponseTemplate::new(200).set_body_json(item(id, project, kind)))
                .mount(&server)
                .await;
        }
        for (endpoint, reply) in [
            (
                "/example/Payments/_apis/wit/workitemtypecategories/Microsoft.TaskCategory",
                json!({"workItemTypes":[{"name":"Development task"}]}),
            ),
            (
                "/example/_apis/wit/workitems",
                json!({"value":[
                    {"id":20,"fields":{"System.TeamProject":"Payments","System.Title":"Verify downloads","System.WorkItemType":"Development task","System.State":"Done","System.CreatedDate":"2026-10-01T12:00:00Z","Microsoft.VSTS.Common.ClosedDate":"2026-10-02T12:00:00Z"}},
                    {"id":21,"fields":{"System.TeamProject":"Payments","System.Title":"Download failure","System.WorkItemType":"Bug","System.State":"Active"}},
                    {"id":22,"fields":{"System.TeamProject":"Private","System.Title":"Private task","System.WorkItemType":"Development task","System.State":"Done"}}
                ]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypecategories/Microsoft.RequirementCategory",
                json!({"workItemTypes":[{"name":"User Story"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypecategories/Microsoft.BugCategory",
                json!({"workItemTypes":[{"name":"Bug"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypes/User%20Story",
                json!({"fields":[{"referenceName":"Custom.Empty"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitemtypes/Bug",
                json!({"fields":[]}),
            ),
            (
                "/example/_apis/wit/fields",
                json!({"value":[{"referenceName":"Custom.Evidence","name":"Evidence","type":"html"},{"referenceName":"Microsoft.VSTS.Common.AcceptanceCriteria","name":"Acceptance Criteria","type":"html"}]}),
            ),
            (
                "/example/Payments/_apis/wit/workitems/7/comments",
                json!({"comments":[{"text":"<p>Validated</p>","format":"html","createdBy":{"displayName":"Taylor"},"createdDate":"2026-10-02T12:00:00Z"}],"continuationToken":"page-two"}),
            ),
            (
                "/example/Payments/_apis/wit/workitems/7/updates",
                json!({"value":[{"revisedBy":{"displayName":"Alex"},"fields":{"System.ChangedDate":{"newValue":"2026-10-02T12:00:00Z"},"System.State":{"oldValue":"New","newValue":"Active"}}}]}),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_json(reply))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/example/Payments/_apis/wit/workitems/7/comments"))
            .and(query_param("api-version", "7.1-preview.4"))
            .and(query_param("continuationToken", "page-two"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"comments":[{"text":"Second page","createdBy":{"displayName":"Morgan"}}]}),
            ))
            .with_priority(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Store::open(&dir.path().join("cache.db")).unwrap();
        let catalog = "[[sources]]\norganization='https://dev.azure.com/example/'\nprojects=['Payments']\nauthor_email='me@example.test'";
        let args = |id| DetailRequest {
            id,
            source: None,
            project: Some("Payments".into()),
        };
        let detail = get(&Client::new(), "test-key", catalog, &cache, &args(7))
            .await
            .unwrap();
        assert!(!detail.partial, "{:?}", detail.warnings);
        assert_eq!(detail.comments.len(), 2);
        assert!(
            detail
                .fields
                .iter()
                .any(|f| f.reference == "Custom.Empty" && f.value.is_null())
        );
        assert_eq!(detail.comments[0].author, "Taylor");
        assert_eq!(
            detail
                .fields
                .iter()
                .find(|f| f.reference == "Custom.Evidence")
                .unwrap()
                .kind,
            "html"
        );
        assert_eq!(detail.relations[0].detail, "feature/receipt");
        assert!(
            detail.relations[0]
                .url
                .as_deref()
                .unwrap()
                .contains("version=GBfeature%2Freceipt")
        );
        assert!(
            detail.relations[1]
                .url
                .as_deref()
                .unwrap()
                .ends_with("/pullrequest/12")
        );
        assert!(detail.relations[3].url.is_none());
        let child = &detail.relations[4];
        assert!(child.task);
        let child_item = child.item.as_ref().unwrap();
        assert_eq!(child_item.id, 20);
        assert_eq!(child_item.kind, "Development task");
        assert_eq!(child_item.state, "Done");
        assert_eq!(child_item.dates[0].name, "ClosedDate");
        assert!(child_item.url.ends_with("/Payments/_workitems/edit/20"));
        assert!(!detail.relations[5].task);
        assert_eq!(detail.relations[5].item.as_ref().unwrap().kind, "Bug");
        assert!(detail.relations[6].item.is_none());
        assert!(detail.relations[7].item.is_none());
        assert_eq!(detail.history[0].author, "Alex");
        assert!(
            get(&Client::new(), "test-key", catalog, &cache, &args(8))
                .await
                .is_err()
        );
        assert!(
            get(&Client::new(), "test-key", catalog, &cache, &args(9))
                .await
                .is_err()
        );
        let bug = get(&Client::new(), "test-key", catalog, &cache, &args(10))
            .await
            .unwrap();
        assert_eq!(bug.item.kind, "Bug");
        assert!(bug.partial); // Unavailable discussion/history are never presented as complete.
        TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }
}
