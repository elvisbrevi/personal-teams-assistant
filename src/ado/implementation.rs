//! Child creation is authorized only by the implementation flow, never by activity plans.
use super::*;
use crate::tools::todo::{Detail, validate_link};

fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>")
}

pub(crate) async fn create_child(
    client: &Client,
    read_key: &str,
    write_key: &str,
    catalog: &str,
    parent: &Detail,
    child: &Value,
) -> Result<Value> {
    let catalog = Catalog::parse(catalog)?.own(client, read_key).await?;
    let parent_url = validate_link(&parent.item.url)?;
    let source = catalog
        .sources
        .iter()
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
            source,
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
    let kind = category["defaultWorkItemType"]["name"]
        .as_str()
        .or_else(|| {
            kinds
                .iter()
                .find_map(|k| k["name"].as_str().filter(|n| *n == "Task"))
        })
        .context("Default task type is unavailable.")?;
    ensure!(
        kinds.iter().any(|k| k["name"] == kind),
        "Invalid process task type."
    );
    let mut parent_api = project_url(
        source,
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
    let url = project_url(
        source,
        &parent.item.project,
        "dev.azure.com",
        &["_apis", "wit", "workitems", &format!("${kind}")],
    )?;
    #[cfg(test)]
    let url = super::test_url(url);
    let response = client
        .post(url)
        .basic_auth("", Some(write_key))
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
        source,
        &parent.item.project,
        "dev.azure.com",
        &["_workitems", "edit", &id.to_string()],
    )?;
    url.set_query(None);
    Ok(
        json!({"id":id,"title":child["title"],"url":validate_link(url.as_str())?.as_str(),"state":"created"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptions_do_not_inject_provider_html() {
        assert_eq!(html("<script>&\n"), "&lt;script&gt;&amp;<br>");
    }
    #[tokio::test]
    async fn reviewed_children_use_process_task_types_and_only_the_dedicated_write_key() {
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
            &parent,
            &json!({"title":"Implement receipt fix","description":"Keep <script> as text."}),
        )
        .await
        .unwrap();
        assert_eq!(created["id"], 88);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_ne!(
            requests[0].headers["authorization"],
            requests[1].headers["authorization"]
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
}
