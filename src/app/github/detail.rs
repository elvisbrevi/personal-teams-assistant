//! GitHub epic detail through the existing connected account, without URL-directed reads.
use super::*;
use crate::tools::todo::{
    Change, Comment, Date, Detail, DetailField, DetailRequest, Item, Relation, validate_link,
};
use serde_json::{Value, json};

pub(in crate::app) async fn epic(args: &DetailRequest) -> Result<Detail> {
    args.validate(true)?;
    epic_with(
        &client()?,
        &access_token().await?,
        "https://api.github.com/",
        args,
    )
    .await
}
pub(in crate::app) async fn issue(args: &DetailRequest) -> Result<Detail> {
    detail_with(
        &client()?,
        &access_token().await?,
        "https://api.github.com/",
        args,
        false,
    )
    .await
}
fn field(reference: &str, name: &str, kind: &str, value: Value) -> DetailField {
    DetailField {
        reference: reference.into(),
        name: name.into(),
        kind: kind.into(),
        value,
        content: None,
    }
}
fn issue_url(issue: &Value, repo: &str, id: u64) -> Result<String> {
    ensure!(
        issue["number"].as_u64() == Some(id) && issue.get("pull_request").is_none(),
        "invalid Epic response"
    );
    let raw = issue["html_url"].as_str().context("invalid Epic URL")?;
    let url = validate_link(raw)?;
    ensure!(
        url.host_str() == Some("github.com")
            && url
                .path()
                .eq_ignore_ascii_case(&format!("/{repo}/issues/{id}")),
        "invalid Epic URL"
    );
    Ok(raw.into())
}
async fn epic_with(
    client: &Client,
    token: &str,
    base: &str,
    args: &DetailRequest,
) -> Result<Detail> {
    detail_with(client, token, base, args, true).await
}
async fn detail_with(
    client: &Client,
    token: &str,
    base: &str,
    args: &DetailRequest,
    require_epic: bool,
) -> Result<Detail> {
    args.validate(true)?;
    let repo = args.project.as_deref().unwrap_or_default();
    let endpoint = format!("{base}repos/{repo}/issues/{}", args.id);
    let issue: Value = get(client, token, &endpoint).await?;
    let url = issue_url(&issue, repo, args.id)?;
    ensure!(
        !require_epic
            || issue["type"]["name"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("Epic")),
        "invalid issue type: select an Epic"
    );
    let mut detail = Detail {
        item: Item {
            id: args.id,
            title: issue["title"]
                .as_str()
                .context("invalid Epic title")?
                .into(),
            url,
            project: repo.into(),
            state: issue["state"]
                .as_str()
                .context("invalid Epic state")?
                .into(),
            kind: issue["type"]["name"].as_str().unwrap_or("Issue").into(),
            created_at: issue["created_at"].as_str().map(str::to_owned),
            dates: vec![Date {
                field: "closed_at".into(),
                name: "Completion date".into(),
                value: issue["closed_at"].as_str().map(str::to_owned),
            }],
        },
        fields: vec![
            field(
                "body",
                "Description",
                "markdown",
                json!(issue["body"].as_str().unwrap_or_default()),
            ),
            field(
                "assignees",
                "Assigned To",
                "string",
                json!(
                    issue["assignees"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|a| a["login"].as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
            field(
                "user",
                "Created by",
                "string",
                issue["user"]["login"].clone(),
            ),
            field(
                "labels",
                "Labels",
                "string",
                json!(
                    issue["labels"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|a| a["name"].as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
            field(
                "milestone",
                "Milestone",
                "string",
                issue["milestone"]["title"].clone(),
            ),
            field(
                "milestone_due",
                "Milestone due date",
                "dateTime",
                issue["milestone"]["due_on"].clone(),
            ),
            field(
                "updated_at",
                "Updated",
                "dateTime",
                issue["updated_at"].clone(),
            ),
            field(
                "closed_by",
                "Closed by",
                "string",
                issue["closed_by"]["login"].clone(),
            ),
            field(
                "state_reason",
                "State reason",
                "string",
                issue["state_reason"].clone(),
            ),
            field("locked", "Locked", "boolean", issue["locked"].clone()),
        ],
        comments: Vec::new(),
        relations: Vec::new(),
        history: Vec::new(),
        partial: false,
        warnings: Vec::new(),
    };
    // Paginate pinned endpoints only; never follow a response's URL or Link header with the token.
    for (suffix, maximum) in [("comments", 5), ("sub_issues", 5), ("events", 2)] {
        for page in 1..=maximum {
            let response = get::<Value>(
                client,
                token,
                &format!("{endpoint}/{suffix}?per_page=100&page={page}"),
            )
            .await;
            let values = match response.as_ref().ok().and_then(Value::as_array) {
                Some(values) => values,
                None => {
                    detail.warn(match suffix {
                        "comments" => {
                            "Comments could not be read. Open the original item or try again."
                        }
                        "sub_issues" => {
                            "Sub-issues could not be read. Open the original epic or try again."
                        }
                        _ => "History could not be read. Open the original item or try again.",
                    });
                    break;
                }
            };
            for value in values.iter().take(100) {
                match suffix {
                    "comments" => detail.comments.push(Comment {
                        content: None,
                        author: value["user"]["login"].as_str().unwrap_or_default().into(),
                        created_at: value["created_at"].as_str().map(str::to_owned),
                        modified_at: value["updated_at"].as_str().map(str::to_owned),
                        text: value["body"].as_str().unwrap_or_default().into(),
                        format: "markdown".into(),
                    }),
                    "sub_issues" => {
                        let raw = value["html_url"].as_str().unwrap_or_default();
                        let valid = validate_link(raw)
                            .ok()
                            .filter(|u| u.host_str() == Some("github.com"));
                        if let Some(url) = valid {
                            let number = value["number"]
                                .as_u64()
                                .context("invalid sub-issue number")?;
                            ensure!(
                                url.path_segments()
                                    .and_then(|mut p| p.next_back())
                                    .is_some_and(|id| id == number.to_string()),
                                "invalid sub-issue URL"
                            );
                            detail.relations.push(Relation {
                                item: Some(Item {
                                    id: number,
                                    title: value["title"].as_str().unwrap_or_default().into(),
                                    url: url.to_string(),
                                    project: url
                                        .path_segments()
                                        .map(|mut parts| {
                                            format!(
                                                "{}/{}",
                                                parts.next().unwrap_or_default(),
                                                parts.next().unwrap_or_default()
                                            )
                                        })
                                        .unwrap_or_default(),
                                    state: value["state"].as_str().unwrap_or_default().into(),
                                    kind: value["type"]["name"].as_str().unwrap_or("Issue").into(),
                                    created_at: value["created_at"].as_str().map(str::to_owned),
                                    dates: Vec::new(),
                                }),
                                task: false,
                                name: "Sub-issue".into(),
                                url: Some(url.into()),
                                detail: format!(
                                    "#{} · {} · {}",
                                    number,
                                    value["title"].as_str().unwrap_or_default(),
                                    value["state"].as_str().unwrap_or_default()
                                ),
                            });
                        } else {
                            detail.warn(
                                "Some sub-issues had unavailable links and could not be shown.",
                            );
                        }
                    }
                    _ => detail.history.push(Change {
                        author: value["actor"]["login"].as_str().unwrap_or_default().into(),
                        at: value["created_at"].as_str().map(str::to_owned),
                        fields: vec![field(
                            "event",
                            value["event"].as_str().unwrap_or("Event"),
                            "string",
                            json!(
                                value["assignee"]["login"]
                                    .as_str()
                                    .or_else(|| value["label"]["name"].as_str())
                                    .or_else(|| value["milestone"]["title"].as_str())
                                    .unwrap_or_default()
                            ),
                        )],
                    }),
                }
            }
            if values.len() < 100 {
                break;
            }
            if page == maximum {
                detail.warn(match suffix { "comments" => "Only the first 500 comments are shown. Open the original item for the complete discussion.", "sub_issues" => "Only the first 500 sub-issues are shown. Open the original epic for the complete list.", _ => "Only the first 200 events are shown. Open the original item for the complete history." });
            }
        }
    }
    detail.history.reverse();
    Ok(detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn epic_detail_requires_repository_and_verified_type_and_reports_partial_sections() {
        let server = MockServer::start().await;
        let epic = |id, kind, url| json!({"number":id,"title":"Release plan","type":{"name":kind},"html_url":url,"state":"open","body":"## Goals\n- Ship","assignees":[{"login":"alex"}],"labels":[{"name":"payments"}],"milestone":{"title":"October"}});
        for (id, kind, url) in [
            (42, "Epic", "https://github.com/example/work/issues/42"),
            (43, "Task", "https://github.com/example/work/issues/43"),
            (44, "Epic", "https://github.com/other/work/issues/44"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/repos/example/work/issues/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(epic(id, kind, url)))
                .mount(&server)
                .await;
        }
        for (suffix, value) in [
            (
                "comments",
                json!([{"user":{"login":"taylor"},"body":"Ready","created_at":"2026-10-01T12:00:00Z"}]),
            ),
            (
                "sub_issues",
                json!([{"number":7,"title":"Receipts","state":"closed","html_url":"https://github.com/example/work/issues/7"}]),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/repos/example/work/issues/42/{suffix}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(value))
                .mount(&server)
                .await;
        }
        let args = |id| DetailRequest {
            id,
            source: None,
            project: Some("example/work".into()),
        };
        let base = format!("{}/", server.uri());
        let detail = epic_with(&Client::new(), "test-key", &base, &args(42))
            .await
            .unwrap();
        assert_eq!(detail.comments[0].author, "taylor");
        assert_eq!(
            detail.relations[0].url.as_deref(),
            Some("https://github.com/example/work/issues/7")
        );
        assert!(
            detail
                .fields
                .iter()
                .any(|f| f.reference == "milestone" && f.value == "October")
        );
        assert!(detail.partial); // Events unavailable; keep the fields, comments and sub-issues.
        for id in [43, 44] {
            assert!(
                epic_with(&Client::new(), "test-key", &base, &args(id))
                    .await
                    .is_err()
            );
        }
        for repo in [
            None,
            Some("../work".into()),
            Some("example/..".into()),
            Some("example/work?token=x".into()),
        ] {
            assert!(
                epic_with(
                    &Client::new(),
                    "test-key",
                    &base,
                    &DetailRequest {
                        project: repo,
                        ..args(42)
                    }
                )
                .await
                .is_err()
            );
        }
    }
}
