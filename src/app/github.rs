mod detail;
use crate::security;
use anyhow::{Context, Result, ensure};
pub(super) use detail::{epic, issue};

pub(super) async fn create_child(
    parent: &crate::tools::todo::Detail,
    child: &serde_json::Value,
) -> Result<serde_json::Value> {
    use serde_json::json;
    crate::tools::todo::validate_repository(&parent.item.project)?;
    ensure!(
        parent.item.kind.eq_ignore_ascii_case("Epic"),
        "Only an Epic can create child issues in this flow."
    );
    let token = access_token().await?;
    let client = client()?;
    let base = format!(
        "https://api.github.com/repos/{}/issues",
        parent.item.project
    );
    let response = client
        .post(&base)
        .bearer_auth(&token)
        .json(&json!({"title":child["title"],"body":child["description"]}))
        .send()
        .await?;
    let created: serde_json::Value = checked_json(response).await?;
    let number = created["number"]
        .as_u64()
        .context("Created issue number unavailable.")?;
    let id = created["id"]
        .as_u64()
        .context("Created issue ID unavailable.")?;
    let url = crate::tools::todo::validate_link(created["html_url"].as_str().unwrap_or_default())?;
    ensure!(
        url.as_str() == format!("https://github.com/{}/issues/{number}", parent.item.project),
        "Invalid created issue URL."
    );
    // The issue exists even if linking fails: retain its link and never create it again.
    let linked = client
        .post(format!("{base}/{}/sub_issues", parent.item.id))
        .bearer_auth(&token)
        .json(&json!({"sub_issue_id":id}))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success());
    Ok(
        json!({"id":number,"title":child["title"],"url":url.as_str(),"state":if linked { "created" } else { "created_unlinked" }}),
    )
}
/// The caller verifies the repository against the connected installation catalog before sending.
pub(super) async fn create_request(
    repository: &str,
    draft: &serde_json::Value,
) -> Result<serde_json::Value> {
    use serde_json::Value;
    crate::tools::todo::validate_repository(repository)?;
    let token = access_token().await?;
    let client = client()?;
    let user: Value = get(&client, &token, "https://api.github.com/user").await?;
    let login = user["login"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("GitHub identity unavailable.")?;
    create_request_at(
        &client,
        &token,
        "https://api.github.com/",
        repository,
        login,
        draft,
    )
    .await
}
async fn create_request_at(
    client: &reqwest::Client,
    token: &str,
    api: &str,
    repository: &str,
    login: &str,
    draft: &serde_json::Value,
) -> Result<serde_json::Value> {
    use serde_json::{Value, json};
    let response = client.post(format!("{api}repos/{repository}/issues"))
        .header("Accept","application/vnd.github+json").bearer_auth(token)
        .json(&json!({"title":draft["title"],"body":draft["description"],"type":"Epic","assignees":[login]})).send().await?;
    let created: Value = checked_json(response).await?;
    let id = created["number"]
        .as_u64()
        .filter(|id| *id > 0)
        .context("Created issue number unavailable.")?;
    let url = crate::tools::todo::validate_link(created["html_url"].as_str().unwrap_or_default())?;
    ensure!(
        url.as_str() == format!("https://github.com/{repository}/issues/{id}"),
        "Invalid created issue URL."
    );
    let epic = created["type"]["name"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("Epic"));
    let assigned = created["assignees"]
        .as_array()
        .is_some_and(|a| a.iter().any(|u| u["login"] == login));
    Ok(
        json!({"id":id,"title":draft["title"],"url":url.as_str(),"kind":created["type"]["name"],"project":repository,"state":if epic && assigned {"created"} else {"needs_attention"}}),
    )
}
use regex::Regex;
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

const TOKEN_NAME: &str = "GITHUB_OAUTH_TOKENS";
const PROFILE: &str = "default";

#[derive(Clone)]
pub struct Pending {
    pub client_id: String,
    pub device_code: String,
    pub interval: u64,
    pub deadline: i64,
}

#[derive(Serialize)]
pub struct DevicePrompt {
    pub user_code: String,
    pub verification_uri: String,
}

#[derive(Serialize, Deserialize)]
struct Tokens {
    client_id: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: i64,
    interval: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct InstallationList {
    installations: Vec<Installation>,
}

#[derive(Deserialize)]
struct Installation {
    id: u64,
}

#[derive(Deserialize)]
struct RepositoryList {
    repositories: Vec<Repository>,
}

#[derive(Deserialize, Serialize)]
pub struct Repository {
    pub full_name: String,
    pub private: bool,
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("personal-teams-assistant-desktop")
        .build()?)
}

async fn checked_json<T: for<'a> Deserialize<'a>>(response: Response) -> Result<T> {
    ensure!(response.status().is_success(), "GitHub request failed");
    ensure!(
        response.content_length().unwrap_or(0) <= 2_000_000,
        "GitHub response too large"
    );
    let bytes = response.bytes().await?;
    ensure!(bytes.len() <= 2_000_000, "GitHub response too large");
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn begin(client_id: String) -> Result<(Pending, DevicePrompt)> {
    ensure!(
        (10..=100).contains(&client_id.len())
            && client_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c)),
        "invalid GitHub App client ID"
    );
    let response = client()?
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .form(&[("client_id", client_id.as_str())])
        .send()
        .await?;
    let code: DeviceResponse = checked_json(response).await?;
    ensure!(
        code.verification_uri == "https://github.com/login/device",
        "unexpected GitHub verification URL"
    );
    ensure!(
        (5..=120).contains(&code.interval) && (60..=1800).contains(&code.expires_in),
        "invalid GitHub device response"
    );
    Ok((
        Pending {
            client_id,
            device_code: code.device_code,
            interval: code.interval,
            deadline: chrono::Utc::now().timestamp() + code.expires_in,
        },
        DevicePrompt {
            user_code: code.user_code,
            verification_uri: code.verification_uri,
        },
    ))
}

