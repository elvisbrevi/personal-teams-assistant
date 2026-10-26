//! Azure CLI owns the user's Entra login; tokens stay in memory and never enter model sessions.
use super::*;
use serde_json::Value;
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
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
    pub task: tokio::task::JoinHandle<std::result::Result<(), LoginError>>,
}

#[derive(Debug, Serialize)]
pub(super) struct LoginError {
    stage: &'static str,
    reason: &'static str,
    elapsed_ms: u64,
    stderr_bytes: usize,
    exit_code: Option<i32>,
    io_kind: Option<String>,
    provider_codes: Vec<String>,
    transport: Option<&'static str>,
}
impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.stage == "verify_session" {
            LOGIN
        } else {
            LOGIN_FAILED
        })
    }
}
impl std::error::Error for LoginError {}
impl LoginError {
    fn new(
        stage: &'static str,
        reason: &'static str,
        started: Instant,
        stderr_bytes: usize,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            stage,
            reason,
            elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            stderr_bytes,
            exit_code,
            io_kind: None,
            provider_codes: Vec::new(),
            transport: None,
        }
    }
    fn output(mut self, bytes: &[u8]) -> Self {
        let text = String::from_utf8_lossy(bytes);
        self.provider_codes = regex::Regex::new(r"\bAADSTS[0-9]{4,10}\b")
            .unwrap()
            .find_iter(&text)
            .take(8)
            .map(|code| code.as_str().to_owned())
            .collect();
        self.transport = if text.contains("SSLError") || text.contains("CERTIFICATE_VERIFY_FAILED")
        {
            Some("tls")
        } else if text.contains("ConnectTimeout") || text.contains("ReadTimeout") {
            Some("timeout")
        } else if text.contains("ConnectionError") {
            Some("connection")
        } else {
            None
        };
        self
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// Only the known public verification URL and a bounded user code may leave the host.
fn prompt(text: &str) -> Option<Prompt> {
    // Both Microsoft pages reach the same verification flow. Keep the public IPC URL stable.
    let rest = [DEVICE_URL, "https://login.microsoft.com/device"]
        .iter()
        .find_map(|url| text.split_once(url).map(|(_, rest)| rest))?;
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
    let started = Instant::now();
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
    let mut child = cmd.spawn().map_err(|error| {
        let mut failure = LoginError::new("start", "spawn_failed", started, 0, None);
        failure.io_kind = Some(format!("{:?}", error.kind()));
        failure
    })?;
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        while bytes.len() < 16_384 {
            let byte = stderr.read_u8().await.map_err(|error| {
                let mut failure = LoginError::new(
                    "read_device_prompt",
                    "read_failed",
                    started,
                    bytes.len(),
                    None,
                );
                failure.io_kind = Some(format!("{:?}", error.kind()));
                failure
            })?;
            bytes.push(byte);
            if byte == b'\n' {
                let text = String::from_utf8_lossy(&bytes);
                if let Some(prompt) = prompt(&text) {
                    return Ok(prompt);
                }
                if text.contains("enter the code ") && text.contains(" to authenticate") {
                    return Err(LoginError::new(
                        "read_device_prompt",
                        "unrecognized_prompt",
                        started,
                        bytes.len(),
                        None,
                    ));
                }
            }
        }
        Err(LoginError::new(
            "read_device_prompt",
            "output_limit",
            started,
            bytes.len(),
            None,
        ))
    })
    .await;
    let prompt = match result.unwrap_or_else(|_| {
        Err(LoginError::new(
            "read_device_prompt",
            "timeout",
            started,
            bytes.len(),
            None,
        ))
    }) {
        Ok(prompt) => prompt,
        Err(mut error) => {
            let exit = child
                .try_wait()
                .ok()
                .flatten()
                .and_then(|status| status.code());
            error.exit_code = exit;
            return Err(error.output(&bytes).into());
        }
    };
    let task = tokio::spawn(async move {
        // Inspect bounded output for allowlisted error codes; never retain raw provider text in logs.
        let mut tail = Vec::new();
        let mut total_bytes = bytes.len();
        let result = tokio::time::timeout(Duration::from_secs(600), async {
            let (status, _) = tokio::join!(child.wait(), async {
                total_bytes += (&mut stderr).take(16_384).read_to_end(&mut tail).await?;
                let mut sink = tokio::io::sink();
                total_bytes = total_bytes.saturating_add(
                    tokio::io::copy(&mut stderr, &mut sink)
                        .await?
                        .min(usize::MAX as u64) as usize,
                );
                Ok::<_, std::io::Error>(())
            });
            status
        })
        .await;
        match result {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(status)) => Err(LoginError::new(
                "wait_for_login",
                "process_failed",
                started,
                total_bytes,
                status.code(),
            )
            .output(&tail)),
            Ok(Err(error)) => {
                let mut failure =
                    LoginError::new("wait_for_login", "wait_failed", started, total_bytes, None)
                        .output(&tail);
                failure.io_kind = Some(format!("{:?}", error.kind()));
                Err(failure)
            }
            Err(_) => Err(
                LoginError::new("wait_for_login", "expired", started, total_bytes, None)
                    .output(&tail),
            ),
        }
    });
    Ok(Pending { prompt, task })
}

