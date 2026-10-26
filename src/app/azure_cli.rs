//! Azure CLI owns the user's Entra login; tokens stay in memory and never enter model sessions.
use super::*;
use serde_json::Value;
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, BufReader};

const RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";
const LOGIN: &str = "Select Azure CLI on Home to sign in with Microsoft in your browser.";
const DEVICE_URL: &str = "https://microsoft.com/devicelogin";
const LOGIN_FAILED: &str = "Azure CLI sign-in failed or expired. Select Azure CLI to try again.";

#[derive(Clone, Serialize)]
pub(super) struct Prompt {
    verification_uri: &'static str,
    user_code: String,
}
pub(super) struct Pending {
    pub prompt: Prompt,
    pub task: tokio::task::JoinHandle<std::result::Result<(), String>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// Only the known public verification URL and a bounded user code may leave the host.
fn prompt(text: &str) -> Option<Prompt> {
    let (_, rest) = text.split_once(DEVICE_URL)?;
    let rest = rest.strip_prefix(" and enter the code ")?;
    let (code, _) = rest.split_once(" to authenticate")?;
    (code.len() >= 6
        && code.len() <= 16
        && code
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-'))
    .then(|| Prompt {
        verification_uri: DEVICE_URL,
        user_code: code.into(),
    })
}

async fn begin_at(binary: &Path, profile: Option<&Path>, tenant: &str) -> Result<Pending> {
    let mut cmd = command(binary, profile);
    cmd.args([
        "login",
        "--use-device-code",
        "--allow-no-subscriptions",
        "--scope",
        "499b84ac-1321-427f-aa17-267ca6975798/.default",
        "--output",
        "none",
    ])
    .env("AZURE_CORE_LOGIN_EXPERIENCE_V2", "off")
    .env("AZURE_CORE_ENABLE_BROKER_ON_WINDOWS", "false")
    .env("AZURE_CORE_ENABLE_BROKER_ON_MAC", "false")
    .env("AZURE_CORE_NO_COLOR", "true")
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    if !tenant.is_empty() && tenant != "common" && tenant != "organizations" {
        cmd.args(["--tenant", tenant]);
    }
    let mut child = cmd.spawn().map_err(|_| anyhow::anyhow!(LOGIN_FAILED))?;
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let prompt = tokio::time::timeout(Duration::from_secs(30), async {
        let mut bytes = Vec::new();
        while bytes.len() < 16_384 {
            let byte = stderr.read_u8().await.ok()?;
            bytes.push(byte);
            if byte == b'\n'
                && let Some(prompt) = prompt(&String::from_utf8_lossy(&bytes))
            {
                return Some(prompt);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
    .ok_or_else(|| anyhow::anyhow!(LOGIN_FAILED))?;
    let task = tokio::spawn(async move {
        // Drain diagnostics without retaining, returning or logging provider output.
        let result = tokio::time::timeout(Duration::from_secs(600), async {
            let mut sink = tokio::io::sink();
            let (status, _) = tokio::join!(child.wait(), tokio::io::copy(&mut stderr, &mut sink));
            status.is_ok_and(|status| status.success())
        })
        .await;
        if result == Ok(true) {
            Ok(())
        } else {
            Err(LOGIN_FAILED.into())
        }
    });
    Ok(Pending { prompt, task })
}

pub(super) async fn begin(tenant: &str) -> Result<Pending> {
    let binary = binary().context("Install Azure CLI on the machine running this app.")?;
    let profile = control::isolated_profile_dir()?;
    begin_at(&binary, profile.as_deref(), tenant).await
}

#[derive(Serialize)]
pub(super) struct Status {
    pub state: &'static str,
    pub authenticated: bool,
    pub user: Option<String>,
    pub tenant: Option<String>,
}
impl Status {
    fn new(state: &'static str) -> Self {
        Self {
            state,
            authenticated: false,
            user: None,
            tenant: None,
        }
    }
}

// Deliberately neither Debug nor Serialize: only Status can leave the host.
pub(super) struct Session {
    token: String,
    pub(super) status: Status,
}
impl Session {
    pub(super) fn token(&self) -> &str {
        &self.token
    }
}
fn binary() -> Option<PathBuf> {
    crate::llm::locate("az").or_else(|| {
        if !cfg!(windows) {
            return None;
        }
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("az.cmd"))
                .find(|p| p.is_file())
        })
    })
}
fn command(binary: &Path, profile: Option<&Path>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.env_clear().envs(crate::llm::child_env(binary));
    for name in ["REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE", "SystemRoot"] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    if let Some(profile) = profile {
        let home = profile.join("home");
        cmd.env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("AZURE_CONFIG_DIR", home.join(".azure"));
    } else if let Some(config) = std::env::var_os("AZURE_CONFIG_DIR") {
        cmd.env("AZURE_CONFIG_DIR", config);
    }
    cmd.env("AZURE_CORE_COLLECT_TELEMETRY", "no")
        .env("AZURE_EXTENSION_USE_DYNAMIC_INSTALL", "no")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    cmd
}
async fn read(binary: &Path, profile: Option<&Path>, args: &[&str]) -> Option<Value> {
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        command(binary, profile)
            .args(args)
            .args(["--output", "json", "--only-show-errors"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() || output.stdout.len() > 64_000 {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}
async fn load(binary: &Path, profile: Option<&Path>) -> std::result::Result<Session, Status> {
    let Some(account) = read(binary, profile, &["account", "show"]).await else {
        return Err(Status::new("not_authenticated"));
    };
    let mut status = Status::new("unavailable");
    status.user = account["user"]["name"]
        .as_str()
        .filter(|s| s.len() <= 320 && !s.chars().any(char::is_control))
        .map(str::to_owned);
    status.tenant = account["tenantId"]
        .as_str()
        .filter(|s| uuid::Uuid::parse_str(s).is_ok())
        .map(str::to_owned);
    let Some(value) = read(
        binary,
        profile,
        &["account", "get-access-token", "--resource", RESOURCE],
    )
    .await
    else {
        return Err(status);
    };
    let expires = value["expires_on"]
        .as_i64()
        .or_else(|| value["expires_on"].as_str()?.parse().ok())
        .unwrap_or(0);
    let token = value["accessToken"].as_str().unwrap_or_default();
    if status.tenant.as_deref() != value["tenant"].as_str()
        || status.tenant.is_none()
        || !value["tokenType"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("Bearer"))
        || expires <= chrono::Utc::now().timestamp() + 30
        || token.is_empty()
        || token.len() > 32_000
        || !token.bytes().all(|c| c.is_ascii_graphic())
    {
        return Err(status);
    }
    status.state = "authenticated";
    status.authenticated = true;
    Ok(Session {
        token: token.into(),
        status,
    })
}
async fn session() -> std::result::Result<Session, Status> {
    let Some(binary) = binary() else {
        return Err(Status::new("not_installed"));
    };
    let profile = control::isolated_profile_dir().map_err(|_| Status::new("unavailable"))?;
    load(&binary, profile.as_deref()).await
}
pub(super) async fn status() -> Status {
    match session().await {
        Ok(session) => session.status,
        Err(status) => status,
    }
}
pub(super) async fn require() -> Result<Session> {
    session().await.map_err(|status| {
        anyhow::anyhow!(if status.state == "not_installed" {
            "Install Azure CLI on the machine running this app."
        } else {
            LOGIN
        })
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn login_prompt_only_accepts_the_microsoft_device_page_and_user_code() {
        let text = "To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ABCD12345 to authenticate.";
        assert_eq!(prompt(text).unwrap().user_code, "ABCD12345");
        for text in [
            text.replace("microsoft.com", "evil.example"),
            text.replace("devicelogin", "devicelogin.evil"),
            text.replace("ABCD12345", "private token"),
            text.replace("ABCD12345", "ABC\n12345"),
        ] {
            assert!(prompt(&text).is_none());
        }
    }
    #[tokio::test]
    async fn device_login_uses_the_profile_and_can_complete_fail_or_be_cancelled() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("az");
        let profile = root.path().join("profile");
        fs::create_dir_all(profile.join("home/.azure")).unwrap();
        let script = format!(
            "#!/bin/sh\n[ \"$AZURE_CONFIG_DIR\" = '{}' ] || exit 9\n[ \"$AZURE_CORE_LOGIN_EXPERIENCE_V2\" = off ] || exit 9\n[ -z \"${{GRAPH_WEBHOOK_SECRET+x}}\" ] || exit 9\n[ -z \"${{AZURE_DEVOPS_EXT_PAT+x}}\" ] || exit 9\n[ \"$*\" = 'login --use-device-code --allow-no-subscriptions --scope {RESOURCE}/.default --output none --tenant example.test' ] || exit 9\nprintf '%s\\n' 'To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ABCD12345 to authenticate.' >&2\nexit 0\n",
            profile.join("home/.azure").display()
        );
        fs::write(&binary, &script).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let mut pending = begin_at(&binary, Some(&profile), "example.test")
            .await
            .unwrap();
        assert_eq!(pending.prompt.verification_uri, DEVICE_URL);
        (&mut pending.task).await.unwrap().unwrap();
        fs::write(
            &binary,
            script.replace("exit 0", "echo synthetic-private-error >&2\nexit 1"),
        )
        .unwrap();
        let mut pending = begin_at(&binary, Some(&profile), "example.test")
            .await
            .unwrap();
        assert_eq!(
            (&mut pending.task).await.unwrap().unwrap_err(),
            LOGIN_FAILED
        );
        fs::write(
            &binary,
            script.replace(
                "exit 0",
                "echo $$ > \"$AZURE_CONFIG_DIR/login.pid\"\nexec sleep 60",
            ),
        )
        .unwrap();
        let pending = begin_at(&binary, Some(&profile), "example.test")
            .await
            .unwrap();
        let pid_path = profile.join("home/.azure/login.pid");
        while !pid_path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = fs::read_to_string(pid_path).unwrap();
        drop(pending);
        tokio::time::timeout(Duration::from_secs(2), async {
            while std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancellation must kill the login child");
        fs::write(
            &binary,
            "#!/bin/sh\necho synthetic-private-error >&2\nexit 1\n",
        )
        .unwrap();
        assert_eq!(
            begin_at(&binary, Some(&profile), "example.test")
                .await
                .err()
                .unwrap()
                .to_string(),
            LOGIN_FAILED
        );
    }
    #[tokio::test]
    async fn cli_authentication_is_profile_confined_and_never_serializes_tokens() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("az");
        let profile = root.path().join("profile");
        let expected = profile.join("home/.azure");
        let script = format!(
            "#!/bin/sh\n[ -z \"${{GRAPH_WEBHOOK_SECRET+x}}\" ] || exit 9\n[ -z \"${{AZURE_DEVOPS_EXT_PAT+x}}\" ] || exit 9\n[ \"$AZURE_CONFIG_DIR\" = '{}' ] || exit 9\nif [ \"$2\" = show ]; then printf '%s' '{{\"user\":{{\"name\":\"person@example.test\"}},\"tenantId\":\"11111111-1111-4111-8111-111111111111\"}}'; else [ \"$4\" = '{}' ] || exit 9; printf '%s' '{{\"accessToken\":\"synthetic-private-entra-token\",\"tokenType\":\"Bearer\",\"expires_on\":4102444800,\"tenant\":\"11111111-1111-4111-8111-111111111111\"}}'; fi\n",
            expected.display(),
            RESOURCE
        );
        fs::write(&binary, &script).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let session = load(&binary, Some(&profile)).await.ok().unwrap();
        assert_eq!(session.token(), "synthetic-private-entra-token");
        assert!(session.status.authenticated);
        let safe = serde_json::to_string(&session.status).unwrap();
        assert!(safe.contains("person@example.test"));
        assert!(!safe.contains("synthetic-private-entra-token"));
        fs::write(&binary, script.replace("4102444800", "1")).unwrap();
        assert!(matches!(
            load(&binary, Some(&profile)).await,
            Err(Status {
                state: "unavailable",
                authenticated: false,
                ..
            })
        ));
        fs::write(&binary, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(matches!(
            load(&binary, Some(&profile)).await,
            Err(Status {
                state: "not_authenticated",
                ..
            })
        ));
    }
}