fn save(tokens: &Tokens) -> Result<()> {
    security::put_desktop_secret(PROFILE, TOKEN_NAME, &serde_json::to_string(tokens)?)
}

pub async fn finish(pending: Pending) -> Result<()> {
    let client = client()?;
    let mut interval = pending.interval;
    loop {
        ensure!(
            chrono::Utc::now().timestamp() < pending.deadline,
            "GitHub authorization expired"
        );
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let response = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", pending.client_id.as_str()),
                ("device_code", pending.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?;
        let token: TokenResponse = checked_json(response).await?;
        if let Some(access_token) = token.access_token {
            ensure!(access_token.len() >= 20, "invalid GitHub token");
            save(&Tokens {
                client_id: pending.client_id,
                access_token,
                refresh_token: token.refresh_token,
                expires_at: token
                    .expires_in
                    .map(|secs| chrono::Utc::now().timestamp() + secs),
            })?;
            return Ok(());
        }
        match token.error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval = interval.saturating_add(5).min(120),
            _ => anyhow::bail!("GitHub authorization failed"),
        }
    }
}

pub(super) async fn access_token() -> Result<String> {
    let mut tokens: Tokens = serde_json::from_str(&security::secret(TOKEN_NAME)?)?;
    if tokens
        .expires_at
        .is_some_and(|t| t <= chrono::Utc::now().timestamp() + 120)
    {
        let refresh = tokens
            .refresh_token
            .as_deref()
            .context("GitHub login required")?;
        let response = client()?
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", tokens.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
            ])
            .send()
            .await?;
        let refreshed: TokenResponse = checked_json(response).await?;
        let new_token = refreshed.access_token.context("GitHub login required")?;
        tokens.access_token = new_token;
        tokens.refresh_token = refreshed.refresh_token;
        tokens.expires_at = refreshed
            .expires_in
            .map(|secs| chrono::Utc::now().timestamp() + secs);
        save(&tokens)?;
    }
    Ok(tokens.access_token)
}

async fn get<T: for<'a> Deserialize<'a>>(client: &Client, token: &str, url: &str) -> Result<T> {
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await?;
    checked_json(response).await
}

/// Issues of type Epic assigned to the connected GitHub user, newest first.
pub(super) async fn epics(
    filters: &crate::tools::todo::Filters,
) -> Result<crate::tools::todo::List> {
    filters.validate()?;
    epics_with(
        &client()?,
        &access_token().await?,
        "https://api.github.com/",
        filters,
    )
    .await
}

