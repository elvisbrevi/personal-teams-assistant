use anyhow::{Context, Result, ensure};
use personal_teams_assistant::security;
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

async fn access_token() -> Result<String> {
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
