use super::*;

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
pub(super) fn endpoint(origin: &str) -> Result<url::Url> {
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
/// One exchange with the coordinator: its whole reply, successful or not.
async fn exchange(connection: &Connection, mut body: Value) -> Result<Value> {
    body["id"] = json!(connection.id);
    let response = client()?
        .post(endpoint(&connection.url)?)
        .bearer_auth(security::secret("PTA_MACHINE_TOKEN")?)
        .json(&body)
        .timeout(Duration::from_secs(
            if body["args"]["method"] == "machine_assistant" {
                110
            } else {
                45
            },
        ))
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "Machine coordinator is unavailable or this machine was disconnected."
    );
    crate::adapters::bounded_json(response, 1_000_000).await
}
async fn remote(connection: &Connection, body: Value) -> Result<Value> {
    let result = exchange(connection, body).await?;
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
                ensure!(
                    result["authoritative"] == true,
                    "Update the coordinator before managing account machines."
                );
                result["this_machine"] = json!(connection.id);
                return Ok(result);
            }
            super::management::list(host).await
        }
        "machine_settings" => {
            let data = state.registry.lock().await;
            let mut result = json!({"id":data.id,"name":data.settings.name,"coordinator":state.connection()?.map(|c| c.url),"coordinator_origin":super::login::retained(&state.root),"roots":data.settings.roots,"teams_machine":data.teams_machine});
            let id = data.id.clone();
            drop(data);
            if let Some(connection) = state.connection()? {
                let detail = remote(
                    &connection,
                    json!({"action":"call","method":"machine_detail","args":{"machine":id}}),
                )
                .await?;
                result["name"] = detail["name"].clone();
            }
            Ok(result)
        }
        "machine_save" => {
            let own = state.registry.lock().await.id.clone();
            super::management::operate(
                host,
                "machine_update",
                &json!({"machine":own,"name":args["name"],"roots":args["roots"]}),
            )
            .await
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
            let token = args["pairing_token"].as_str().filter(|v| v.len() == 44)
                .context("Authorize this machine on your own coordinator with `pta web machines authorize`, then pass its private JSON file to `pta tools call machine_connect` on stdin.")?;
            let mut data = state.registry.lock().await;
            let response=client()?.post(url).bearer_auth(token).json(&json!({"action":"register","id":data.id,"name":data.settings.name,"os":std::env::consts::OS,"installation":super::login::installation(&state.root)?})).send().await?;
            ensure!(
                response.status().is_success(),
                "Could not connect this machine. Use an unexpired, unused pairing file from your own coordinator."
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
            state.reset_roots(&mut data)?;
            host.state.variables.forget_catalog();
            write_private(
                &state.root.join("connection.json"),
                &serde_json::to_string(&connection)?,
            )?;
            Ok(json!({"connected":true,"roots_reset":true}))
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
            host.state.variables.forget_catalog();
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
            let version = super::management::version(host)?;
            let mut data = state.registry.lock().await;
            ensure!(
                id == data.id
                    || data.machines.iter().any(|m| m.id == id
                        && !m.revoked
                        && now() - m.seen < 35
                        && m.account_version == version),
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

/// Invoked only after the portal consumes an operator-issued pairing credential or a
/// browser-approved sign-in whose verifier the initiating installation presented.
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
    let installation = args["installation"].as_str().unwrap_or_default();
    ensure!(
        installation.is_empty()
            || (installation.len() == 64 && installation.bytes().all(|c| c.is_ascii_hexdigit())),
        "Invalid machine registration."
    );
    let login = args["enrollment"] == "login";
    let account_version = authority::invocation(&host.state)?
        .scope
        .account
        .map(|a| a.version)
        .unwrap_or_default();
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
    // Browser sign-in re-enrolls only the same installation. An operator pairing file
    // explicitly authorized this machine ID and may replace it.
    if login {
        ensure!(!installation.is_empty(), "Invalid machine registration.");
        if let Some(existing) = data
            .machines
            .iter()
            .find(|m| m.id == id && m.account_version == account_version)
        {
            ensure!(
                security::constant_eq(&existing.installation, installation),
                "This machine ID is already connected from another installation. Disconnect it there, then sign in again."
            );
        }
    }
    let token = security::random_secret();
    data.machines.retain(|m| m.id != id);
    data.machines.push(Machine {
        id: id.into(),
        name: name.into(),
        os: os.into(),
        seen: now(),
        secret_hash: digest(&token),
        account_version,
        assistant_phase: None,
        installation: installation.into(),
        revoked: false,
        roots: Vec::new(),
        management_support: false,
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
    ensure!(
        authority::invocation(state)?.scope.admitted,
        "Account binding changed."
    );
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
    authority::reject_authority_arguments(args)?;
    let binding = authority::invocation(&host.state)?.scope.account;
    let state = &host.state.implementation;
    ensure!(
        state.connection()?.is_none(),
        "A connected worker cannot serve coordinator requests."
    );
    let id = args["id"].as_str().context("Invalid machine ID.")?;
    let token = args["token"]
        .as_str()
        .context("Missing machine credential.")?;
    {
        let mut data = state.registry.lock().await;
        let machine = data
            .machines
            .iter_mut()
            .find(|m| {
                m.id == id
                    && !m.revoked
                    && m.account_version
                        == binding
                            .as_ref()
                            .map(|a| a.version.as_str())
                            .unwrap_or_default()
                    && security::constant_eq(&m.secret_hash, &digest(token))
            })
            .context("Machine is not registered.")?;
        machine.seen = now();
        if args["action"] == "poll" {
            machine.management_support = args["management_support"] == true;
        }
        if args["action"] == "poll" && machine.management_support {
            let roots: Vec<PathBuf> = serde_json::from_value(args["roots"].clone())?;
            ensure!(
                roots.len() <= 16 && roots.iter().all(|p| p.as_os_str().len() <= 4096),
                "Invalid machine roots."
            );
            machine.roots = roots;
        }
        if args["action"] == "poll" {
            machine.assistant_phase = args
                .get("assistant_phase")
                .map(|phase| serde_json::from_value(phase.clone()))
                .transpose()?;
        }
        state.save(&data)?;
    }
    match args["action"].as_str().unwrap_or_default() {
        "disconnect" => {
            let mut data = state.registry.lock().await;
            super::management::revoke(&mut data, id)?;
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
            ensure!(
                data.machines.iter().any(|m| m.id == id
                    && !m.revoked
                    && security::constant_eq(&m.secret_hash, &digest(token))),
                "Machine is not registered."
            );
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
            "machine_detail" | "machine_update" | "machine_revoke" | "machine_directories" => {
                Box::pin(super::management::operate(
                    host,
                    args["method"].as_str().unwrap(),
                    &args["args"],
                ))
                .await
            }
            "variable_catalog" | "variable_create" | "variable_update" | "variable_delete" => {
                variables::machine_call(
                    host,
                    args["method"].as_str().unwrap_or_default(),
                    &args["args"],
                )
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
/// An account call to the coordinator that keeps its refusal (`code`, `message`) for the caller,
/// or `None` when this installation is not connected to one.
pub(in crate::app) async fn coordinator_reply(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> Result<Option<Value>> {
    match host.state.implementation.connection()? {
        Some(connection) => exchange(
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
    authority::require_account(&host.state)?;
    ensure!(
        [
            "machine_assistant",
            "machine_roots",
            "machine_browse",
            "implementation_catalog",
            "implementation_browse",
            "implementation_launch",
            "implementation_runs",
            "implementation_cancel",
            "implementation_mobile_session"
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
    let version = super::management::version(host)?;
    ensure!(
        state
            .registry
            .lock()
            .await
            .machines
            .iter()
            .any(|m| m.id == machine
                && !m.revoked
                && now() - m.seen < 35
                && m.account_version == version),
        "Execution machine is offline."
    );
    if matches!(
        method,
        "implementation_browse" | "implementation_launch" | "implementation_mobile_session"
    ) {
        ensure!(
            state
                .registry
                .lock()
                .await
                .machines
                .iter()
                .any(|m| m.id == machine && m.management_support),
            "Update this execution machine before browsing or running implementations."
        );
    }
    if method == "machine_assistant" {
        let data = state.registry.lock().await;
        let target = data
            .machines
            .iter()
            .find(|m| m.id == machine && !m.revoked)
            .context("Machine is not registered.")?;
        ensure!(
            matches!(
                target.assistant_phase,
                Some(AssistantPhase::Running | AssistantPhase::Stopped)
            ),
            "Refresh machines: the assistant state is unknown or changing."
        );
        ensure!(
            args["action"] == "stop"
                || (args["action"] == "start" && data.teams_machine == machine),
            "Select this machine for Teams before starting it."
        );
    }
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
    for _ in 0..if method == "machine_assistant" {
        360
    } else {
        160
    } {
        tokio::time::sleep(Duration::from_millis(250)).await;
        authority::require_account(&host.state)?;
        ensure!(
            state
                .registry
                .lock()
                .await
                .machines
                .iter()
                .any(|m| m.id == machine && !m.revoked),
            "Machine has been revoked."
        );
        let mut calls = state.calls.lock().await;
        if let Some(index) = calls.iter().position(|c| c.id == id && c.result.is_some()) {
            let call = calls.remove(index);
            let result = call.result.unwrap();
            ensure!(
                result["ok"] == true,
                "{}",
                if method == "machine_assistant" {
                    "The machine could not change the assistant state. Check its Teams selection, connection and settings."
                } else if matches!(method, "machine_roots" | "machine_browse") {
                    "The machine rejected these directories. Refresh the machine and choose existing allowed directories."
                } else {
                    "The execution machine rejected the operation. Check its directories, installed CLI and running jobs."
                }
            );
            return Ok(result["data"].clone());
        }
    }
    state.calls.lock().await.retain(|c| c.id != id);
    if method == "machine_assistant" {
        anyhow::bail!(
            "The machine did not confirm the assistant state. Refresh machines before trying again."
        );
    }
    if matches!(method, "machine_roots" | "machine_browse") {
        anyhow::bail!(
            "The machine did not confirm the directory operation. Refresh its details before trying again."
        );
    }
    anyhow::bail!("The machine did not reply. Inspect its runs before starting anything again.")
}

pub(in crate::app) async fn schedule(host: Arc<Host>) {
    tokio::join!(poll(host.clone()), guard_teams(host));
}
async fn poll(host: Arc<Host>) {
    let mut results = Vec::new();
    let mut cursor = 0;
    // Assistant startup can wait for Graph; keep presence, leases and status flowing meanwhile.
    let mut assistant_commands = tokio::task::JoinSet::new();
    loop {
        while let Some(Ok(result)) = assistant_commands.try_join_next() {
            results.push(result);
        }
        let state = &host.state.implementation;
        if authority::invocation(&host.state).is_ok_and(|i| i.scope.owner)
            && let Ok(Some(connection)) = state.connection()
        {
            let own = state.registry.lock().await.id.clone();
            // The account's variable catalog follows this machine without delaying presence.
            variables::auto(&host);
            let (next, sessions) = state
                .db
                .changes(cursor)
                .unwrap_or_else(|_| (cursor, Vec::new()));
            let assistant_phase = *host.state.phase.borrow();
            let roots = state.registry.lock().await.settings.roots.clone();
            if let Ok(reply) = remote(
                &connection,
                json!({"action":"poll","id":own,"results":results,"sessions":sessions,"session_cursor":next,"assistant_phase":assistant_phase,"management_support":true,"roots":roots}),
            )
            .await
            {
                if reply["session_cursor"].as_i64()==Some(next) { cursor=next; }
                results.clear();
                if let Some(command) = reply["command"].as_object() {
                    if command["method"] == "machine_assistant" {
                        let host = host.clone();
                        let command = command.clone();
                        assistant_commands.spawn(async move {
                            let result = local(&host, "machine_assistant", &command["args"]).await;
                            let result = match result {
                                Ok(data) => json!({"ok":true,"data":data}),
                                Err(_) => json!({"ok":false}),
                            };
                            json!({"id":command["id"],"result":result})
                        });
                    } else {
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
                    .any(|m| m.id == session.machine && !m.revoked && now() - m.seen < 35);
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
    #[test]
    fn revocation_keeps_the_lease_barrier_even_if_the_same_machine_is_trusted_again() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path()).unwrap();
        let mut data = state.registry.try_lock().unwrap();
        let own = data.id.clone();
        let worker = uuid::Uuid::new_v4().to_string();
        data.machines.push(Machine {
            id: worker.clone(),
            name: "Laptop".into(),
            os: "windows".into(),
            seen: now(),
            secret_hash: "synthetic".into(),
            account_version: String::new(),
            assistant_phase: Some(AssistantPhase::Running),
            installation: String::new(),
            revoked: false,
            roots: vec![],
            management_support: true,
        });
        data.teams_machine = worker.clone();
        assert!(leased(&mut data, &worker));
        super::super::management::revoke(&mut data, &worker).unwrap();
        assert!(!leased(&mut data, &own));
        data.machines[0].revoked = false;
        data.teams_machine = worker.clone();
        assert!(!leased(&mut data, &worker));
        data.teams_lease.as_mut().unwrap().1 = now() - 1;
        assert!(leased(&mut data, &worker));
    }
}