async fn epics_with(
    client: &Client,
    token: &str,
    base: &str,
    filters: &crate::tools::todo::Filters,
) -> Result<crate::tools::todo::List> {
    issues_with(client, token, base, filters, true).await
}
pub(super) async fn issues(
    filters: &crate::tools::todo::Filters,
) -> Result<crate::tools::todo::List> {
    filters.validate()?;
    issues_with(
        &client()?,
        &access_token().await?,
        "https://api.github.com/",
        filters,
        false,
    )
    .await
}
async fn issues_with(
    client: &Client,
    token: &str,
    base: &str,
    filters: &crate::tools::todo::Filters,
    epics_only: bool,
) -> Result<crate::tools::todo::List> {
    use crate::tools::todo::{Date, Item, List, validate_link};
    use serde_json::Value;
    ensure!(
        filters.source.is_none() && filters.kind.is_none(),
        "invalid GitHub source or kind filter"
    );
    ensure!(
        filters
            .state
            .as_deref()
            .is_none_or(|state| ["open", "closed"].contains(&state)),
        "invalid GitHub state: use open or closed"
    );
    if let Some(repo) = &filters.project {
        crate::tools::todo::validate_repository(repo)?;
    }
    let user: Value = get(client, token, &format!("{base}user")).await?;
    let login = user["login"]
        .as_str()
        .context("GitHub account unavailable")?;
    ensure!(
        !login.is_empty() && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "invalid GitHub login"
    );
    let mut query = format!("is:issue assignee:{login}");
    if epics_only {
        query.push_str(" type:\"Epic\"");
    }
    if let Some(state) = &filters.state {
        query.push_str(&format!(" state:{state}"));
    }
    if let Some(repo) = &filters.project {
        query.push_str(&format!(" repo:{repo}"));
    }
    let mut url = url::Url::parse(&format!("{base}search/issues"))?;
    url.query_pairs_mut()
        .append_pair("q", &query)
        .append_pair("sort", "created")
        .append_pair("order", "desc")
        .append_pair("per_page", &filters.limit.to_string());
    let found: Value = get(client, token, url.as_str()).await?;
    let mut result = List {
        states: vec!["open".into(), "closed".into()],
        assignment_fields: vec!["Assignees".into()],
        ..Default::default()
    };
    if found["incomplete_results"] == true {
        result.warn(
            "GitHub returned incomplete search results; refresh or narrow the repository filter."
                .into(),
        );
    }
    let items = found["items"]
        .as_array()
        .context("invalid GitHub Epic search")?;
    result.has_more = found["total_count"]
        .as_u64()
        .is_some_and(|total| total > items.len() as u64);
    for issue in items {
        if issue.get("pull_request").is_some() {
            continue;
        }
        if !issue["assignees"].as_array().is_some_and(|a| {
            a.iter().any(|u| {
                u["login"]
                    .as_str()
                    .is_some_and(|u| u.eq_ignore_ascii_case(login))
            })
        }) {
            continue;
        }
        let id = issue["number"].as_u64().context("invalid Epic number")?;
        let raw = issue["html_url"].as_str().context("invalid Epic link")?;
        let link = validate_link(raw)?;
        ensure!(link.host_str() == Some("github.com"), "invalid Epic link");
        let parts: Vec<_> = link
            .path_segments()
            .context("invalid Epic repository")?
            .collect();
        ensure!(parts[3] == id.to_string(), "invalid Epic link number");
        let repo = format!("{}/{}", parts[0], parts[1]);
        if filters
            .project
            .as_ref()
            .is_some_and(|p| !p.eq_ignore_ascii_case(&repo))
        {
            continue;
        }
        // Some search versions omit the issue type; verify it through the issue endpoint then.
        let detail;
        let issue = if issue["type"]["name"].is_string() {
            issue
        } else {
            detail =
                get::<Value>(client, token, &format!("{base}repos/{repo}/issues/{id}")).await?;
            &detail
        };
        if epics_only
            && !issue["type"]["name"]
                .as_str()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("Epic"))
        {
            continue;
        }
        let state = issue["state"].as_str().context("invalid Epic state")?;
        if filters.state.as_ref().is_some_and(|s| s != state) {
            continue;
        }
        result.projects.push(repo.clone());
        result.items.push(Item {
            id,
            title: issue["title"]
                .as_str()
                .context("invalid Epic title")?
                .into(),
            url: raw.into(),
            project: repo,
            state: state.into(),
            kind: issue["type"]["name"].as_str().unwrap_or("Issue").into(),
            created_at: issue["created_at"].as_str().map(str::to_owned),
            dates: vec![
                Date {
                    field: "start_date".into(),
                    name: "Start date".into(),
                    value: None,
                },
                Date {
                    field: "closed_at".into(),
                    name: "Completion date".into(),
                    value: issue["closed_at"].as_str().map(str::to_owned),
                },
            ],
        });
    }
    result.finish(filters.limit);
    Ok(result)
}

