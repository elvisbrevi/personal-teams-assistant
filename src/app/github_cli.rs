//! Explicit request destinations use the GitHub CLI account, independently of the GitHub App.
use super::*;
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};

const LOGIN: &str =
    "Sign in with gh auth login on the machine running this app, then refresh GitHub CLI status.";

fn command(binary: &Path, profile: Option<&Path>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.env_clear().envs(crate::llm::child_env(binary));
    if let Some(profile) = profile {
        let home = profile.join("home");
        cmd.env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GH_CONFIG_DIR", home.join(".config/gh"));
    } else {
        for name in ["GH_CONFIG_DIR", "GH_TOKEN", "GITHUB_TOKEN"] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
    }
    cmd.env("GH_HOST", "github.com")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    cmd
}
async fn read(binary: &Path, profile: Option<&Path>, args: &[&str]) -> Result<Vec<u8>> {
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        command(binary, profile).args(args).output(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .context(LOGIN)?;
    ensure!(output.status.success(), "{LOGIN}");
    ensure!(
        output.stdout.len() <= 16_000_000,
        "GitHub CLI response too large."
    );
    Ok(output.stdout)
}
async fn run(args: &[&str]) -> Result<Vec<u8>> {
    let binary =
        crate::llm::locate("gh").context("Install GitHub CLI on the machine running this app.")?;
    let profile = control::isolated_profile_dir()?;
    read(&binary, profile.as_deref(), args).await
}
pub(super) async fn status() -> Value {
    let Some(binary) = crate::llm::locate("gh") else {
        return json!({"state":"not_installed","authenticated":false,"user":null});
    };
    let Ok(profile) = control::isolated_profile_dir() else {
        return json!({"state":"unavailable","authenticated":false,"user":null});
    };
    let user = async {
        let bytes = read(
            &binary,
            profile.as_deref(),
            &["api", "user", "--hostname", "github.com"],
        )
        .await?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let login = value["login"]
            .as_str()
            .context("GitHub identity unavailable.")?;
        ensure!(
            !login.is_empty()
                && login.len() <= 100
                && login
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "Invalid GitHub identity."
        );
        Ok::<_, anyhow::Error>(login.to_owned())
    }
    .await;
    match user {
        Ok(user) => json!({"state":"authenticated","authenticated":true,"user":user}),
        Err(_) => json!({"state":"not_authenticated","authenticated":false,"user":null}),
    }
}
pub(super) async fn repositories() -> Result<Vec<Value>> {
    let bytes = run(&[
        "repo",
        "list",
        "--limit",
        "100000",
        "--json",
        "nameWithOwner,isPrivate",
    ])
    .await?;
    repository_targets(&bytes)
}
fn repository_targets(bytes: &[u8]) -> Result<Vec<Value>> {
    let repos: Vec<Value> =
        serde_json::from_slice(bytes).context("Invalid GitHub CLI repository list.")?;
    ensure!(
        repos.len() < 100_000,
        "GitHub repository coverage incomplete."
    );
    let mut targets = Vec::new();
    for repo in repos {
        let name = repo["nameWithOwner"]
            .as_str()
            .context("Invalid GitHub repository.")?;
        crate::tools::todo::validate_repository(name)?;
        targets.push(json!({"provider":"github","project":name}));
    }
    targets.sort_by(|a, b| a["project"].as_str().cmp(&b["project"].as_str()));
    targets.dedup();
    Ok(targets)
}
pub(super) async fn token() -> Result<String> {
    let bytes = run(&["auth", "token", "--hostname", "github.com"]).await?;
    let token = std::str::from_utf8(&bytes)
        .context("Invalid GitHub CLI session.")?
        .trim();
    ensure!(
        !token.is_empty() && token.len() <= 32_000 && token.bytes().all(|c| c.is_ascii_graphic()),
        "Invalid GitHub CLI session."
    );
    Ok(token.to_owned())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn repository_catalog_keeps_public_and_private_repos_and_rejects_invalid_names() {
        let targets = repository_targets(br#"[{"nameWithOwner":"example/public","isPrivate":false},{"nameWithOwner":"example/private","isPrivate":true},{"nameWithOwner":"example/public","isPrivate":false}]"#).unwrap();
        assert_eq!(
            targets,
            vec![
                json!({"provider":"github","project":"example/private"}),
                json!({"provider":"github","project":"example/public"})
            ]
        );
        assert!(
            repository_targets(br#"[{"nameWithOwner":"https://other.example/repo"}]"#).is_err()
        );
    }
    #[tokio::test]
    async fn cli_reads_are_profile_confined_and_fail_without_exposing_output() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("gh");
        std::fs::write(&binary, "#!/bin/sh\n[ \"$GH_HOST\" = github.com ] || exit 1\n[ \"$GH_CONFIG_DIR\" = \"$HOME/.config/gh\" ] || exit 2\n[ -z \"$GH_TOKEN$GITHUB_TOKEN\" ] || exit 3\nif [ \"$1\" = api ]; then printf '{\"login\":\"example\"}'; else printf 'synthetic-secret'; exit 1; fi\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let bytes = read(&binary, Some(dir.path()), &["api", "user"])
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["login"],
            "example"
        );
        let error = read(&binary, Some(dir.path()), &["auth", "token"])
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("synthetic-secret"));
    }
}
