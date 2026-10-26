//! Provider-owned remote connections. Pairing credentials never enter saved runs.
use super::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::Mutex,
};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Access {
    pub status: String,
    pub session_id: Option<String>,
    pub url: Option<String>,
    pub environment_id: Option<String>,
    pub server_name: Option<String>,
    pub expires_at: Option<i64>,
}
pub(crate) type Observer = Arc<dyn Fn(Access) -> Result<()> + Send + Sync>;
impl Access {
    pub(crate) fn unavailable() -> Self {
        Self {
            status: "unavailable".into(),
            ..Self::default()
        }
    }
}

pub(crate) fn claude_url(value: &str) -> Result<String> {
    let url = url::Url::parse(value)?;
    let id = url.path().strip_prefix("/code/").unwrap_or_default();
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("claude.ai")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && id.starts_with("session_")
            && (9..=200).contains(&id.len())
            && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'),
        "The provider did not return a supported remote session URL."
    );
    Ok(value.into())
}

async fn codex_command(binary: &std::path::Path, action: &str) -> Result<Value> {
    let directory = tempfile::tempdir()?;
    let output = tokio::time::timeout(
        Duration::from_secs(25),
        tokio::process::Command::new(binary)
            .args(["remote-control", action, "--json"])
            .current_dir(directory.path())
            .env_clear()
            .envs(agent::child_env(binary))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success() && output.stdout.len() <= 64_000,
        "Codex Remote Control could not connect. Update Codex and check its ChatGPT login and workspace access on the execution machine."
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}
pub(super) async fn codex_start(binary: &std::path::Path) -> Result<(Access, String)> {
    let value = codex_command(binary, "start").await?;
    ensure!(
        value["status"] == "connected" && value["environmentId"].as_str().is_some(),
        "Codex Remote Control is not connected. Check its ChatGPT login and workspace access on the execution machine."
    );
    let socket = value["daemon"]["socketPath"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Codex did not return its local control socket."))?
        .to_owned();
    Ok((
        Access {
            status: "connected".into(),
            environment_id: value["environmentId"].as_str().map(Into::into),
            server_name: value["serverName"]
                .as_str()
                .map(|s| s.chars().take(200).collect()),
            ..Access::default()
        },
        socket,
    ))
}
pub(crate) async fn pair_codex() -> Result<Value> {
    let binary = agent::locate("codex")
        .ok_or_else(|| anyhow::anyhow!("Codex is not installed on the execution machine."))?;
    let value = codex_command(&binary, "pair").await?;
    let code = value["manualPairingCode"].as_str().unwrap_or_default();
    ensure!(
        (4..=40).contains(&code.len())
            && code.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            && value["expiresAt"]
                .as_i64()
                .is_some_and(|t| t > chrono::Utc::now().timestamp()),
        "Codex did not return a valid, unexpired pairing code."
    );
    // Only the manual code reaches the authenticated caller, never the provider's pairing token.
    Ok(json!({"manual_code":code,"expires_at":value["expiresAt"]}))
}
pub(crate) async fn cancel_codex(id: &str) -> Result<()> {
    let binary = agent::locate("codex")
        .ok_or_else(|| anyhow::anyhow!("Codex is not installed on the execution machine."))?;
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        super::codex::Client::connect(&binary)
            .await?
            .cancel(id)
            .await
    })
    .await;
    ensure!(
        matches!(result, Ok(Ok(()))),
        "Codex interruption could not be confirmed. Inspect the session before starting another run."
    );
    Ok(())
}

