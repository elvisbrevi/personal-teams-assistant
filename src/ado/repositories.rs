//! The Git repositories of one project (`_apis/git/repositories`), from which a workflow's
//! repositories are chosen. Read with the enabled source's read credential, bounded and cached
//! like the process metadata; nothing here writes or changes a source.
use super::{Catalog, cached_read, project_url, request};
use crate::{state::Store, workflows::CatalogRepository};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// More repositories than this in one project fail instead of returning a partial list.
pub const MAX_REPOSITORIES: usize = 1000;
const CACHE_SECONDS: i64 = 300;

fn text(value: &Value, max: usize) -> String {
    value
        .as_str()
        .filter(|s| s.len() <= max && !s.chars().any(char::is_control))
        .unwrap_or_default()
        .to_owned()
}

/// The project's repositories in one of the source's configured organizations. Without
/// `organization`, the project's name must be unique across them.
pub(crate) async fn list(
    client: &Client,
    key: &str,
    catalog: &str,
    cache: &Store,
    organization: Option<&str>,
    project: &str,
) -> Result<Value> {
    let wanted = organization
        .map(|o| {
            crate::workflows::organization_name(o)
                .context("invalid organization: use https://dev.azure.com/NAME")
        })
        .transpose()?;
    let targets = super::implementation::request_targets(client, key, catalog).await?;
    let matching: Vec<&Value> = targets
        .iter()
        .filter(|t| {
            t["project"]
                .as_str()
                .is_some_and(|p| p.eq_ignore_ascii_case(project))
                && wanted.as_ref().is_none_or(|w| {
                    t["organization"]
                        .as_str()
                        .and_then(crate::workflows::organization_name)
                        .as_ref()
                        == Some(w)
                })
        })
        .collect();
    let target = match matching.as_slice() {
        [] => bail!(
            "invalid project: {project} is not in the configured organizations of the Azure source"
        ),
        [target] => *target,
        _ => bail!(
            "invalid project: {project} exists in several organizations; choose one with organization"
        ),
    };
    let organization = target["organization"].as_str().unwrap_or_default();
    let project = target["project"].as_str().unwrap_or_default();
    let catalog = Catalog::parse(catalog)?;
    let source = catalog
        .sources
        .iter()
        .find(|s| s.organization == organization)
        .context("Project outside the Azure source catalog.")?;
    let url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "git", "repositories"],
    )?;
    // Metadata only; the credential fingerprint prevents reuse after an account change.
    let cache_key = format!(
        "workflow-repositories:{:x}:{url}",
        Sha256::digest(key.as_bytes())
    );
    let value = cached_read(
        cache,
        &cache_key,
        CACHE_SECONDS,
        request(client, key, Method::GET, url, None),
    )
    .await?;
    let page = value["value"]
        .as_array()
        .context("invalid Azure DevOps repositories")?;
    ensure!(
        page.len() <= MAX_REPOSITORIES,
        "Project {project} has more than {MAX_REPOSITORIES} Git repositories; the repository catalog would be incomplete."
    );
    let mut repositories = Vec::new();
    for repository in page {
        if repository["isDisabled"] == true {
            continue;
        }
        let id = text(&repository["id"], 36);
        let name = text(&repository["name"], 200);
        if uuid::Uuid::parse_str(&id).is_err() || name.trim().is_empty() {
            continue;
        }
        repositories.push(CatalogRepository {
            id,
            name,
            default_branch: text(&repository["defaultBranch"], 400),
            remote_url: text(&repository["remoteUrl"], 2048),
            ssh_url: text(&repository["sshUrl"], 2048),
            web_url: text(&repository["webUrl"], 2048),
        });
    }
    repositories.sort_by_key(|a| a.name.to_lowercase());
    Ok(json!({"organization":organization,"project":project,"repositories":repositories}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    const CATALOG: &str = "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='me@example.test'";

    async fn server(repositories: Value) -> MockServer {
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        Mock::given(method("GET"))
            .and(path("/example/_apis/projects"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"value":[{"name":"Payments"},{"name":"Billing"}]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/example/Payments/_apis/git/repositories"))
            // The read credential, as basic authentication with an empty user.
            .and(header("authorization", "Basic OnJlYWQ="))
            .respond_with(ResponseTemplate::new(200).set_body_json(repositories))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn the_catalog_is_read_with_the_read_credential_cached_and_bounded() {
        let server = server(json!({"count":3,"value":[
            {"id":"7d9e1f20-3a4b-4c5d-8e6f-0a1b2c3d4e5f","name":"payments-web","defaultBranch":"refs/heads/main","remoteUrl":"https://example@dev.azure.com/example/Payments/_git/payments-web","sshUrl":"git@ssh.dev.azure.com:v3/example/Payments/payments-web","webUrl":"https://dev.azure.com/example/Payments/_git/payments-web"},
            {"id":"0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f","name":"payments-api","defaultBranch":"refs/heads/main"},
            {"id":"1f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f","name":"archived","isDisabled":true}
        ]}))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Store::open(&dir.path().join("cache.db")).unwrap();
        let client = Client::new();
        let listed = list(&client, "read", CATALOG, &cache, None, "payments")
            .await
            .unwrap();
        assert_eq!(listed["project"], "Payments");
        assert_eq!(listed["organization"], "https://dev.azure.com/example");
        let names: Vec<&str> = listed["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["payments-api", "payments-web"]);
        // Cached: the second read does not reach Azure DevOps again.
        let again = list(
            &client,
            "read",
            CATALOG,
            &cache,
            Some("https://dev.azure.com/example"),
            "Payments",
        )
        .await
        .unwrap();
        assert_eq!(again, listed);
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .all(|r| r.method == wiremock::http::Method::GET)
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path().ends_with("/_apis/git/repositories"))
                .count(),
            1
        );
        assert!(
            list(&client, "read", CATALOG, &cache, None, "Unknown")
                .await
                .is_err()
        );
        assert!(
            list(
                &client,
                "read",
                CATALOG,
                &cache,
                Some("https://evil.example/x"),
                "Payments"
            )
            .await
            .is_err()
        );
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }

    #[tokio::test]
    async fn a_project_beyond_the_coverage_bound_fails_explicitly() {
        let many: Vec<Value> = (0..=MAX_REPOSITORIES)
            .map(|_| json!({"id":uuid::Uuid::new_v4().to_string(),"name":"repo"}))
            .collect();
        let _server = server(json!({"value":many})).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Store::open(&dir.path().join("cache.db")).unwrap();
        let error = list(&Client::new(), "read", CATALOG, &cache, None, "Payments")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("more than 1000"), "{error}");
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }
}
