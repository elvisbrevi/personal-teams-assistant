//! What the workflow engine reads from Azure DevOps, read-only, with the enabled source's read
//! credential: the pull requests of one repository whose source branch is the one a coding
//! session reported (`_apis/git/repositories/{id}/pullrequests?searchCriteria.sourceRefName=…`),
//! whether a reported branch still exists (`_apis/git/repositories/{id}/refs?filter=heads/…`),
//! and whether a work item is assigned to the source's user. Nothing here writes, merges,
//! completes or changes a pull request or a work item.
use super::{Catalog, Source, project_url, request};
use crate::tools::todo::Detail;
use anyhow::{Context, Result, ensure};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};

/// At most this many pull requests of one branch are read.
const TOP: usize = 50;

/// A pull request as a workflow run records it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    pub id: u64,
    /// The repository's catalog ID.
    pub repository: String,
    pub title: String,
    /// `active`, `completed` or `abandoned`, as Azure DevOps reports it.
    pub status: String,
    /// Its verified link, built from the source's organization, the project, the repository's
    /// name and the pull request's ID.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    /// The person resumed the run with this pull request abandoned: it no longer stops it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepted: bool,
}

/// Whether a branch name a coding session reported is one this read asks for: a plain Git
/// branch name (letters, digits, `/`, `.`, `_`, `-`), without `..`, a leading `-` or `/`, or a
/// trailing `/`, `.` or `.lock`.
pub fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 250
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
        && !branch.contains("..")
        && !branch.contains("//")
        && !branch.starts_with(['-', '/', '.'])
        && !branch.ends_with(['/', '.'])
        && !branch.ends_with(".lock")
}

/// The configured source of an organization (`https://dev.azure.com/NAME`).
fn source(catalog: &str, organization: &str) -> Result<Source> {
    let wanted = crate::workflows::organization_name(organization)
        .context("invalid organization: use https://dev.azure.com/NAME")?;
    Catalog::parse(catalog)?
        .sources
        .into_iter()
        .find(|s| crate::workflows::organization_name(&s.organization).as_ref() == Some(&wanted))
        .context("invalid organization: it is not in the Azure source's catalog")
}

fn text(value: &serde_json::Value, max: usize) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

/// The pull requests of `repository` (catalog ID and name) whose source branch is `branch`, of
/// any status, newest first.
pub(crate) async fn pull_requests(
    client: &Client,
    key: &str,
    catalog: &str,
    organization: &str,
    project: &str,
    repository: (&str, &str),
    branch: &str,
) -> Result<Vec<PullRequest>> {
    let (id, name) = repository;
    ensure!(
        uuid::Uuid::parse_str(id).is_ok() && id.len() == 36,
        "invalid repository ID"
    );
    ensure!(valid_branch(branch), "invalid branch name");
    let source = source(catalog, organization)?;
    let mut url = project_url(
        &source,
        project,
        "dev.azure.com",
        &["_apis", "git", "repositories", id, "pullrequests"],
    )?;
    let reference = format!("refs/heads/{branch}");
    url.query_pairs_mut()
        .append_pair("searchCriteria.sourceRefName", &reference)
        .append_pair("searchCriteria.status", "all")
        .append_pair("$top", &TOP.to_string());
    let value = request(client, key, Method::GET, url, None).await?;
    let page = value["value"]
        .as_array()
        .context("invalid Azure DevOps pull requests")?;
    let mut found = Vec::new();
    for pull in page.iter().take(TOP) {
        let Some(number) = pull["pullRequestId"].as_u64().filter(|n| *n > 0) else {
            continue;
        };
        // Only the branch and repository asked for, whatever else the reply holds.
        if pull["sourceRefName"].as_str() != Some(reference.as_str())
            || !pull["repository"]["id"]
                .as_str()
                .is_some_and(|r| r.eq_ignore_ascii_case(id))
        {
            continue;
        }
        let status = match pull["status"].as_str() {
            Some(status @ ("active" | "completed" | "abandoned")) => status.to_owned(),
            _ => "unknown".to_owned(),
        };
        let mut link = project_url(
            &source,
            project,
            "dev.azure.com",
            &["_git", name, "pullrequest", &number.to_string()],
        )?;
        link.set_query(None);
        found.push(PullRequest {
            id: number,
            repository: id.to_lowercase(),
            title: text(&pull["title"], 400),
            status,
            url: crate::tools::todo::https_link(link.as_str())
                .context("invalid pull request link")?,
            created_at: pull["creationDate"]
                .is_string()
                .then(|| text(&pull["creationDate"], 40)),
            closed_at: pull["closedDate"]
                .is_string()
                .then(|| text(&pull["closedDate"], 40)),
            accepted: false,
        });
    }
    found.sort_by_key(|a| std::cmp::Reverse(a.id));
    Ok(found)
}

