//! Use the user's shared Codex daemon so mobile and PTA see the same live thread.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::collections::VecDeque;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};

#[cfg(unix)]
type Stream = tokio::net::UnixStream;
#[cfg(windows)]
type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

pub(super) struct Client {
    ws: WebSocketStream<Stream>,
    pending: VecDeque<Value>,
    next_id: u64,
    socket: String,
    pub(super) access: mobile::Access,
}
impl Client {
    async fn socket(socket: &str, access: mobile::Access) -> Result<Self> {
        #[cfg(unix)]
        let stream = {
            ensure!(
                std::path::Path::new(socket).is_absolute(),
                "Codex returned an invalid local socket."
            );
            tokio::net::UnixStream::connect(socket).await?
        };
        #[cfg(windows)]
        let stream = {
            ensure!(
                socket.starts_with(r"\\.\pipe\"),
                "Codex returned an invalid local pipe."
            );
            tokio::net::windows::named_pipe::ClientOptions::new().open(socket)?
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(2_000_000))
            .max_frame_size(Some(2_000_000));
        let (ws, _) =
            tokio_tungstenite::client_async_with_config("ws://localhost", stream, Some(config))
                .await?;
        let mut client = Self {
            ws,
            pending: VecDeque::new(),
            next_id: 0,
            socket: socket.into(),
            access,
        };
        client.rpc("initialize", json!({"clientInfo":{"name":"personal-teams-assistant","version":env!("CARGO_PKG_VERSION")}})).await?;
        client.send(json!({"method":"initialized"})).await?;
        Ok(client)
    }
    pub(super) async fn connect(binary: &std::path::Path) -> Result<Self> {
        let (access, socket) = mobile::codex_start(binary).await?;
        tokio::time::timeout(Duration::from_secs(10), Self::socket(&socket, access)).await?
    }
    async fn send(&mut self, value: Value) -> Result<()> {
        self.ws
            .send(Message::Text(value.to_string().into()))
            .await?;
        Ok(())
    }
    async fn receive(&mut self) -> Result<Value> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(text))) => return Ok(serde_json::from_str(&text)?),
                Some(Ok(Message::Ping(_))) => {
                    self.ws.flush().await?;
                }
                Some(Ok(Message::Pong(_))) => {}
                _ => anyhow::bail!(
                    "The Codex daemon connection ended. Inspect the recorded session before starting another run."
                ),
            }
        }
    }
    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"id":id,"method":method,"params":params}))
            .await?;
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let event = self.receive().await?;
                if event["id"] == id {
                    ensure!(event.get("error").is_none(), "Codex could not complete the session operation. Check its CLI version and recorded session on the execution machine.");
                    return Ok(event["result"].clone());
                }
                ensure!(self.pending.len() < 100, "Too many pending Codex notifications.");
                self.pending.push_back(event);
            }
        }).await?
    }
    pub(super) async fn resume(
        &mut self,
        id: &str,
        choice: (&str, &str),
        directories: &[PathBuf],
        planning: bool,
    ) -> Result<()> {
        uuid::Uuid::parse_str(id)?;
        let mut params = self.parameters(choice, directories, planning).await?;
        params["threadId"] = json!(id);
        params["excludeTurns"] = json!(true);
        let resumed = self.rpc("thread/resume", params.clone()).await?;
        let thread = if resumed["thread"]["source"] == "exec" {
            // Legacy exec transcripts are hidden by mobile's interactive source filter.
            // A native fork preserves that transcript while giving mobile an interactive thread.
            self.rpc("thread/fork", params).await?
        } else {
            resumed
        };
        let native = thread["thread"]["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Codex did not report a remote thread ID."))?;
        uuid::Uuid::parse_str(native)?;
        self.access.session_id = Some(native.into());
        Ok(())
    }
    pub(super) async fn cancel(&mut self, id: &str) -> Result<()> {
        uuid::Uuid::parse_str(id)?;
        self.rpc("thread/resume", json!({"threadId":id,"excludeTurns":true}))
            .await?;
        let turns = self
            .rpc(
                "thread/turns/list",
                json!({"threadId":id,"limit":1,"itemsView":"notLoaded"}),
            )
            .await?;
        let Some(turn) = turns["data"].as_array().and_then(|turns| turns.first()) else {
            return Ok(());
        };
        if turn["status"] != "inProgress" {
            return Ok(());
        }
        let turn = turn["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Codex did not report the current turn."))?
            .to_owned();
        self.rpc("turn/interrupt", json!({"threadId":id,"turnId":turn}))
            .await?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let event = match self.pending.pop_front() {
                    Some(event) => event,
                    None => self.receive().await?,
                };
                if event["method"] == "turn/completed"
                    && event["params"]["threadId"] == id
                    && event["params"]["turn"]["id"] == turn
                {
                    return Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await??;
        Ok(())
    }
    async fn parameters(
        &mut self,
        choice: (&str, &str),
        directories: &[PathBuf],
        planning: bool,
    ) -> Result<Value> {
        let (model, effort) = choice;
        let mut config = json!({"model_reasoning_effort":effort,"sandbox_workspace_write":{"writable_roots":directories}});
        if planning {
            config["web_search"] = json!("disabled");
            for feature in agent::CODEX_DISABLED_FEATURES {
                config[format!("features.{feature}")] = json!(false);
            }
            // Disable every configured MCP server rather than merging an empty table into user settings.
            let saved = self
                .rpc("config/read", json!({"includeLayers":false}))
                .await?;
            if let Some(servers) = saved["config"]["mcp_servers"].as_object() {
                let mut disabled = config_table(servers);
                for server in disabled.values_mut() {
                    server["enabled"] = json!(false);
                }
                config["mcp_servers"] = json!(disabled);
            }
        }
        let mut params = json!({"model":model,"cwd":directories[0],"approvalPolicy":"never",
            "sandbox":if planning { "read-only" } else { "workspace-write" },"config":config,"ephemeral":false});
        if planning {
            params["baseInstructions"] = json!(
                "Generate only the requested JSON from the supplied context. Do not use tools or write to providers."
            );
        }
        Ok(params)
    }
    pub(super) async fn execute(
        mut self,
        choice: (&str, &str),
        directories: &[PathBuf],
        prompt: &str,
        schema: Option<Value>,
        observer: SessionObserver<'_>,
        remote: mobile::Observer,
    ) -> Result<(bool, String)> {
        let (model, effort) = choice;
        let params = self
            .parameters(choice, directories, schema.is_some())
            .await?;
        let started = self.rpc("thread/start", params).await?;
        let thread = started["thread"]["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Codex did not report a thread ID."))?
            .to_owned();
        uuid::Uuid::parse_str(&thread)?;
        observer(None)?;
        observer(Some(&thread))?;
        self.access.session_id = Some(thread.clone());
        remote(self.access.clone())?;
        let mut interrupt = Interrupt {
            socket: self.socket.clone(),
            thread: thread.clone(),
            turn: None,
            complete: false,
        };
        let turn = self.rpc("turn/start", json!({"threadId":thread,"input":[{"type":"text","text":prompt}],"model":model,"effort":effort,"outputSchema":schema})).await?;
        let turn_id = turn["turn"]["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Codex did not report a turn ID."))?
            .to_owned();
        interrupt.turn = Some(turn_id.clone());
        let mut text = String::new();
        let ok = tokio::time::timeout(Duration::from_secs(7200), async {
            loop {
                let event = match self.pending.pop_front() { Some(event) => event, None => self.receive().await? };
                let method = event["method"].as_str().unwrap_or_default();
                let p = &event["params"];
                if event.get("id").is_some() && !method.is_empty() {
                    self.send(json!({"id":event["id"],"error":{"code":-32601,"message":"This host does not grant tool approvals."}})).await?;
                    continue;
                }
                if p["threadId"] != thread || p["turnId"].as_str().is_some_and(|id| id != turn_id) { continue; }
                if method == "item/completed" && p["item"]["type"] == "agentMessage" {
                    text = p["item"]["text"].as_str().unwrap_or_default().chars().take(128_000).collect();
                }
                if method == "turn/completed" && p["turn"]["id"] == turn_id {
                    interrupt.complete = true;
                    return Ok::<_, anyhow::Error>(p["turn"]["status"] == "completed" && !text.trim().is_empty());
                }
            }
        }).await??;
        let redactor = crate::security::Redactor::new(&[], Vec::new())?;
        Ok((ok, redactor.redact(&text)))
    }
}
// Protocol config reads include null option fields; TOML overrides must omit them.
fn config_table(values: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    values
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| {
            let value = match value {
                Value::Object(table) => Value::Object(config_table(table)),
                _ => value.clone(),
            };
            (key.clone(), value)
        })
        .collect()
}
struct Interrupt {
    socket: String,
    thread: String,
    turn: Option<String>,
    complete: bool,
}
impl Drop for Interrupt {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        let (socket, thread, turn) = (self.socket.clone(), self.thread.clone(), self.turn.clone());
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                let mut client = Client::socket(&socket, mobile::Access::default()).await?;
                let turn = if let Some(turn) = turn {
                    turn
                } else {
                    let state = client
                        .rpc(
                            "thread/turns/list",
                            json!({"threadId":thread,"limit":1,"itemsView":"notLoaded"}),
                        )
                        .await?;
                    state["data"]
                        .as_array()
                        .and_then(|turns| turns.last())
                        .and_then(|turn| turn["id"].as_str())
                        .ok_or_else(|| anyhow::anyhow!("Codex did not report the current turn."))?
                        .to_owned()
                };
                client
                    .rpc("turn/interrupt", json!({"threadId":thread,"turnId":turn}))
                    .await?;
                Ok::<_, anyhow::Error>(())
            })
            .await;
        });
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    const THREAD: &str = "11111111-1111-4111-8111-111111111111";
    #[tokio::test]
    async fn explicit_stop_waits_for_the_daemons_terminal_turn_notification() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("codex.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let (interrupted, requested) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(message))) = ws.next().await {
                let request: Value = serde_json::from_str(&message).unwrap();
                let method = request["method"].as_str().unwrap();
                let result = match method {
                    "initialized" => continue,
                    "initialize" | "thread/resume" | "turn/interrupt" => json!({}),
                    "thread/turns/list" => {
                        json!({"data":[{"id":"active-turn","status":"inProgress"}]})
                    }
                    _ => panic!("Unexpected {method}"),
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
                if method == "turn/interrupt" {
                    interrupted.send(()).unwrap();
                    finished.await.unwrap();
                    ws.send(Message::Text(json!({"method":"turn/completed","params":{"threadId":THREAD,"turn":{"id":"active-turn","status":"interrupted"}}}).to_string().into())).await.unwrap();
                    break;
                }
            }
        });
        let mut client = Client::socket(socket.to_str().unwrap(), mobile::Access::default())
            .await
            .unwrap();
        let stopping = client.cancel(THREAD);
        tokio::pin!(stopping);
        tokio::select! { result = &mut stopping => panic!("Stopped before the daemon confirmed termination: {result:?}"), _ = requested => {} }
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut stopping)
                .await
                .is_err()
        );
        finish.send(()).unwrap();
        stopping.await.unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    async fn legacy_exec_history_is_forked_once_and_the_mobile_copy_is_reused() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("codex.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let copy = "22222222-2222-4222-8222-222222222222";
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut forks = 0;
            while let Some(Ok(Message::Text(message))) = ws.next().await {
                let request: Value = serde_json::from_str(&message).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "initialized" => continue,
                    "initialize" => json!({}),
                    "thread/resume" => {
                        if request["params"]["threadId"] == THREAD {
                            json!({"thread":{"id":THREAD,"source":"exec"}})
                        } else {
                            json!({"thread":{"id":copy,"source":"vscode"}})
                        }
                    }
                    "thread/fork" => {
                        forks += 1;
                        json!({"thread":{"id":copy,"source":"vscode"}})
                    }
                    method => panic!("Unexpected {method}"),
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            }
            forks
        });
        let mut client = Client::socket(socket.to_str().unwrap(), mobile::Access::default())
            .await
            .unwrap();
        client
            .resume(THREAD, ("synthetic", "low"), &[root.path().into()], false)
            .await
            .unwrap();
        assert_eq!(client.access.session_id.as_deref(), Some(copy));
        client
            .resume(copy, ("synthetic", "low"), &[root.path().into()], false)
            .await
            .unwrap();
        drop(client);
        assert_eq!(server.await.unwrap(), 1);
    }
    #[tokio::test]
    async fn cancelling_a_daemon_execution_interrupts_its_turn() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("codex.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let (ready, started) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(message))) = ws.next().await {
                let request: Value = serde_json::from_str(&message).unwrap();
                let method = request["method"].as_str().unwrap();
                let result = match method {
                    "initialized" => continue,
                    "initialize" => json!({}),
                    "thread/start" => json!({"thread":{"id":THREAD}}),
                    "turn/start" => json!({"turn":{"id":"active-turn"}}),
                    _ => panic!("Unexpected {method}"),
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
                if method == "turn/start" {
                    break;
                }
            }
            ready.send(()).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(message))) = ws.next().await {
                let request: Value = serde_json::from_str(&message).unwrap();
                if request["method"] == "initialized" {
                    continue;
                }
                if request["method"] == "turn/interrupt" {
                    assert_eq!(request["params"]["threadId"], THREAD);
                    assert_eq!(request["params"]["turnId"], "active-turn");
                }
                let result = if request["method"] == "thread/turns/list" {
                    json!({"data":[{"id":"active-turn"}]})
                } else {
                    json!({})
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
                if request["method"] == "turn/interrupt" {
                    return;
                }
            }
            panic!("The active turn was not interrupted");
        });
        let client = Client::socket(socket.to_str().unwrap(), mobile::Access::default())
            .await
            .unwrap();
        let directory = root.path().to_owned();
        let execution = tokio::spawn(async move {
            client
                .execute(
                    ("synthetic", "low"),
                    &[directory],
                    "Synthetic task",
                    None,
                    &|_| Ok(()),
                    std::sync::Arc::new(|_| Ok(())),
                )
                .await
        });
        started.await.unwrap();
        // Let the response assign the current turn before cancellation.
        tokio::task::yield_now().await;
        execution.abort();
        let _ = execution.await;
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn shared_daemon_runs_preserve_limits_and_observe_the_native_thread() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("codex.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(message))) = ws.next().await {
                let request: Value = serde_json::from_str(&message).unwrap();
                let method = request["method"].as_str().unwrap();
                let result = match method {
                    "initialized" => continue,
                    "initialize" => json!({}),
                    "config/read" => {
                        json!({"config":{"mcp_servers":{"third.party":{"command":"hidden","url":null,"tool_timeout_sec":null}}}})
                    }
                    "thread/start" => {
                        assert_eq!(request["params"]["sandbox"], "read-only");
                        assert_eq!(request["params"]["approvalPolicy"], "never");
                        assert_eq!(
                            request["params"]["config"]["mcp_servers"]["third.party"]["enabled"],
                            false
                        );
                        assert_eq!(request["params"]["config"]["features.shell_tool"], false);
                        assert!(
                            request["params"]["config"]["mcp_servers"]["third.party"]
                                .get("tool_timeout_sec")
                                .is_none()
                        );
                        json!({"thread":{"id":THREAD}})
                    }
                    "turn/start" => {
                        assert_eq!(request["params"]["threadId"], THREAD);
                        assert_eq!(request["params"]["effort"], "low");
                        assert!(request["params"]["outputSchema"].is_object());
                        json!({"turn":{"id":"turn-1"}})
                    }
                    _ => panic!("Unexpected method {method}"),
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
                if method == "turn/start" {
                    for event in [
                        json!({"method":"item/completed","params":{"threadId":"unrelated","item":{"type":"agentMessage","text":"Wrong session"}}}),
                        json!({"method":"item/completed","params":{"threadId":THREAD,"turnId":"turn-1","item":{"type":"agentMessage","text":"{\"title\":\"Synthetic\"}"}}}),
                        json!({"method":"turn/completed","params":{"threadId":THREAD,"turn":{"id":"turn-1","status":"completed"}}}),
                    ] {
                        ws.send(Message::Text(event.to_string().into()))
                            .await
                            .unwrap();
                    }
                    break;
                }
            }
        });
        let client = Client::socket(
            socket.to_str().unwrap(),
            mobile::Access {
                status: "connected".into(),
                ..mobile::Access::default()
            },
        )
        .await
        .unwrap();
        let native = std::sync::Mutex::new(String::new());
        let result = client
            .execute(
                ("synthetic", "low"),
                &[root.path().into()],
                "Synthetic request",
                Some(json!({"type":"object"})),
                &|id| {
                    if let Some(id) = id {
                        *native.lock().unwrap() = id.into();
                    }
                    Ok(())
                },
                std::sync::Arc::new(|_| Ok(())),
            )
            .await
            .unwrap();
        assert!(result.0);
        assert_eq!(result.1, "{\"title\":\"Synthetic\"}");
        assert_eq!(*native.lock().unwrap(), THREAD);
        server.await.unwrap();
    }
}
