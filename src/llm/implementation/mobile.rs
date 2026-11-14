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
        crate::security::program_async(binary)
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
    lines: super::lines::Lines,
    /// An event could not be read within the stream's bounds and was skipped.
    skipped: bool,
    _tree: ProcessTree,
    stderr: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    remote_connected: bool,
    /// While a delegated coding session runs nobody answers: a prompt is recorded and refused at
    /// once, even with a mobile connection, so the run never waits for a person.
    autonomous: bool,
    prompts: Vec<String>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stderr.abort();
    }
}
impl Session {
    /// Why the CLI's process stopped before its session ended: at once when the session had
    /// reported nothing (`seen`), with the kind of its first stderr line, or mid-stream. Its
    /// exit code and stderr are read within a short bound; the stderr text is not kept.
    async fn stopped(&mut self, seen: bool) -> Failure {
        let code = tokio::time::timeout(Duration::from_secs(2), self.child.wait())
            .await
            .ok()
            .and_then(|status| status.ok())
            .and_then(|status| status.code());
        if seen {
            return Failure {
                exit_code: code,
                ..Failure::of(Cause::StreamEnded)
            };
        }
        let stderr = tokio::time::timeout(Duration::from_secs(2), &mut self.stderr)
            .await
            .ok()
            .and_then(|read| read.ok())
            .and_then(|read| read.ok())
            .unwrap_or_default();
        Failure::exited_at_start(code, &stderr)
    }
    async fn send(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        Ok(())
    }
    async fn next(&mut self) -> Result<Value> {
        loop {
            // A line is read in bounded memory: an oversized one is read without its payload
            // or skipped, never the end of the session.
            let bytes = self.output.fill_buf().await?;
            let (used, line) = if bytes.is_empty() {
                let line = self.lines.finish();
                ensure!(line.is_some(), "The remote agent connection ended.");
                (0, line)
            } else {
                self.lines.feed(bytes)
            };
            self.output.consume(used);
            if line == Some(super::lines::Line::Skipped) {
                self.skipped = true;
            }
            if let Some(super::lines::Line::Event(value)) = line {
                // The provider bridge forwards prompts to the signed-in person's app and
                // injects their response itself. The host never grants an approval.
                if value["type"] == "control_request" {
                    let request = &value["request"];
                    let asks = ["can_use_tool", "elicitation"]
                        .contains(&request["subtype"].as_str().unwrap_or_default());
                    if self.autonomous && asks {
                        let tool = request["tool_name"]
                            .as_str()
                            .or(request["subtype"].as_str())
                            .unwrap_or("prompt");
                        let input = if request["input"].is_null() {
                            &request["message"]
                        } else {
                            &request["input"]
                        };
                        keep_prompt(&mut self.prompts, &prompt_text(tool, input));
                    }
                    if !self.remote_connected || (self.autonomous && asks) {
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
/// Whether the session `id` is retained and its process still runs on this machine.
pub(crate) async fn alive(id: &str) -> bool {
    sessions()
        .lock()
        .await
        .get(id)
        .is_some_and(|live| !live.task.is_finished())
}

/// Records a connected session `offline` when its follow ends without retaining it, on every
/// path: an answer, an error, the limit, or the run's task aborted (a cancel) with the session.
struct Disconnect(Option<Observer>);
impl Drop for Disconnect {
    fn drop(&mut self) {
        if let Some(observer) = self.0.take() {
            let _ = observer(Access {
                status: "offline".into(),
                ..Access::default()
            });
        }
    }
}

async fn spawn(
    binary: &std::path::Path,
    args: &[OsString],
    directory: &std::path::Path,
) -> Result<Session> {
    let mut command = crate::security::program_async(binary);
    command
        .args(args)
        .current_dir(directory)
        .env_clear()
        .envs(agent::child_env(binary))
        // The CLI says when it is idle: the authoritative end of its work (`Activity`).
        .env("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| Failed::with(Cause::CliMissing, "Coding process did not start."))?;
    Ok(Session {
        _tree: ProcessTree(
            child
                .id()
                .ok_or_else(|| anyhow::anyhow!("Coding process did not start."))?,
        ),
        input: child.stdin.take().unwrap(),
        output: BufReader::new(child.stdout.take().unwrap()),
        lines: super::lines::Lines::new(),
        skipped: false,
        stderr: tokio::spawn(tail(child.stderr.take().unwrap())),
        child,
        remote_connected: false,
        autonomous: false,
        prompts: Vec::new(),
    })
}

/// How long a session whose last turn ended with nothing pending is watched for another turn
/// when the CLI has not said it is idle (a CLI that predates the signal, or a task that just
/// finished and may wake a turn).
const SETTLE: Duration = Duration::from_secs(5);

/// What a Claude session is doing, read from its stream. In `stream-json` input mode a
/// `result` ends a turn, not the session: a background task (a shell, a subagent, a monitor)
/// that finishes later wakes another turn. The session is over when its last turn ended,
/// nothing it started is pending and the CLI says it is idle; without that signal, after
/// `SETTLE` with no new activity.
#[derive(Default)]
struct Activity {
    /// A turn ended (`result`) and no turn started since.
    turn_ended: bool,
    /// The CLI said it is idle since the last turn ended and nothing finished since.
    idle: bool,
    /// Background tasks that started and have not finished, without the ambient ones (live
    /// watchers the CLI marks as not activity).
    pending: std::collections::HashSet<String>,
}
impl Activity {
    fn event(&mut self, event: &Value) {
        let task = || event["task_id"].as_str().unwrap_or_default().to_owned();
        match (event["type"].as_str(), event["subtype"].as_str()) {
            (Some("result"), _) => {
                self.turn_ended = true;
                self.idle = false;
            }
            (Some("assistant" | "user" | "stream_event"), _) => {
                self.turn_ended = false;
                self.idle = false;
            }
            (Some("system"), Some("session_state_changed")) => {
                self.idle = event["state"] == "idle";
                if !self.idle {
                    self.turn_ended = false;
                }
            }
            (Some("system"), Some("task_started")) if event["ambient"] != true => {
                self.pending.insert(task());
            }
            (Some("system"), Some("task_notification")) => self.finished(&[task()]),
            (Some("system"), Some("task_updated"))
                if ["completed", "failed", "killed"]
                    .contains(&event["patch"]["status"].as_str().unwrap_or_default()) =>
            {
                self.finished(&[task()])
            }
            // The full set of live tasks: it replaces what the edges said.
            (Some("system"), Some("background_tasks_changed")) => {
                let live: std::collections::HashSet<String> = event["tasks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|t| t["ambient"] != true)
                    .filter_map(|t| t["task_id"].as_str().map(str::to_owned))
                    .collect();
                let gone: Vec<String> = self.pending.difference(&live).cloned().collect();
                self.finished(&gone);
                self.pending = live;
            }
            _ => {}
        }
    }
    /// A task that finishes may wake a turn: the session is not idle until the CLI says so.
    fn finished(&mut self, tasks: &[String]) {
        for task in tasks {
            if self.pending.remove(task) {
                self.idle = false;
            }
        }
    }
    fn ended(&self) -> bool {
        self.turn_ended
    }
    /// The last turn ended and nothing is pending: over once the CLI says idle, or after
    /// `SETTLE` without activity.
    fn settling(&self) -> bool {
        self.turn_ended && self.pending.is_empty()
    }
    fn over(&self) -> bool {
        self.settling() && self.idle
    }
}

/// Run one Claude session; a coding session (`planning` false) is autonomous until it ends.
/// With `keep`, a session that ended with its answer stays reachable through Remote Control
/// for the person (`retain`); any other session's process ends with its run.
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute(
    binary: &std::path::Path,
    args: &[OsString],
    directory: &std::path::Path,
    prompt: &str,
    id: &str,
    (planning, keep): (bool, bool),
    observer: SessionObserver<'_>,
    remote: Observer,
) -> Result<Outcome> {
    follow_session(
        binary,
        args,
        directory,
        prompt,
        id,
        (planning, keep),
        observer,
        remote,
        (SESSION_LIMIT, SETTLE),
    )
    .await
}

/// `execute` within `(limit, settle)`: the run ends when the session does (`Activity`), when
/// its process exits after a turn, or at `limit`, which stops it and records why.
#[allow(clippy::too_many_arguments)]
async fn follow_session(
    binary: &std::path::Path,
    args: &[OsString],
    directory: &std::path::Path,
    prompt: &str,
    id: &str,
    (planning, keep): (bool, bool),
    observer: SessionObserver<'_>,
    remote: Observer,
    (limit, settle): (Duration, Duration),
) -> Result<Outcome> {
    let mut session = spawn(binary, args, directory).await?;
    observer(None)?;
    let access = session
        .enable()
        .await
        .unwrap_or_else(|_| Access::unavailable());
    remote(access.clone())?;
    // Declared after the session, so it is dropped first: the record says offline as the
    // process tree ends.
    let mut offline = Disconnect((access.status == "connected").then(|| remote.clone()));
    session.autonomous = !planning;
    if session.send(json!({"type":"user","message":{"role":"user","content":prompt},"parent_tool_use_id":null,"session_id":id})).await.is_err() {
        // Its input closed: the process has already ended.
        return Err(Failed(session.stopped(false).await).into());
    }
    let mut events = Events::default();
    let mut activity = Activity::default();
    // The stream ended while the session still worked (`Failed` with the cause), or an
    // observer refused the session.
    let followed = tokio::time::timeout(limit, async {
        while !activity.over() {
            let next = if activity.settling() {
                match tokio::time::timeout(settle, session.next()).await {
                    Ok(next) => next,
                    Err(_) => break,
                }
            } else {
                session.next().await
            };
            let event = match next {
                Ok(event) => event,
                // The process exited after its turn: the session is over.
                Err(_) if activity.ended() => break,
                Err(_) => return Err(Failed(session.stopped(events.seen).await).into()),
            };
            events.read("claude", &event, session.lines.dropped(), observer, id)?;
            activity.event(&event);
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    let Ok(followed) = followed else {
        // Dropping the session stops its process tree.
        let mut text = LIMIT_REACHED.to_owned();
        if !events.text.trim().is_empty() {
            text.push_str("\n\nIts last report:\n");
            text.push_str(&events.text);
        }
        let mut prompts = events.prompts;
        for prompt in std::mem::take(&mut session.prompts) {
            keep_prompt(&mut prompts, &prompt);
        }
        let text = noted(text, session.skipped, planning);
        return Ok(Outcome::redacted(false, &text, &prompts)?.because(Failure::of(Cause::Timeout)));
    };
    followed?;
    let ok = !events.failed && !events.text.trim().is_empty();
    let failure = events.failure();
    // After the delegated session the retained connection belongs to the person's mobile app.
    session.autonomous = false;
    for prompt in std::mem::take(&mut session.prompts) {
        keep_prompt(&mut events.prompts, &prompt);
    }
    let text = noted(std::mem::take(&mut events.text), session.skipped, planning);
    if keep && ok && access.status == "connected" {
        offline.0 = None;
        if retain(id, session, access, remote.clone()).await.is_err() {
            remote(Access::unavailable())?;
        }
    } else {
        let _ = session.child.kill().await;
    }
    Ok(Outcome::redacted(ok, &text, &events.prompts)?.because(failure))
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
    // A resumed session keeps the permission mode its run started with.
    let mut args = arguments(provider, model, effort, directories, planning)?;
    args.extend(["--resume".into(), id.into()]);
    if planning {
        args.extend(["--tools".into(), "".into()]);
    }
    let mut session = spawn(&binary, &args, &directories[0]).await?;
    let access = session.enable().await?;
    retain(id, session, access.clone(), observer).await?;
    Ok(access)
}

/// A fake Claude CLI whose Remote Control connects, for tests here and in the host's.
#[cfg(all(test, unix))]
pub(crate) mod fake {
    use std::path::Path;
    use std::time::Duration;
    /// Install at `binary` a Claude CLI that appends its process ID to `log/pids`, connects
    /// Remote Control, reads the prompt, runs `body` (which prints its turn; the prompt line is
    /// in `$prompt`), says it is idle and then stays open, as a kept session does.
    pub(crate) fn connected_cli(binary: &Path, log: &Path, body: &str) {
        let script = r##"#!/bin/sh
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--session-id' ]; then shift; session="$1"; fi
  shift
done
echo $$ >> '{log}/pids'
read -r initialize
printf '%s
' '{"type":"control_response","response":{"subtype":"success","request_id":"pta-initialize","response":{}}}'
read -r remote
printf '%s
' '{"type":"control_response","response":{"subtype":"success","request_id":"pta-remote","response":{"session_url":"https://claude.ai/code/session_Remote123"}}}'
read -r prompt
printf '{"type":"system","subtype":"init","session_id":"%s"}
' "$session"
{body}
printf '%s
' '{"type":"system","subtype":"session_state_changed","state":"idle"}'
while read -r line; do :; done
"##;
        crate::llm::agent::install_fake_cli(
            binary,
            &script
                .replace("{log}", &log.display().to_string())
                .replace("{body}", body),
        );
    }
    /// The process IDs the fake CLIs logged in `log`, in start order.
    pub(crate) fn pids(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log.join("pids"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
    /// Whether process `pid` still runs (a zombie has ended).
    pub(crate) fn runs(pid: &str) -> bool {
        let output = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&output.stdout);
        output.status.success() && !stat.trim().is_empty() && !stat.trim().starts_with('Z')
    }
    /// Whether process `pid` ends within five seconds.
    pub(crate) async fn ends(pid: &str) -> bool {
        for _ in 0..100 {
            if !runs(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }
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
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("claude-fixture");
        crate::llm::agent::install_fake_cli(
            &binary,
            r##"#!/bin/sh
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
printf '%s\n' '{"type":"system","subtype":"session_state_changed","state":"idle"}'
while read -r control; do :; done
"##,
        );
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
        assert!(result.ok);
        assert_eq!(result.text, "Completed synthetic check.");
        let id = native.lock().unwrap().clone();
        assert!(alive(&id).await);
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
        assert!(!alive(&id).await);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn only_a_session_that_answered_and_may_be_kept_outlives_its_run() {
        // (the turn failed, the session may be kept, it is kept)
        for (failed, keep, kept) in [
            (false, true, true),
            (true, true, false),
            (false, false, false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let binary = root.path().join("claude-fixture");
            fake::connected_cli(
                &binary,
                root.path(),
                &format!(
                    r#"printf '%s\n' '{{"type":"result","is_error":{failed},"result":"Answered."}}'"#
                ),
            );
            let id = uuid::Uuid::new_v4().to_string();
            let observed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let updates = observed.clone();
            let args = arguments(
                "claude",
                "synthetic-model",
                "low",
                &[root.path().into()],
                false,
            )
            .unwrap();
            let outcome = tokio::time::timeout(
                Duration::from_secs(10),
                follow_session(
                    &binary,
                    &args,
                    root.path(),
                    "Synthetic coding task",
                    &id,
                    (false, keep),
                    &|_| Ok(()),
                    Arc::new(move |access| {
                        updates.lock().unwrap().push(access.status);
                        Ok(())
                    }),
                    (Duration::from_secs(30), Duration::from_secs(5)),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(outcome.ok, !failed);
            let pid = fake::pids(root.path()).pop().unwrap();
            let last = || observed.lock().unwrap().last().cloned().unwrap();
            assert_eq!(alive(&id).await, kept, "{failed} {keep}");
            if kept {
                // Handed to the user: reachable until it is closed or its connection expires.
                assert!(fake::runs(&pid));
                assert_eq!(last(), "connected");
                close(&id).await;
                assert!(!alive(&id).await);
            }
            assert!(fake::ends(&pid).await, "{failed} {keep}");
            assert_eq!(last(), "offline");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_cancelled_session_ends_its_process_and_reads_offline() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("claude-fixture");
        fake::connected_cli(&binary, root.path(), "while :; do sleep 1; done");
        let observed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let updates = observed.clone();
        let args = arguments(
            "claude",
            "synthetic-model",
            "low",
            &[root.path().into()],
            false,
        )
        .unwrap();
        let following = {
            let (binary, root) = (binary.clone(), root.path().to_path_buf());
            tokio::spawn(async move {
                follow_session(
                    &binary,
                    &args,
                    &root,
                    "Synthetic coding task",
                    &uuid::Uuid::new_v4().to_string(),
                    (false, true),
                    &|_| Ok(()),
                    Arc::new(move |access| {
                        updates.lock().unwrap().push(access.status);
                        Ok(())
                    }),
                    (Duration::from_secs(60), Duration::from_secs(5)),
                )
                .await
            })
        };
        let pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !observed.lock().unwrap().is_empty()
                    && let Some(pid) = fake::pids(root.path()).pop()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(fake::runs(&pid));
        // `implementation_cancel` aborts the run's task.
        following.abort();
        let _ = following.await;
        assert!(fake::ends(&pid).await);
        assert_eq!(
            *observed.lock().unwrap(),
            ["connected".to_owned(), "offline".to_owned()]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_claude_coding_turn_never_waits_on_a_prompt_and_records_it() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("claude-fixture");
        // Remote Control is refused, so nothing else could answer; the run must not wait.
        crate::llm::agent::install_fake_cli(
            &binary,
            r##"#!/bin/sh
printf '%s ' "$@" > arguments.txt
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--session-id' ]; then shift; session="$1"; fi
  shift
done
read -r initialize
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"pta-initialize","response":{}}}'
read -r remote
printf '%s\n' '{"type":"control_response","response":{"subtype":"error","request_id":"pta-remote","error":"not eligible"}}'
read -r prompt
printf '{"type":"system","session_id":"%s"}\n' "$session"
printf '%s\n' '{"type":"control_request","request_id":"ask-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"gh pr create --fill"}}}'
read -r answer
case "$answer" in *'"request_id":"ask-1"'*'error'*|*'error'*'"request_id":"ask-1"'*) ;; *) exit 11;; esac
printf '{"type":"result","session_id":"%s","is_error":false,"result":"Opened nothing.","permission_denials":[{"tool_name":"Bash","tool_input":{"command":"gh pr create --fill"}}]}\n' "$session"
"##,
        );
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            super::super::execute_at(
                &binary,
                ("claude", "synthetic-model", "low"),
                &[root.path().into()],
                "Synthetic coding task",
                None,
                &|_| Ok(()),
                Arc::new(|_| Ok(())),
            ),
        )
        .await
        .expect("The run waited for an answer")
        .unwrap();
        assert!(outcome.ok);
        assert_eq!(
            outcome.prompts,
            [r#"Bash: {"command":"gh pr create --fill"}"#]
        );
        let launched = std::fs::read_to_string(root.path().join("arguments.txt")).unwrap();
        assert!(
            launched.contains("--permission-mode bypassPermissions "),
            "{launched}"
        );
        assert!(
            launched.contains("--disallowedTools AskUserQuestion "),
            "{launched}"
        );
    }

    /// A fake Claude CLI whose session runs `body` after the prompt; Remote Control is refused,
    /// so the session is not retained after the run.
    #[cfg(unix)]
    fn fake_session(root: &std::path::Path, body: &str) -> PathBuf {
        let binary = root.join("claude-fixture");
        crate::llm::agent::install_fake_cli(
            &binary,
            &format!(
                r##"#!/bin/sh
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--session-id' ]; then shift; session="$1"; fi
  shift
done
read -r initialize
printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"pta-initialize","response":{{}}}}}}'
read -r remote
printf '%s\n' '{{"type":"control_response","response":{{"subtype":"error","request_id":"pta-remote","error":"not eligible"}}}}'
read -r prompt
printf '{{"type":"system","subtype":"init","session_id":"%s"}}\n' "$session"
{body}
"##
            ),
        );
        binary
    }
    #[cfg(unix)]
    async fn run_session(
        binary: &std::path::Path,
        root: &std::path::Path,
        limits: (Duration, Duration),
    ) -> Outcome {
        try_session(binary, root, limits).await.unwrap()
    }
    #[cfg(unix)]
    async fn try_session(
        binary: &std::path::Path,
        root: &std::path::Path,
        limits: (Duration, Duration),
    ) -> Result<Outcome> {
        let args = arguments("claude", "synthetic-model", "low", &[root.into()], false).unwrap();
        follow_session(
            binary,
            &args,
            root,
            "Synthetic coding task",
            &uuid::Uuid::new_v4().to_string(),
            (false, true),
            &|_| Ok(()),
            Arc::new(|_| Ok(())),
            limits,
        )
        .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_that_stops_early_records_why_and_never_what_its_cli_wrote() {
        use super::super::failure::Stderr;
        use super::super::{Cause, Failure, of_error};
        let limits = (Duration::from_secs(20), Duration::from_secs(5));
        // A CLI that exits with code 1 before emitting anything, saying on stderr that a
        // system variable is missing.
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("claude-fixture");
        crate::llm::agent::install_fake_cli(
            &binary,
            "#!/bin/sh\necho 'Error: the SystemRoot environment variable is not set (C:\\Users\\someone, token sk-synthetic)' >&2\nexit 1\n",
        );
        let error = try_session(&binary, root.path(), limits).await.unwrap_err();
        assert_eq!(
            of_error(&error, Cause::Unknown),
            Failure {
                cause: Cause::CliExitedAtStart,
                exit_code: Some(1),
                stderr: Some(Stderr::MissingVariable),
            }
        );
        let text = format!("{error:#}");
        assert!(text.contains("exit code 1"), "{text}");
        for leaked in ["someone", "sk-synthetic", "SystemRoot environment"] {
            assert!(!text.contains(leaked), "{text}");
        }
        // A CLI that is not there.
        let error = try_session(&root.path().join("missing"), root.path(), limits)
            .await
            .unwrap_err();
        assert_eq!(of_error(&error, Cause::Unknown).cause, Cause::CliMissing);
        // A stream that ends while the session still works.
        let root = tempfile::tempdir().unwrap();
        let binary = fake_session(root.path(), "exit 3");
        let error = try_session(&binary, root.path(), limits).await.unwrap_err();
        assert_eq!(
            of_error(&error, Cause::Unknown),
            Failure {
                cause: Cause::StreamEnded,
                exit_code: Some(3),
                stderr: None,
            }
        );
        // A turn the CLI reports as an error, one without a report, and a report too large to
        // read.
        for (body, cause) in [
            (
                r#"printf '%s\n' '{"type":"result","is_error":true,"result":"Synthetic API error."}'"#,
                Cause::AgentError,
            ),
            (
                r#"printf '%s\n' '{"type":"result","is_error":false,"result":""}'"#,
                Cause::NoReport,
            ),
            (
                r#"printf '{"type":"result","is_error":false,"result":"%s"}\n' "$(head -c 300000 /dev/zero | tr '\0' R)""#,
                Cause::OutputLimit,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let binary = fake_session(root.path(), body);
            let outcome = run_session(&binary, root.path(), limits).await;
            assert!(!outcome.ok, "{body}");
            assert_eq!(outcome.failure, Some(Failure::of(cause)), "{body}");
        }
        // A session at its time limit.
        let root = tempfile::tempdir().unwrap();
        let binary = fake_session(root.path(), "while :; do sleep 1; done");
        let outcome = run_session(
            &binary,
            root.path(),
            (Duration::from_millis(500), Duration::from_secs(5)),
        )
        .await;
        assert_eq!(outcome.failure, Some(Failure::of(Cause::Timeout)));
        // A session that ends with its answer has no failure.
        let root = tempfile::tempdir().unwrap();
        let binary = fake_session(
            root.path(),
            r#"printf '%s\n' '{"type":"result","is_error":false,"result":"Done."}'"#,
        );
        let outcome = run_session(&binary, root.path(), limits).await;
        assert!(outcome.ok);
        assert_eq!(outcome.failure, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_turn_that_ends_with_a_background_task_pending_is_not_the_end_of_the_run() {
        let root = tempfile::tempdir().unwrap();
        // The first turn starts a pipeline watch in the background and ends; the task's
        // notification wakes a second turn, which finishes the work and reports the branch.
        let binary = fake_session(
            root.path(),
            r##"[ "$CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS" = 1 ] || exit 9
printf '%s\n' '{"type":"system","subtype":"session_state_changed","state":"running"}'
printf '%s\n' '{"type":"system","subtype":"task_started","task_id":"watch","description":"Pipeline","is_backgrounded":true}'
printf '%s\n' '{"type":"system","subtype":"task_started","task_id":"inbox","description":"Watcher","ambient":true}'
printf '%s\n' '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"watch","task_type":"local_bash","description":"Pipeline"},{"task_id":"inbox","task_type":"monitor","description":"Watcher","ambient":true}]}'
printf '%s\n' '{"type":"result","is_error":false,"result":"The pipeline is running; I will be notified when it finishes."}'
printf '%s\n' '{"type":"system","subtype":"session_state_changed","state":"idle"}'
sleep 1
echo second-turn > second-turn.txt
printf '%s\n' '{"type":"system","subtype":"task_notification","task_id":"watch","status":"completed","output_file":"","summary":"Pipeline passed"}'
printf '%s\n' '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"inbox","task_type":"monitor","description":"Watcher","ambient":true}]}'
printf '%s\n' '{"type":"system","subtype":"session_state_changed","state":"running"}'
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Opening the pull request."}]}}'
printf '%s\n' '{"type":"result","is_error":false,"result":"Opened the pull request.\nBranch: feature/two-turns"}'
printf '%s\n' '{"type":"system","subtype":"session_state_changed","state":"idle"}'
while read -r line; do :; done"##,
        );
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            run_session(
                &binary,
                root.path(),
                (Duration::from_secs(30), Duration::from_secs(5)),
            ),
        )
        .await
        .expect("The run did not end when the session went idle with nothing pending");
        assert!(root.path().join("second-turn.txt").exists());
        assert!(outcome.ok);
        assert_eq!(
            outcome.text,
            "Opened the pull request.\nBranch: feature/two-turns"
        );
        // The idle signal ends it at once: no settle time added.
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_cli_that_does_not_say_it_is_idle_ends_after_the_settle_time_or_its_next_turn() {
        let root = tempfile::tempdir().unwrap();
        let binary = fake_session(
            root.path(),
            r##"printf '%s\n' '{"type":"result","is_error":true,"result":"First turn failed."}'
sleep 0.2
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[]}}'
printf '%s\n' '{"type":"result","is_error":false,"result":"Done.\nBranch: feature/settled"}'
while read -r line; do :; done"##,
        );
        let started = std::time::Instant::now();
        let outcome = run_session(
            &binary,
            root.path(),
            (Duration::from_secs(30), Duration::from_secs(1)),
        )
        .await;
        // The last turn's report and its success count, not the first turn's error.
        assert!(outcome.ok);
        assert_eq!(outcome.text, "Done.\nBranch: feature/settled");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(5),
            "{elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_that_never_ends_is_stopped_at_the_limit_with_its_cause() {
        let root = tempfile::tempdir().unwrap();
        let binary = fake_session(
            root.path(),
            r##"printf '%s\n' '{"type":"system","subtype":"task_started","task_id":"forever","description":"Watch"}'
printf '%s\n' '{"type":"result","is_error":false,"result":"Waiting.\nBranch: feature/unfinished"}'
printf '%s\n' '{"type":"system","subtype":"session_state_changed","state":"idle"}'
while read -r line; do :; done"##,
        );
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            run_session(
                &binary,
                root.path(),
                (Duration::from_secs(1), Duration::from_millis(200)),
            ),
        )
        .await
        .expect("The limit did not stop the session");
        assert!(!outcome.ok);
        assert!(outcome.text.starts_with(LIMIT_REACHED), "{}", outcome.text);
        assert!(outcome.text.contains("Waiting."));
    }

    /// A shell function for the fake CLI: `image N` prints Claude Code's line for a tool
    /// result that read an image of N base64 bytes, which the line carries twice.
    #[cfg(unix)]
    const IMAGE: &str = r##"image() {
  printf '%s' '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"read-1","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"'
  head -c "$1" /dev/zero | tr '\0' A
  printf '%s' '"}}]}]},"parent_tool_use_id":null,"tool_use_result":{"type":"image","file":{"base64":"'
  head -c "$1" /dev/zero | tr '\0' A
  printf '%s\n' '","type":"image/png"}}}'
}
"##;

    #[cfg(unix)]
    #[tokio::test]
    async fn reading_large_images_does_not_end_the_session() {
        let root = tempfile::tempdir().unwrap();
        // Lines of about 300 KB and 5 MB between ordinary events.
        let binary = fake_session(
            root.path(),
            &format!(
                r##"{IMAGE}printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Checking the screenshots."}}]}}}}'
image 150000
printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Checking the larger one."}}]}}}}'
image 2500000
printf '%s\n' '{{"type":"result","is_error":false,"result":"Opened the pull request.\nBranch: feature/screenshots"}}'
printf '%s\n' '{{"type":"system","subtype":"session_state_changed","state":"idle"}}'
while read -r line; do :; done"##
            ),
        );
        let outcome = tokio::time::timeout(
            Duration::from_secs(20),
            run_session(
                &binary,
                root.path(),
                (Duration::from_secs(60), Duration::from_secs(5)),
            ),
        )
        .await
        .expect("The session did not end after its last turn");
        // `done`: it completed, raised no prompt and its report is the final turn's.
        assert!(outcome.ok);
        assert!(outcome.prompts.is_empty());
        assert_eq!(
            outcome.text,
            "Opened the pull request.\nBranch: feature/screenshots"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn events_after_an_oversized_line_are_read_and_a_skipped_one_is_noted_once() {
        let root = tempfile::tempdir().unwrap();
        let binary = fake_session(
            root.path(),
            &format!(
                r##"{IMAGE}image 2500000
printf '%s\n' '{{"type":"control_request","request_id":"ask-1","request":{{"subtype":"can_use_tool","tool_name":"Bash","input":{{"command":"az boards work-item update"}}}}}}'
read -r answer
case "$answer" in *'"request_id":"ask-1"'*) ;; *) exit 11;; esac
image 150000
printf '%s\n' '{{"type":"system","subtype":"status","session_id":"22222222-2222-4222-8222-222222222222"}}'
# Two events that cannot be read: one that is not JSON, one far over the kept bound.
printf '%s' '{{"type":"user","secret":'; head -c 300000 /dev/zero | tr '\0' 7; printf '%s\n' ']'
printf '%s' '{{"type":"user","parts":['; i=0; while [ $i -lt 6 ]; do printf '"'; head -c 200000 /dev/zero | tr '\0' S; printf '",'; i=$((i+1)); done; printf '%s\n' '""]}}'
image 150000
printf '%s\n' '{{"type":"result","session_id":"22222222-2222-4222-8222-222222222222","is_error":false,"result":"Done.\nBranch: feature/after"}}'
printf '%s\n' '{{"type":"system","subtype":"session_state_changed","state":"idle"}}'
while read -r line; do :; done"##
            ),
        );
        let observed = std::sync::Mutex::new(Vec::new());
        let args = arguments(
            "claude",
            "synthetic-model",
            "low",
            &[root.path().into()],
            false,
        )
        .unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(20),
            follow_session(
                &binary,
                &args,
                root.path(),
                "Synthetic coding task",
                &uuid::Uuid::new_v4().to_string(),
                (false, true),
                &|id| {
                    observed.lock().unwrap().extend(id.map(str::to_owned));
                    Ok(())
                },
                Arc::new(|_| Ok(())),
                (Duration::from_secs(60), Duration::from_secs(5)),
            ),
        )
        .await
        .expect("The session did not end after its last turn")
        .unwrap();
        assert_eq!(
            observed.lock().unwrap().last().map(String::as_str),
            Some("22222222-2222-4222-8222-222222222222")
        );
        assert_eq!(
            outcome.prompts,
            [r#"Bash: {"command":"az boards work-item update"}"#]
        );
        assert!(outcome.ok);
        assert_eq!(
            outcome.text,
            format!("Done.\nBranch: feature/after\n\n{SKIPPED_EVENT}")
        );
    }

    #[test]
    fn the_session_is_over_only_after_its_last_turn_with_nothing_pending() {
        let mut activity = Activity::default();
        let event = |activity: &mut Activity, value: Value| activity.event(&value);
        assert!(!activity.ended());
        event(
            &mut activity,
            json!({"type":"system","subtype":"task_started","task_id":"a"}),
        );
        event(&mut activity, json!({"type":"result"}));
        event(
            &mut activity,
            json!({"type":"system","subtype":"session_state_changed","state":"idle"}),
        );
        assert!(activity.ended() && !activity.over() && !activity.settling());
        // A finished task wakes a turn: until the CLI says idle again, it is only settling.
        event(
            &mut activity,
            json!({"type":"system","subtype":"task_updated","task_id":"a","patch":{"status":"completed"}}),
        );
        assert!(activity.settling() && !activity.over());
        event(
            &mut activity,
            json!({"type":"system","subtype":"session_state_changed","state":"running"}),
        );
        assert!(!activity.ended() && !activity.settling());
        event(&mut activity, json!({"type":"result"}));
        assert!(activity.settling());
        event(
            &mut activity,
            json!({"type":"system","subtype":"session_state_changed","state":"idle"}),
        );
        assert!(activity.over());
        // The level replaces the edges; an ambient watcher is not work.
        event(
            &mut activity,
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b"},{"task_id":"c","ambient":true}]}),
        );
        assert!(!activity.over());
        event(
            &mut activity,
            json!({"type":"system","subtype":"task_notification","task_id":"b","status":"stopped"}),
        );
        assert!(activity.settling());
    }
}