pub async fn repositories() -> Result<Vec<Repository>> {
    let token = access_token().await?;
    let client = client()?;
    let mut repos = Vec::new();
    for page in 1..=20 {
        let url = format!("https://api.github.com/user/installations?per_page=100&page={page}");
        let installations: InstallationList = get(&client, &token, &url).await?;
        let empty = installations.installations.is_empty();
        for installation in installations.installations {
            for page in 1..=20 {
                let url = format!(
                    "https://api.github.com/user/installations/{}/repositories?per_page=100&page={page}",
                    installation.id
                );
                let found: RepositoryList = get(&client, &token, &url).await?;
                let end = found.repositories.len() < 100;
                repos.extend(found.repositories);
                if end {
                    break;
                }
            }
        }
        if empty {
            break;
        }
    }
    repos.sort_by(|a, b| a.full_name.cmp(&b.full_name));
    repos.dedup_by(|a, b| a.full_name == b.full_name);
    Ok(repos)
}

pub async fn clone_repository(
    full_name: &str,
    alias: &str,
    data_dir: &Path,
) -> Result<std::path::PathBuf> {
    ensure!(
        Regex::new(r"^[a-z][a-z0-9_-]{0,63}$")?.is_match(alias),
        "invalid repository alias"
    );
    ensure!(
        Regex::new(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")?.is_match(full_name),
        "invalid GitHub repository"
    );
    ensure!(
        repositories()
            .await?
            .iter()
            .any(|repo| repo.full_name == full_name),
        "repository not authorized by GitHub App"
    );
    let token = access_token().await?;
    let root = data_dir.join("repositories");
    security::private_dir(&root)?;
    let destination = root.join(alias);
    ensure!(!destination.exists(), "repository alias already exists");
    let temporary = root.join(format!(".clone-{}", uuid::Uuid::new_v4()));
    let url = format!("https://github.com/{full_name}.git");
    let result = tokio::task::spawn_blocking({
        let temporary = temporary.clone();
        let url = url.clone();
        move || -> Result<()> {
            let exe = std::env::current_exe()?;
            let status = git_status(
                Command::new("git")
                    .args(["clone", "--single-branch", "--", &url])
                    .arg(&temporary)
                    .env("GIT_ASKPASS", exe)
                    .env("GIT_TERMINAL_PROMPT", "0")
                    .env("PERSONAL_TEAMS_GIT_ASKPASS", "1")
                    .env("PERSONAL_TEAMS_GIT_TOKEN", token)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env(
                        "GIT_CONFIG_GLOBAL",
                        if cfg!(windows) { "NUL" } else { "/dev/null" },
                    )
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )?;
            ensure!(status.success(), "Git clone failed");
            Ok(())
        }
    })
    .await?;
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temporary);
        return result.map(|_| destination);
    }
    std::fs::rename(&temporary, &destination)?;
    Ok(destination)
}

