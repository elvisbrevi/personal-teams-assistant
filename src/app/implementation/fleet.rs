use super::*;

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
fn endpoint(origin: &str) -> Result<url::Url> {
    let mut url = url::Url::parse(origin)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "Use the HTTPS origin of your coordinator portal."
    );
    url.set_path("/api/machines");
    Ok(url)
}
async fn remote(connection: &Connection, mut body: Value) -> Result<Value> {
    body["id"] = json!(connection.id);
    let response = client()?
        .post(endpoint(&connection.url)?)
        .bearer_auth(security::secret("PTA_MACHINE_TOKEN")?)
        .json(&body)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "Machine coordinator is unavailable or this machine was disconnected."
    );
    let result: Value = crate::adapters::bounded_json(response, 1_000_000).await?;
    ensure!(
        result["ok"] == true,
        "Machine operation failed. Check the selected machine and its configuration."
    );
    Ok(result["data"].clone())
}
pub(super) async fn operate(host: &Arc<Host>, method: &str, args: &Value) -> Result<Value> {
    let state = &host.state.implementation;
    match method {
        "machine_list" => {
            if let Some(connection) = state.connection()? {
                let mut result = remote(
                    &connection,
                    json!({"action":"call","method":"machine_list","args":{}}),
                )
                .await?;
                result["this_machine"] = json!(connection.id);
                return Ok(result);
            }
            let data = state.registry.lock().await;
            let local = json!({"id":data.id,"name":data.settings.name,"os":std::env::consts::OS,"online":true,"local":true});
            let mut machines = vec![local];
            machines.extend(data.machines.iter().map(|m| json!({"id":m.id,"name":m.name,"os":m.os,"online":now()-m.seen<35,"local":false})));
            Ok(
                json!({"machines":machines,"teams_machine":data.teams_machine,"this_machine":data.id}),
            )
        }
        "machine_settings" => {
            let data = state.registry.lock().await;
            Ok(
                json!({"id":data.id,"name":data.settings.name,"coordinator":state.connection()?.map(|c| c.url),"roots":data.settings.roots,"teams_machine":data.teams_machine}),
            )
        }
        "machine_save" => {
            let settings: Settings = serde_json::from_value(args.clone())?;
            ensure!(
                !settings.name.trim().is_empty()
                    && settings.name.len() <= 100
                    && !settings.name.chars().any(char::is_control),
                "Choose a machine name."
            );
            let config = read_config(&host.state.config_path)?;
            ensure!(
                config.server.webhook_prefix.is_empty()
                    || settings.roots == state.registry.lock().await.settings.roots,
                "The portal operator manages this machine's allowed directories."
            );
            ensure!(
                settings.roots.len() <= 16
                    && settings.roots.iter().all(|p| p.is_absolute() && p.is_dir()),
                "Allowed roots must be existing absolute directories."
            );
            let mut data = state.registry.lock().await;
            data.settings = settings;
            state.save(&data)?;
            Ok(json!({"saved":true}))
        }
        "machine_connect" => {
            ensure!(
                state.connection()?.is_none(),
                "Disconnect this machine before changing coordinators."
            );
            ensure!(
                host.state.running.lock().await.is_none(),
                "Stop the Teams assistant on this machine before connecting it to a coordinator."
            );
            let origin = args["url"].as_str().context("Choose a coordinator URL.")?;
            let url = endpoint(origin)?;
            let _guard = host.state.github_api.lock().await;
            let token = github::access_token().await?;
            let data = state.registry.lock().await;
            let response=client()?.post(url).bearer_auth(token).json(&json!({"action":"register","id":data.id,"name":data.settings.name,"os":std::env::consts::OS})).send().await?;
            ensure!(
                response.status().is_success(),
                "Could not connect this machine. Sign in to the same GitHub account used by the portal."
            );
            let value: Value = crate::adapters::bounded_json(response, 16_000).await?;
            ensure!(value["ok"] == true, "The coordinator refused this machine.");
            security::put_desktop_secret(
                "default",
                "PTA_MACHINE_TOKEN",
                value["data"]["token"]
                    .as_str()
                    .context("Invalid machine credential.")?,
            )?;
            let connection = Connection {
                url: origin.into(),
                account: value["data"]["account"]
                    .as_u64()
                    .context("Invalid machine account.")?,
                id: data.id.clone(),
            };
            write_private(
                &state.root.join("connection.json"),
                &serde_json::to_string(&connection)?,
            )?;
            Ok(json!({"connected":true}))
        }
        "machine_disconnect" => {
            ensure!(
                host.state.running.lock().await.is_none(),
                "Stop the Teams assistant before disconnecting this machine."
            );
            if let Some(connection) = state.connection()? {
                remote(&connection, json!({"action":"disconnect"})).await?;
            }
            if state.root.join("connection.json").exists() {
                fs::remove_file(state.root.join("connection.json"))?;
            }
            let _ = security::delete_desktop_secret("default", "PTA_MACHINE_TOKEN");
            Ok(json!({"disconnected":true}))
        }
        "machine_teams" => {
            if let Some(connection) = state.connection()? {
                return remote(
                    &connection,
                    json!({"action":"call","method":"machine_teams","args":args}),
                )
                .await;
            }
            let id = args["machine"]
                .as_str()
                .context("Choose the Teams machine.")?;
            uuid(id)?;
            let mut data = state.registry.lock().await;
            ensure!(
                id == data.id
                    || data
                        .machines
                        .iter()
                        .any(|m| m.id == id && now() - m.seen < 35),
                "Choose an online machine."
            );
            data.teams_machine = id.into();
            state.save(&data)?;
            Ok(
                json!({"teams_machine":id,"handover_pending":data.teams_lease.as_ref().is_some_and(|(held,expiry)|held!=id && *expiry>now())}),
            )
        }
        "machine_protocol" => protocol(host, args).await,
        _ => anyhow::bail!("Unknown machine operation."),
    }
}