struct Session {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    _tree: ProcessTree,
    stderr: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    remote_connected: bool,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stderr.abort();
    }
}
impl Session {
    async fn send(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        Ok(())
    }
    async fn next(&mut self) -> Result<Value> {
        loop {
            let mut line = Vec::new();
            let count = (&mut self.output)
                .take(256_001)
                .read_until(b'\n', &mut line)
                .await?;
            ensure!(
                count > 0 && count <= 256_000,
                "The remote agent connection ended or exceeded its output limit."
            );
            if let Ok(value) = serde_json::from_slice::<Value>(&line) {
                // The provider bridge forwards prompts to the signed-in person's app and
                // injects their response itself. The host never grants an approval.
                if value["type"] == "control_request" {
                    if !self.remote_connected {
                        self.send(json!({"type":"control_response","response":{"subtype":"error",
                            "request_id":value["request_id"],"error":"This host does not grant tool approvals."}})).await?;
                    }
                } else {
                    return Ok(value);
                }
            }
        }
    }
    async fn control(&mut self, id: &str, request: Value) -> Result<Value> {
        self.send(json!({"type":"control_request","request_id":id,"request":request}))
            .await?;
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let value = self.next().await?;
                if value["type"] == "control_response" && value["response"]["request_id"] == id {
                    ensure!(value["response"]["subtype"] == "success", "Claude Remote Control could not connect. Update Claude Code and check its subscription login and organization policy on the execution machine.");
                    return Ok(value["response"]["response"].clone());
                }
            }
        }).await?
    }
    async fn enable(&mut self) -> Result<Access> {
        self.control("pta-initialize", json!({"subtype":"initialize"}))
            .await?;
        let result = self
            .control(
                "pta-remote",
                json!({"subtype":"remote_control","enabled":true,"name":"Teams Assistant"}),
            )
            .await?;
        let url = claude_url(result["session_url"].as_str().unwrap_or_default())?;
        self.remote_connected = true;
        Ok(Access {
            status: "connected".into(),
            url: Some(url),
            expires_at: Some(chrono::Utc::now().timestamp() + 7200),
            ..Access::default()
        })
    }
}

struct Live {
    access: Access,
    task: tokio::task::JoinHandle<()>,
    observer: Observer,
}
fn sessions() -> &'static Mutex<HashMap<String, Live>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, Live>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}
async fn retain(id: &str, mut session: Session, access: Access, observer: Observer) -> Result<()> {
    let mut sessions = sessions().lock().await;
    sessions.retain(|_, live| !live.task.is_finished());
    ensure!(
        sessions.len() < 16,
        "Close an existing mobile session before connecting another (maximum 16)."
    );
    let deadline = access.expires_at.unwrap_or_default();
    let disconnected = observer.clone();
    let task = tokio::spawn(async move {
        let _ = tokio::time::timeout(
            Duration::from_secs((deadline - chrono::Utc::now().timestamp()).max(0) as u64),
            async { while session.next().await.is_ok() {} },
        )
        .await;
        let _ = observer(Access {
            status: "offline".into(),
            ..Access::default()
        });
    });
    if let Some(old) = sessions.insert(
        id.into(),
        Live {
            access,
            task,
            observer: disconnected,
        },
    ) {
        old.task.abort();
    }
    Ok(())
}
pub(crate) async fn close(id: &str) {
    if let Some(live) = sessions().lock().await.remove(id) {
        live.task.abort();
        let _ = live.task.await;
        let _ = (live.observer)(Access {
            status: "offline".into(),
            ..Access::default()
        });
    }
}
#[cfg(test)]
async fn is_live(id: &str) -> bool {
    sessions()
        .lock()
        .await
        .get(id)
        .is_some_and(|live| !live.task.is_finished())
}

async fn spawn(
    binary: &std::path::Path,
    args: &[OsString],
    directory: &std::path::Path,
) -> Result<Session> {
    let mut command = tokio::process::Command::new(binary);
    command
        .args(args)
        .current_dir(directory)
        .env_clear()
        .envs(agent::child_env(binary))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn()?;
    Ok(Session {
        _tree: ProcessTree(
            child
                .id()
                .ok_or_else(|| anyhow::anyhow!("Coding process did not start."))?,
        ),
        input: child.stdin.take().unwrap(),
        output: BufReader::new(child.stdout.take().unwrap()),
        stderr: tokio::spawn(tail(child.stderr.take().unwrap())),
        child,
        remote_connected: false,
    })
}