/// Whether `branch` exists in `repository` (its catalog ID) now: a ref named exactly
/// `refs/heads/BRANCH` in the repository's refs that start so.
pub(crate) async fn branch_exists(
    client: &Client,
    key: &str,
    catalog: &str,
    organization: &str,
    project: &str,
    repository: &str,
    branch: &str,
) -> Result<bool> {
    ensure!(
        uuid::Uuid::parse_str(repository).is_ok() && repository.len() == 36,
        "invalid repository ID"
    );
    ensure!(valid_branch(branch), "invalid branch name");
    let source = source(catalog, organization)?;
    let mut url = project_url(
        &source,
        project,
        "dev.azure.com",
        &["_apis", "git", "repositories", repository, "refs"],
    )?;
    url.query_pairs_mut()
        .append_pair("filter", &format!("heads/{branch}"))
        .append_pair("$top", &TOP.to_string());
    let value = request(client, key, Method::GET, url, None).await?;
    let page = value["value"]
        .as_array()
        .context("invalid Azure DevOps refs")?;
    let wanted = format!("refs/heads/{branch}");
    Ok(page.iter().any(|r| r["name"].as_str() == Some(wanted.as_str())))
}

/// Whether the item is assigned to the source's user: its Assigned To, or a developer role of
/// its process (an identity field named «developer» or «desarrollador», as To-Do reads them),
/// names the user of the source of its organization.
pub(crate) fn assigned(catalog: &str, detail: &Detail) -> Result<bool> {
    let organization = crate::workflows::director::item_organization(&detail.item.url)
        .context("The work item has no Azure DevOps link.")?;
    let source = source(catalog, &format!("https://dev.azure.com/{organization}"))?;
    let email = source.author_email.to_lowercase();
    Ok(detail.fields.iter().any(|field| {
        let name = field.name.to_lowercase();
        let role = field.reference == "System.AssignedTo"
            || name.contains("developer")
            || name.contains("desarrollador");
        let identity = field.value["uniqueName"]
            .as_str()
            .or_else(|| field.value.as_str())
            .unwrap_or_default()
            .to_lowercase();
        role && (identity == email || identity.ends_with(&format!("<{email}>")))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    const CATALOG: &str = "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='me@example.test'";
    const REPO: &str = "0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f";

    #[test]
    fn only_plain_branch_names_are_asked_for() {
        for good in ["feature/42-pay", "fix_1", "release/2026.11"] {
            assert!(valid_branch(good), "{good}");
        }
        for bad in [
            "",
            "-x",
            "a..b",
            "a b",
            "refs/heads/../x",
            "x.lock",
            "x/",
            "/x",
            "a?b",
            "a//b",
        ] {
            assert!(!valid_branch(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn pull_requests_are_read_by_source_branch_with_verified_links_and_nothing_is_written() {
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{REPO}/pullrequests"
            )))
            .and(query_param(
                "searchCriteria.sourceRefName",
                "refs/heads/feature/42",
            ))
            .and(query_param("searchCriteria.status", "all"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"pullRequestId":7,"status":"completed","title":"Pay","sourceRefName":"refs/heads/feature/42",
                 "repository":{"id":REPO},"creationDate":"2026-11-01T10:00:00Z","closedDate":"2026-11-02T10:00:00Z"},
                {"pullRequestId":9,"status":"active","title":"Other branch","sourceRefName":"refs/heads/other",
                 "repository":{"id":REPO}},
                {"pullRequestId":8,"status":"strange","title":"Pay again\u{7}","sourceRefName":"refs/heads/feature/42",
                 "repository":{"id":REPO}}
            ]})))
            .mount(&server)
            .await;
        let client = Client::new();
        let found = pull_requests(
            &client,
            "key",
            CATALOG,
            "https://dev.azure.com/example",
            "Payments",
            (REPO, "payments api"),
            "feature/42",
        )
        .await
        .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].id, 8);
        assert_eq!(found[0].status, "unknown");
        assert_eq!(found[0].title, "Pay again");
        assert_eq!(found[1].status, "completed");
        assert_eq!(
            found[1].url,
            "https://dev.azure.com/example/Payments/_git/payments%20api/pullrequest/7"
        );
        // A branch that is not a plain name, or another organization, is never asked.
        assert!(
            pull_requests(
                &client,
                "key",
                CATALOG,
                "https://dev.azure.com/example",
                "Payments",
                (REPO, "payments-api"),
                "x;rm"
            )
            .await
            .is_err()
        );
        assert!(
            pull_requests(
                &client,
                "key",
                CATALOG,
                "https://dev.azure.com/other",
                "Payments",
                (REPO, "payments-api"),
                "feature/42"
            )
            .await
            .is_err()
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests.iter().all(|r| r.method.as_str() == "GET"));
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }

    #[tokio::test]
    async fn a_branch_exists_only_when_its_exact_ref_is_listed_and_nothing_is_written() {
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{REPO}/refs"
            )))
            .and(query_param("filter", "heads/feature/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"name":"refs/heads/feature/42-other","objectId":"a"},
                {"name":"refs/heads/feature/42","objectId":"b"}
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{REPO}/refs"
            )))
            .and(query_param("filter", "heads/feature/4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"name":"refs/heads/feature/42","objectId":"b"}
            ]})))
            .mount(&server)
            .await;
        let client = Client::new();
        let exists = |branch: &'static str| {
            let client = client.clone();
            async move {
                branch_exists(
                    &client,
                    "key",
                    CATALOG,
                    "https://dev.azure.com/example",
                    "Payments",
                    REPO,
                    branch,
                )
                .await
            }
        };
        assert!(exists("feature/42").await.unwrap());
        // A prefix of an existing branch is not that branch.
        assert!(!exists("feature/4").await.unwrap());
        assert!(exists("x;rm").await.is_err());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|r| r.method.as_str() == "GET"));
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }

    #[test]
    fn an_item_is_assigned_when_its_assigned_to_or_a_developer_role_names_the_user() {
        let detail = |reference: &str, name: &str, value: serde_json::Value| Detail {
            item: crate::tools::todo::Item {
                id: 1,
                title: "T".into(),
                url: "https://dev.azure.com/example/Payments/_workitems/edit/1".into(),
                project: "Payments".into(),
                state: "New".into(),
                kind: "User Story".into(),
                category: "requirement".into(),
                created_at: None,
                changed_at: None,
                dates: Vec::new(),
            },
            fields: vec![crate::tools::todo::DetailField {
                reference: reference.into(),
                name: name.into(),
                kind: "string".into(),
                value,
                content: None,
            }],
            comments: Vec::new(),
            relations: Vec::new(),
            history: Vec::new(),
            partial: false,
            warnings: Vec::new(),
        };
        let me = json!({"displayName":"Me","uniqueName":"ME@example.test"});
        let other = json!({"displayName":"Other","uniqueName":"other@example.test"});
        assert!(
            assigned(
                CATALOG,
                &detail("System.AssignedTo", "Assigned To", me.clone())
            )
            .unwrap()
        );
        assert!(
            assigned(
                CATALOG,
                &detail("Custom.Dev1", "Desarrollador 1", me.clone())
            )
            .unwrap()
        );
        assert!(!assigned(CATALOG, &detail("System.AssignedTo", "Assigned To", other)).unwrap());
        assert!(!assigned(CATALOG, &detail("System.CreatedBy", "Created By", me)).unwrap());
    }
}