/// Invoked only by the portal after GitHub verified the token against the account binding.
pub(in crate::app) async fn register(host: &Arc<Host>, args: &Value) -> Result<Value> {
    let id = args["id"].as_str().context("Invalid machine ID.")?;
    uuid(id)?;
    let name = args["name"].as_str().unwrap_or_default();
    let os = args["os"].as_str().unwrap_or_default();
    ensure!(
        !name.is_empty()
            && name.len() <= 100
            && !name.chars().any(char::is_control)
            && ["linux", "macos", "windows"].contains(&os),
        "Invalid machine registration."
    );
    let state = &host.state.implementation;
    let mut data = state.registry.lock().await;
    ensure!(
        id != data.id && state.connection()?.is_none(),
        "A coordinator cannot register itself as a remote machine."
    );
    ensure!(
        data.machines.len() < 32 || data.machines.iter().any(|m| m.id == id),
        "Machine limit reached."
    );
    let token = security::random_secret();
    data.machines.retain(|m| m.id != id);
    data.machines.push(Machine {
        id: id.into(),
        name: name.into(),
        os: os.into(),
        seen: now(),
        secret_hash: digest(&token),
        account_version: args["account_version"].as_str().unwrap_or_default().into(),
    });
    state.save(&data)?;
    Ok(json!({"token":token}))
}
fn leased(data: &mut Registry, id: &str) -> bool {
    if data.teams_machine != id {
        return false;
    }
    if data
        .teams_lease
        .as_ref()
        .is_some_and(|(held, expiry)| held != id && *expiry > now())
    {
        return false;
    }
    data.teams_lease = Some((id.into(), now() + 120));
    true
}
pub(in crate::app) async fn teams_allowed(state: &DesktopState) -> Result<bool> {
    let own = state.implementation.registry.lock().await.id.clone();
    if let Some(connection) = state.implementation.connection()? {
        return Ok(
            remote(&connection, json!({"action":"lease","id":own})).await?["allowed"] == true,
        );
    }
    let mut data = state.implementation.registry.lock().await;
    let result = leased(&mut data, &own);
    state.implementation.save(&data)?;
    Ok(result)
}
pub(in crate::app) async fn protocol(host: &Arc<Host>, args: &Value) -> Result<Value> {
    let state = &host.state.implementation;
    let id = args["id"].as_str().context("Invalid machine ID.")?;
    let token = args["token"]
        .as_str()
        .context("Missing machine credential.")?;
    {
        let mut data = state.registry.lock().await;
        let machine = data
            .machines
            .iter_mut()
            .find(|m| m.id == id && security::constant_eq(&m.secret_hash, &digest(token)))
            .context("Machine is not registered.")?;
        machine.seen = now();
    }
    match args["action"].as_str().unwrap_or_default() {
        "disconnect" => {
            let mut data = state.registry.lock().await;
            data.machines.retain(|m| m.id != id);
            if data.teams_machine == id {
                data.teams_machine = data.id.clone();
            }
            state.save(&data)?;
            Ok(json!({"disconnected":true}))
        }
        "poll" => {
            if args.get("sessions").is_some() {
                state
                    .db
                    .cache(id, serde_json::from_value(args["sessions"].clone())?)?;
            }
            let session_cursor = args["session_cursor"].as_i64().unwrap_or(0);
            let mut calls = state.calls.lock().await;
            if let Some(results) = args["results"].as_array() {
                ensure!(results.len() <= 10, "Too many machine results.");
                for result in results {
                    if let Some(call) = calls.iter_mut().find(|c| {
                        c.machine == id && c.id == result["id"] && c.claimed && c.result.is_none()
                    }) {
                        call.result = Some(result["result"].clone());
                    }
                }
            }
            let next = calls.iter_mut().find(|c| c.machine == id && !c.claimed);
            let command = next.map(|c| {
                c.claimed = true;
                json!({"id":c.id,"method":c.method,"args":c.args})
            });
            Ok(json!({"command":command,"session_cursor":session_cursor}))
        }
        "lease" => {
            let mut data = state.registry.lock().await;
            let allowed = leased(&mut data, id);
            state.save(&data)?;
            Ok(json!({"allowed":allowed}))
        }
        "call" => match args["method"].as_str().unwrap_or_default() {
            "implementation_sessions" => Box::pin(session_index(host, &args["args"])).await,
            "implementation_create" | "implementation_creation" => {
                Box::pin(super::operation(
                    host,
                    args["method"].as_str().unwrap(),
                    &args["args"],
                ))
                .await
            }
            "machine_list" | "machine_teams" => {
                Box::pin(operate(
                    host,
                    args["method"].as_str().unwrap(),
                    &args["args"],
                ))
                .await
            }
            "forward" => {
                call(
                    host,
                    args["args"]["machine"].as_str().unwrap_or_default(),
                    args["args"]["method"].as_str().unwrap_or_default(),
                    args["args"]["args"].clone(),
                )
                .await
            }
            _ => anyhow::bail!("Unavailable coordinator operation."),
        },
        _ => anyhow::bail!("Invalid machine action."),
    }
}
pub(super) async fn coordinator(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> Result<Option<Value>> {
    match host.state.implementation.connection()? {
        Some(connection) => remote(
            &connection,
            json!({"action":"call","method":method,"args":args}),
        )
        .await
        .map(Some),
        None => Ok(None),
    }
}
pub(super) async fn call(
    host: &Arc<Host>,
    machine: &str,
    method: &str,
    args: Value,
) -> Result<Value> {
    ensure!(
        [
            "implementation_catalog",
            "implementation_browse",
            "implementation_launch",
            "implementation_runs",
            "implementation_cancel"
        ]
        .contains(&method),
        "Unavailable execution operation."
    );
    let state = &host.state.implementation;
    let own = state.registry.lock().await.id.clone();
    if machine == own {
        return local(host, method, &args).await;
    }
    if let Some(connection) = state.connection()? {
        return remote(&connection,json!({"action":"call","method":"forward","args":{"machine":machine,"method":method,"args":args}})).await;
    }
    ensure!(
        state
            .registry
            .lock()
            .await
            .machines
            .iter()
            .any(|m| m.id == machine && now() - m.seen < 35),
        "Execution machine is offline."
    );
    let id = uuid::Uuid::new_v4().to_string();
    {
        let mut calls = state.calls.lock().await;
        ensure!(calls.len() < 64, "Too many pending machine requests.");
        calls.push(Call {
            id: id.clone(),
            machine: machine.into(),
            method: method.into(),
            args,
            claimed: false,
            result: None,
        });
    }
    for _ in 0..160 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let mut calls = state.calls.lock().await;
        if let Some(index) = calls.iter().position(|c| c.id == id && c.result.is_some()) {
            let call = calls.remove(index);
            let result = call.result.unwrap();
            ensure!(
                result["ok"] == true,
                "The execution machine rejected the operation. Check its directories, installed CLI and running jobs."
            );
            return Ok(result["data"].clone());
        }
    }
    state.calls.lock().await.retain(|c| c.id != id);
    anyhow::bail!("The machine did not reply. Inspect its runs before starting anything again.")
}