pub(super) async fn begin(tenant: &str) -> Result<Pending> {
    let binary = binary().ok_or(SetupError(
        "Install Azure CLI on the machine running this app.",
    ))?;
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
    let started = Instant::now();
    session().await.map_err(|status| {
        if status.state == "not_installed" {
            anyhow::Error::new(SetupError(
                "Install Azure CLI on the machine running this app.",
            ))
        } else {
            anyhow::Error::new(LoginError::new(
                "verify_session",
                status.state,
                started,
                0,
                None,
            ))
        }
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn provider_diagnostics_keep_codes_and_transport_without_provider_text() {
        let error = LoginError::new("read_device_prompt", "read_failed", Instant::now(), 150, Some(1))
            .output(b"AADSTS53003 CERTIFICATE_VERIFY_FAILED token=synthetic-private-error code ABCD12345 https://example.test/?code=secret");
        assert_eq!(error.provider_codes, ["AADSTS53003"]);
        assert_eq!(error.transport, Some("tls"));
        let saved = serde_json::to_string(&error).unwrap();
        for private in [
            "synthetic-private-error",
            "ABCD12345",
            "example.test",
            "secret",
        ] {
            assert!(!saved.contains(private));
        }
    }
    #[test]
    fn login_prompt_only_accepts_the_microsoft_device_page_and_user_code() {
        let text = "To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ABCD12345 to authenticate.";
        assert_eq!(prompt(text).unwrap().user_code, "ABCD12345");
        let current = text.replace(DEVICE_URL, "https://login.microsoft.com/device");
        assert_eq!(prompt(&current).unwrap().user_code, "ABCD12345");
        assert_eq!(prompt(&current).unwrap().verification_uri, DEVICE_URL);
        for text in [
            text.replace("microsoft.com", "evil.example"),
            text.replace("devicelogin", "devicelogin.evil"),
            text.replace(DEVICE_URL, "https://login.microsoft.com/device.evil"),
            text.replace(DEVICE_URL, "https://login.microsoft.com.evil/device"),
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
            "#!/bin/sh\n[ \"$AZURE_CONFIG_DIR\" = '{}' ] || exit 9\n[ \"$AZURE_CORE_LOGIN_EXPERIENCE_V2\" = off ] || exit 9\n[ -z \"${{GRAPH_WEBHOOK_SECRET+x}}\" ] || exit 9\n[ -z \"${{AZURE_DEVOPS_EXT_PAT+x}}\" ] || exit 9\n[ \"$*\" = 'login --use-device-code --allow-no-subscriptions --scope {RESOURCE}/.default --output none --tenant example.test' ] || exit 9\nprintf '%s\\n' 'To sign in, use a web browser to open the page https://login.microsoft.com/device and enter the code ABCD12345 to authenticate.' >&2\nexit 0\n",
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
            script.replace(
                "https://login.microsoft.com/device",
                "https://untrusted.example/device",
            ),
        )
        .unwrap();
        let error = begin_at(&binary, Some(&profile), "example.test")
            .await
            .err()
            .unwrap();
        let diagnostic = error.downcast_ref::<LoginError>().unwrap();
        assert_eq!(diagnostic.stage, "read_device_prompt");
        assert_eq!(diagnostic.reason, "unrecognized_prompt");
        let saved = serde_json::to_string(diagnostic).unwrap();
        assert!(saved.contains("stderr_bytes"));
        assert!(!saved.contains("ABCD12345"));
        assert!(!saved.contains("untrusted.example"));
        assert!(fail(error).starts_with("[not_ready] Azure CLI sign-in failed"));
        fs::write(
            &binary,
            script.replace(
                "exit 0",
                "echo 'AADSTS53003 token=synthetic-private-error' >&2\nexit 23",
            ),
        )
        .unwrap();
        let mut pending = begin_at(&binary, Some(&profile), "example.test")
            .await
            .unwrap();
        let error = (&mut pending.task).await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), LOGIN_FAILED);
        assert_eq!(error.exit_code, Some(23));
        assert_eq!(error.provider_codes, ["AADSTS53003"]);
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("synthetic-private-error")
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