pub(super) async fn execute(
    binary: &std::path::Path,
    args: &[OsString],
    directory: &std::path::Path,
    prompt: &str,
    id: &str,
    observer: SessionObserver<'_>,
    remote: Observer,
) -> Result<(bool, String)> {
    let mut session = spawn(binary, args, directory).await?;
    observer(None)?;
    let access = session
        .enable()
        .await
        .unwrap_or_else(|_| Access::unavailable());
    remote(access.clone())?;
    session.send(json!({"type":"user","message":{"role":"user","content":prompt},"parent_tool_use_id":null,"session_id":id})).await?;
    let mut events = Events::default();
    let result = tokio::time::timeout(Duration::from_secs(7200), async {
        loop {
            let event = session.next().await?;
            events.event("claude", &event, observer, id)?;
            if event["type"] == "result" {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await?;
    result?;
    let ok = !events.failed && !events.text.trim().is_empty();
    if access.status == "connected" {
        if retain(id, session, access, remote.clone()).await.is_err() {
            remote(Access::unavailable())?;
        }
    } else {
        let _ = session.child.kill().await;
    }
    let redactor = crate::security::Redactor::new(&[], Vec::new())?;
    Ok((ok, redactor.redact(&events.text)))
}

pub(crate) async fn reconnect(
    choice: (&str, &str, &str),
    id: &str,
    remote_id: Option<&str>,
    directories: &[PathBuf],
    planning: bool,
    observer: Observer,
) -> Result<Access> {
    let (provider, model, effort) = choice;
    uuid::Uuid::parse_str(id)?;
    let binary = agent::locate(provider).ok_or_else(|| {
        anyhow::anyhow!("The selected coding CLI is not installed on the execution machine.")
    })?;
    if provider == "codex" {
        let mut client = super::codex::Client::connect(&binary).await?;
        client
            .resume(
                remote_id.unwrap_or(id),
                (model, effort),
                directories,
                planning,
            )
            .await?;
        return Ok(client.access);
    }
    ensure!(
        provider == "claude",
        "This provider does not support mobile session connections."
    );
    let guard = sessions().lock().await;
    if let Some(live) = guard.get(id).filter(|live| !live.task.is_finished()) {
        return Ok(live.access.clone());
    }
    drop(guard);
    let mut args = arguments(provider, model, effort, directories)?;
    args.extend(["--resume".into(), id.into()]);
    if planning {
        args.extend(["--tools".into(), "".into()]);
    }
    let mut session = spawn(&binary, &args, &directories[0]).await?;
    let access = session.enable().await?;
    retain(id, session, access.clone(), observer).await?;
    Ok(access)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_urls_are_provider_session_urls_without_tokens_or_arbitrary_destinations() {
        assert!(claude_url("https://claude.ai/code/session_123Abc").is_ok());
        for url in [
            "https://claude.ai.evil/code/session_123",
            "https://claude.ai/code/../login",
            "https://claude.ai/code/session_123?token=secret",
            "https://user:secret@claude.ai/code/session_123",
            "https://claude.ai:444/code/session_123",
            "javascript:alert(1)",
            "https://claude.ai/code/local-uuid",
            "https://claude.ai/code/session_123#fragment",
        ] {
            assert!(claude_url(url).is_err(), "{url}");
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn claude_publishes_the_real_remote_id_and_keeps_serving_after_the_initial_result() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("claude-fixture");
        std::fs::write(&binary, r##"#!/bin/sh
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--session-id' ]; then shift; session="$1"; fi
  shift
done
read -r initialize
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"pta-initialize","response":{}}}'
read -r remote
case "$remote" in *'"enabled":true'*) ;; *) exit 7;; esac
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"pta-remote","response":{"session_url":"https://claude.ai/code/session_Remote123"}}}'
read -r prompt
case "$prompt" in *'Synthetic mobile check'*) ;; *) exit 8;; esac
printf '{"type":"system","session_id":"%s"}\n' "$session"
printf '{"type":"result","session_id":"%s","is_error":false,"result":"Completed synthetic check."}\n' "$session"
while read -r control; do :; done
"##).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let native = std::sync::Mutex::new(String::new());
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let updates = observed.clone();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            super::super::execute_at(
                &binary,
                ("claude", "synthetic-model", "low"),
                &[root.path().into()],
                "Synthetic mobile check",
                None,
                &|id| {
                    if let Some(id) = id {
                        *native.lock().unwrap() = id.into();
                    }
                    Ok(())
                },
                Arc::new(move |access| {
                    updates.lock().unwrap().push(access);
                    Ok(())
                }),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.0);
        assert_eq!(result.1, "Completed synthetic check.");
        let id = native.lock().unwrap().clone();
        assert!(is_live(&id).await);
        {
            let updates = observed.lock().unwrap();
            assert_eq!(
                updates[0].url.as_deref(),
                Some("https://claude.ai/code/session_Remote123")
            );
            assert_ne!(
                updates[0].url.as_ref().unwrap().split('/').next_back(),
                Some(id.as_str())
            );
        }
        close(&id).await;
        assert!(!is_live(&id).await);
    }
}