pub async fn update_repository(alias: &str, path: &Path, data_dir: &Path) -> Result<()> {
    ensure!(
        Regex::new(r"^[a-z][a-z0-9_-]{0,63}$")?.is_match(alias),
        "invalid repository alias"
    );
    let root = data_dir.join("repositories");
    ensure!(
        std::fs::canonicalize(path)? == std::fs::canonicalize(root.join(alias))?,
        "repository is not managed by this app"
    );
    let path = path.to_path_buf();
    let remote = tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            Command::new("git")
                .arg("-C")
                .arg(path)
                .args(["remote", "get-url", "origin"])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
        }
    })
    .await??;
    ensure!(remote.status.success(), "cannot read Git origin");
    let remote = String::from_utf8(remote.stdout)?.trim().to_owned();
    let full_name = remote
        .strip_prefix("https://github.com/")
        .and_then(|name| name.strip_suffix(".git"))
        .context("Git origin is not GitHub HTTPS")?;
    ensure!(
        repositories()
            .await?
            .iter()
            .any(|repo| repo.full_name == full_name),
        "GitHub App no longer has repository access"
    );
    let token = access_token().await?;
    tokio::task::spawn_blocking(move || -> Result<()> {
        let exe = std::env::current_exe()?;
        let clean = Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(["status", "--porcelain"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()?;
        ensure!(
            clean.status.success() && clean.stdout.is_empty(),
            "Git checkout has local changes"
        );
        let status = git_status(
            Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(["fetch", "origin"])
                .env("GIT_ASKPASS", exe)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("PERSONAL_TEAMS_GIT_ASKPASS", "1")
                .env("PERSONAL_TEAMS_GIT_TOKEN", token)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env(
                    "GIT_CONFIG_GLOBAL",
                    if cfg!(windows) { "NUL" } else { "/dev/null" },
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )?;
        ensure!(status.success(), "Git fetch failed");
        let status = git_status(
            Command::new("git")
                .arg("-C")
                .arg(&path)
                .args([
                    "-c",
                    if cfg!(windows) {
                        "core.hooksPath=NUL"
                    } else {
                        "core.hooksPath=/dev/null"
                    },
                    "merge",
                    "--ff-only",
                    "FETCH_HEAD",
                ])
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env(
                    "GIT_CONFIG_GLOBAL",
                    if cfg!(windows) { "NUL" } else { "/dev/null" },
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )?;
        ensure!(status.success(), "Git checkout diverged");
        Ok(())
    })
    .await??;
    Ok(())
}

pub fn askpass() {
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if prompt.contains("username") {
        print!("x-access-token");
    } else if prompt.contains("password") {
        print!(
            "{}",
            std::env::var("PERSONAL_TEAMS_GIT_TOKEN").unwrap_or_default()
        );
    }
}

fn git_status(command: &mut Command) -> Result<std::process::ExitStatus> {
    let mut child = command.spawn()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Git operation timed out");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod todo_tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn requests_send_epic_type_once_and_keep_links_when_type_is_dropped() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/repos/example/work/issues")).respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"number":77,"html_url":"https://github.com/example/work/issues/77","type":null,"assignees":[{"login":"example-user"}]}))).expect(1).mount(&server).await;
        let created = create_request_at(
            &Client::new(),
            "synthetic-token",
            &format!("{}/", server.uri()),
            "example/work",
            "example-user",
            &serde_json::json!({"title":"Receipts","description":"Support downloads"}),
        )
        .await
        .unwrap();
        assert_eq!(created["state"], "needs_attention");
        assert_eq!(created["url"], "https://github.com/example/work/issues/77");
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["type"], "Epic");
        assert_eq!(body["assignees"][0], "example-user");
        assert_eq!(body["body"], "Support downloads");
    }
    #[tokio::test]
    async fn epics_use_issue_type_own_assignment_and_server_side_filters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login":"example-user"})))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/search/issues")).respond_with(|request: &wiremock::Request| {
            let params: std::collections::BTreeMap<_,_> = request.url.query_pairs().collect();
            assert_eq!(params["q"], "is:issue assignee:example-user type:\"Epic\" state:closed repo:example/work");
            assert_eq!(params["sort"], "created");
            assert_eq!(params["order"], "desc");
            assert_eq!(params["per_page"], "10");
            let issue = |number, kind, user| json!({"number":number,"title":"Plan the release","type":{"name":kind},"state":"closed","created_at":"2026-10-01T12:00:00Z","closed_at":"2026-10-04T12:00:00Z","html_url":format!("https://github.com/example/work/issues/{number}"),"assignees":[{"login":user}]});
            ResponseTemplate::new(200).set_body_json(json!({"total_count":12,"incomplete_results":false,"items":[issue(1,"Epic","example-user"),issue(2,"Task","example-user"),issue(3,"Epic","someone-else")]}))
        }).mount(&server).await;
        let filters: crate::tools::todo::Filters =
            serde_json::from_value(json!({"state":"closed","project":"example/work"})).unwrap();
        let result = epics_with(
            &client().unwrap(),
            "test-token",
            &format!("{}/", server.uri()),
            &filters,
        )
        .await
        .unwrap();
        assert_eq!(result.items.len(), 1);
        assert!(result.has_more);
        assert_eq!(result.states, ["closed", "open"]);
        assert_eq!(result.items[0].dates[0].value, None);
        assert_eq!(
            result.items[0].dates[1].value.as_deref(),
            Some("2026-10-04T12:00:00Z")
        );
        let invalid: crate::tools::todo::Filters =
            serde_json::from_value(json!({"state":"closed OR assignee:someone-else"})).unwrap();
        assert!(
            epics_with(
                &client().unwrap(),
                "test-token",
                &format!("{}/", server.uri()),
                &invalid
            )
            .await
            .is_err()
        );
    }
}