pub(in crate::app) async fn schedule(host: Arc<Host>) {
    tokio::join!(poll(host.clone()), guard_teams(host));
}
async fn poll(host: Arc<Host>) {
    let mut results = Vec::new();
    let mut cursor = 0;
    loop {
        let state = &host.state.implementation;
        if let Ok(Some(connection)) = state.connection() {
            let own = state.registry.lock().await.id.clone();
            let (next, sessions) = state
                .db
                .changes(cursor)
                .unwrap_or_else(|_| (cursor, Vec::new()));
            if let Ok(reply) = remote(
                &connection,
                json!({"action":"poll","id":own,"results":results,"sessions":sessions,"session_cursor":next}),
            )
            .await
            {
                if reply["session_cursor"].as_i64()==Some(next) { cursor=next; }
                results.clear();
                if let Some(command) = reply["command"].as_object() {
                    let result = local(
                        &host,
                        command["method"].as_str().unwrap_or_default(),
                        &command["args"],
                    )
                    .await;
                    let value = match result {
                        Ok(data) => json!({"ok":true,"data":data}),
                        Err(_) => json!({"ok":false}),
                    };
                    results.push(json!({"id":command["id"],"result":value}));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
pub(super) async fn session_index(host: &Arc<Host>, args: &Value) -> Result<Value> {
    let urls: Vec<String> = serde_json::from_value(args["urls"].clone())?;
    ensure!(urls.len() <= 100, "Too many work items.");
    for url in &urls {
        crate::tools::todo::validate_link(url)?;
    }
    let state = &host.state.implementation;
    let own = state.db.sessions(&urls)?;
    if let Some(connection) = state.connection()? {
        let result = remote(
            &connection,
            json!({"action":"call","method":"implementation_sessions","args":args}),
        )
        .await;
        return match result {
            Ok(mut result) => {
                let mut sessions = result["sessions"].as_array().cloned().unwrap_or_default();
                for session in own {
                    if !sessions.iter().any(|s| s["id"] == session.id) {
                        let mut value = serde_json::to_value(session)?;
                        value["online"] = json!(true);
                        sessions.push(value);
                    }
                }
                result["sessions"] = json!(sessions);
                Ok(result)
            }
            Err(_) => {
                let id = state.registry.lock().await.id.clone();
                let sessions: Vec<_> = own
                    .into_iter()
                    .map(|session| {
                        let online = session.machine == id;
                        let mut value = serde_json::to_value(session).unwrap_or(Value::Null);
                        value["online"] = json!(online);
                        value
                    })
                    .collect();
                Ok(json!({"sessions":sessions,"partial":true}))
            }
        };
    }
    let data = state.registry.lock().await;
    let sessions = own
        .into_iter()
        .map(|session| {
            let online = session.machine == data.id
                || data
                    .machines
                    .iter()
                    .any(|m| m.id == session.machine && now() - m.seen < 35);
            let mut value = serde_json::to_value(session).unwrap_or(Value::Null);
            value["online"] = json!(online);
            value
        })
        .collect::<Vec<_>>();
    Ok(json!({"sessions":sessions,"partial":false,"this_machine":data.id}))
}
async fn guard_teams(host: Arc<Host>) {
    loop {
        let running = host
            .state
            .running
            .lock()
            .await
            .as_ref()
            .is_some_and(|r| !r.task.is_finished());
        if running && !teams_allowed(&host.state).await.unwrap_or(false) {
            let _guard = host.state.operations.lock().await;
            let _ = super::super::stop(&host.state).await;
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coordinator_urls_cannot_point_to_arbitrary_paths_or_carry_credentials() {
        assert!(endpoint("https://assistant.example.com").is_ok());
        for url in [
            "http://assistant.example.com",
            "https://secret@assistant.example.com",
            "https://assistant.example.com/other",
            "https://assistant.example.com/?key=value",
        ] {
            assert!(endpoint(url).is_err());
        }
    }
    #[test]
    fn a_teams_handover_waits_for_the_old_lease() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path()).unwrap();
        let mut data = state.registry.try_lock().unwrap();
        let first = data.id.clone();
        assert!(leased(&mut data, &first));
        data.teams_machine = "second".into();
        assert!(!leased(&mut data, "second"));
        assert!(!leased(&mut data, &first));
        data.teams_lease = Some((first, now() - 1));
        assert!(leased(&mut data, "second"));
    }
}
