use super::super::tests::{
    NativeShell, assertion, configured_fixture, fake_host, json_body, session_cookie,
};
use super::*;
use axum::{body::Body, http::Request};
use futures_util::StreamExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

const SSE: &str = "application/json, text/event-stream";

fn message(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn call(id: u64, name: &str, arguments: Value) -> Value {
    message(id, "tools/call", json!({"name":name,"arguments":arguments}))
}
async fn post_raw(
    portal: &Arc<Portal>,
    assertion: Option<&str>,
    extra: &[(&str, &str)],
    body: Vec<u8>,
) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "localhost:38656")
        .header(header::ACCEPT, SSE);
    if !extra.iter().any(|(name, _)| *name == "content-type") {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(assertion) = assertion {
        request = request.header("cf-access-jwt-assertion", assertion);
    }
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    router(portal.clone())
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap()
}
async fn post(
    portal: &Arc<Portal>,
    assertion: Option<&str>,
    extra: &[(&str, &str)],
    body: Value,
) -> Response {
    post_raw(portal, assertion, extra, serde_json::to_vec(&body).unwrap()).await
}
async fn result(portal: &Arc<Portal>, assertion: &str, body: Value) -> Value {
    let response = post(portal, Some(assertion), &[], body).await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}
fn text(reply: &Value) -> String {
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}
async fn host_methods(server: &wiremock::MockServer) -> Vec<(String, String)> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            (
                body["method"].as_str().unwrap().to_owned(),
                request.headers["authorization"]
                    .to_str()
                    .unwrap()
                    .to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn only_a_verified_assertion_of_an_enabled_account_is_admitted_never_a_browser() {
    let (_dir, portal, mut entries, _issuer) = configured_fixture().await;
    let ping = message(1, "ping", json!({}));
    let alice = assertion(&entries[0]);
    assert_eq!(
        post(&portal, Some(&alice), &[], ping.clone())
            .await
            .status(),
        StatusCode::OK
    );
    // No assertion, a wrong audience or issuer, an expired one.
    assert_eq!(
        post(&portal, None, &[], ping.clone()).await.status(),
        StatusCode::UNAUTHORIZED
    );
    for (field, value) in [
        ("aud", json!(["c".repeat(64)])),
        ("iss", json!("https://other.cloudflareaccess.com")),
        ("exp", json!(access::now() - 10)),
    ] {
        let mut claims = access::tests::claims(&entries[0].id);
        claims[field] = value;
        let forged = access::tests::token(&claims, "synthetic-key");
        assert_eq!(
            post(&portal, Some(&forged), &[], ping.clone())
                .await
                .status(),
            StatusCode::UNAUTHORIZED,
            "{field}"
        );
    }
    // A verified identity bound to no account.
    let stranger = access::tests::token(
        &access::tests::claims(&uuid::Uuid::new_v4().to_string()),
        "synthetic-key",
    );
    assert_eq!(
        post(&portal, Some(&stranger), &[], ping.clone())
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // The portal's browser session cookie alone, or beside the assertion; a browser Origin or
    // browser fetch metadata.
    let (session, _) = session_cookie(&portal, &entries[0]).await;
    assert_eq!(
        post(&portal, None, &[("cookie", &session.cookie)], ping.clone())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for extra in [
        ("cookie", session.cookie.as_str()),
        ("origin", "https://localhost:38656"),
        ("origin", "https://attacker.example"),
        ("sec-fetch-site", "same-origin"),
        ("sec-fetch-mode", "cors"),
    ] {
        assert_eq!(
            post(&portal, Some(&alice), &[extra], ping.clone())
                .await
                .status(),
            StatusCode::FORBIDDEN,
            "{extra:?}"
        );
    }
    // A disabled account, and one the operator has not admitted.
    entries[1].enabled = false;
    write_private(
        &portal.root.join("accounts.json"),
        &serde_json::to_string(&entries).unwrap(),
    )
    .unwrap();
    let bob = assertion(&entries[1]);
    assert_eq!(
        post(&portal, Some(&bob), &[], ping.clone()).await.status(),
        StatusCode::FORBIDDEN
    );
    entries[1].enabled = true;
    entries[1].authorized = false;
    write_private(
        &portal.root.join("accounts.json"),
        &serde_json::to_string(&entries).unwrap(),
    )
    .unwrap();
    assert_eq!(
        post(&portal, Some(&bob), &[], ping).await.status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn the_protocol_matches_the_stdio_server_for_the_tools_it_offers() {
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let alice = assertion(&entries[0]);
    for version in protocol::PROTOCOLS {
        let response = post(
            &portal,
            Some(&alice),
            &[],
            message(
                1,
                "initialize",
                json!({"protocolVersion":version,"capabilities":{}}),
            ),
        )
        .await;
        assert!(response.headers().contains_key("mcp-session-id"));
        let reply = json_body(response).await;
        assert_eq!(reply["result"]["protocolVersion"], version);
        let stdio = protocol::initialized(version, protocol::INSTRUCTIONS);
        assert_eq!(reply["result"]["capabilities"], stdio["capabilities"]);
        assert_eq!(reply["result"]["serverInfo"], stdio["serverInfo"]);
        assert!(
            reply["result"]["instructions"]
                .as_str()
                .unwrap()
                .starts_with(protocol::INSTRUCTIONS)
        );
    }
    let fallback = result(
        &portal,
        &alice,
        message(2, "initialize", json!({"protocolVersion":"1999-01-01"})),
    )
    .await;
    assert_eq!(
        fallback["result"]["protocolVersion"],
        protocol::PROTOCOLS[0]
    );
    let listed = result(&portal, &alice, message(3, "tools/list", json!({}))).await;
    let listed = listed["result"]["tools"].as_array().unwrap().clone();
    let offered: Vec<Value> = tools::TOOLS
        .iter()
        .filter(|tool| agent::tool(tool.name) == agent::Remote::Offered)
        .map(protocol::listed)
        .collect();
    assert_eq!(listed, offered);
    for denied in [
        "launcher_run",
        "launcher_define",
        "update_apply",
        "update_stage",
        "cloud_rotate_key",
        "machine_login",
    ] {
        assert!(listed.iter().all(|tool| tool["name"] != denied), "{denied}");
    }
    assert_eq!(
        result(&portal, &alice, message(4, "ping", json!({}))).await["result"],
        json!({})
    );
    let unknown = result(&portal, &alice, message(5, "resources/list", json!({}))).await;
    assert_eq!(unknown["error"]["code"], -32601);
    // Notifications are accepted; batches, other HTTP methods and unknown versions are not.
    let notification = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    assert_eq!(
        post(&portal, Some(&alice), &[], notification)
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        post(
            &portal,
            Some(&alice),
            &[],
            json!([message(6, "ping", json!({}))])
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(
            &portal,
            Some(&alice),
            &[("mcp-protocol-version", "1999-01-01")],
            message(7, "ping", json!({}))
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    let get = router(portal.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/mcp")
                .header(header::HOST, "localhost:38656")
                .header("cf-access-jwt-assertion", &alice)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn each_account_reaches_only_its_own_host_with_the_agent_transport() {
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let (alice_host, _alice_lock) = fake_host(&portal, &entries[0]).await;
    let (bob_host, _bob_lock) = fake_host(&portal, &entries[1]).await;
    let alice = assertion(&entries[0]);
    let bob = assertion(&entries[1]);

    let reply = result(
        &portal,
        &alice,
        call(1, "notes_search", json!({"query":"x"})),
    )
    .await;
    assert_eq!(reply["result"]["isError"], false);
    assert!(text(&reply).contains(&entries[0].username));
    let reply = result(&portal, &bob, call(2, "variable_list", json!({}))).await;
    assert!(text(&reply).contains(&entries[1].username));
    assert_eq!(
        host_methods(&alice_host).await,
        [(
            "notes_list".to_owned(),
            "Bearer synthetic-agent-token".to_owned()
        )]
    );
    assert_eq!(
        host_methods(&bob_host).await,
        [(
            "variable_list".to_owned(),
            "Bearer synthetic-agent-token".to_owned()
        )]
    );
    // The account comes from the identity: an argument cannot select another one.
    for arguments in [
        json!({"account":entries[1].id}),
        json!({"account_id":entries[1].id}),
        json!({"profile":"../bob"}),
        json!({"transport":"window"}),
    ] {
        let reply = result(&portal, &alice, call(3, "status", arguments)).await;
        assert_eq!(reply["error"]["code"], -32602);
    }
    // A denied or unknown tool never reaches a host.
    for name in [
        "launcher_run",
        "update_apply",
        "teams_post",
        "machine_login",
    ] {
        let reply = result(&portal, &alice, call(4, name, json!({}))).await;
        assert_eq!(reply["result"]["isError"], true, "{name}");
    }
    let reply = result(&portal, &alice, call(5, "variable_reveal", json!({}))).await;
    assert_eq!(reply["error"]["code"], -32602);
    assert_eq!(host_methods(&alice_host).await.len(), 1);
    assert_eq!(host_methods(&bob_host).await.len(), 1);
}

#[tokio::test]
async fn confirmations_use_elicitation_when_offered_and_confirmed_true_otherwise() {
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let (alice_host, _alice_lock) = fake_host(&portal, &entries[0]).await;
    let alice = assertion(&entries[0]);
    let bob = assertion(&entries[1]);
    let send = |id, confirmed: Option<bool>| {
        let mut arguments = json!({"resource":"synthetic-resource"});
        if let Some(confirmed) = confirmed {
            arguments["confirmed"] = json!(confirmed);
        }
        call(id, "answer_send", arguments)
    };
    // Without elicitation: nothing runs until the agent says the user approved.
    let reply = result(&portal, &alice, send(1, None)).await;
    assert_eq!(reply["result"]["isError"], true);
    assert!(text(&reply).contains("needs the user's confirmation"));
    assert!(host_methods(&alice_host).await.is_empty());
    let reply = result(&portal, &alice, send(2, Some(true))).await;
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(host_methods(&alice_host).await.len(), 1);

    // With elicitation: the question goes to the client on the call's own event stream.
    let initialized = post(
        &portal,
        Some(&alice),
        &[],
        message(3, "initialize", json!({"capabilities":{"elicitation":{}}})),
    )
    .await;
    let session = initialized.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    // Another account cannot use that session.
    assert_eq!(
        post(
            &portal,
            Some(&bob),
            &[("mcp-session-id", &session)],
            message(4, "ping", json!({}))
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(
            &portal,
            Some(&alice),
            &[("mcp-session-id", "forged.session")],
            message(5, "ping", json!({}))
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let streamed = post(
        &portal,
        Some(&alice),
        &[("mcp-session-id", &session)],
        send(6, None),
    )
    .await;
    assert_eq!(
        streamed.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut events = streamed.into_body().into_data_stream();
    let first = String::from_utf8(events.next().await.unwrap().unwrap().to_vec()).unwrap();
    let asked: Value =
        serde_json::from_str(first.trim().strip_prefix("event: message\ndata: ").unwrap()).unwrap();
    assert_eq!(asked["method"], "elicitation/create");
    let ask = asked["id"].clone();
    let approve =
        json!({"jsonrpc":"2.0","id":ask,"result":{"action":"accept","content":{"confirm":true}}});
    // Only the account that was asked can answer.
    assert_eq!(
        post(&portal, Some(&bob), &[], approve.clone())
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(host_methods(&alice_host).await.len(), 1);
    assert_eq!(
        post(&portal, Some(&alice), &[], approve.clone())
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    let last = String::from_utf8(events.next().await.unwrap().unwrap().to_vec()).unwrap();
    let done: Value =
        serde_json::from_str(last.trim().strip_prefix("event: message\ndata: ").unwrap()).unwrap();
    assert_eq!(done["id"], 6);
    assert_eq!(done["result"]["isError"], false);
    assert_eq!(host_methods(&alice_host).await.len(), 2);
    // The same answer cannot be used twice.
    assert_eq!(
        post(&portal, Some(&alice), &[], approve).await.status(),
        StatusCode::BAD_REQUEST
    );

    // A refusal sooner than a person reads is the client's: the confirmed rule applies, and a
    // call without confirmed: true does nothing.
    let streamed = post(
        &portal,
        Some(&alice),
        &[("mcp-session-id", &session)],
        send(7, None),
    )
    .await;
    let mut events = streamed.into_body().into_data_stream();
    let first = String::from_utf8(events.next().await.unwrap().unwrap().to_vec()).unwrap();
    let asked: Value =
        serde_json::from_str(first.trim().strip_prefix("event: message\ndata: ").unwrap()).unwrap();
    let decline = json!({"jsonrpc":"2.0","id":asked["id"],"result":{"action":"decline"}});
    assert_eq!(
        post(&portal, Some(&alice), &[], decline).await.status(),
        StatusCode::ACCEPTED
    );
    let last = String::from_utf8(events.next().await.unwrap().unwrap().to_vec()).unwrap();
    let done: Value =
        serde_json::from_str(last.trim().strip_prefix("event: message\ndata: ").unwrap()).unwrap();
    assert_eq!(done["result"]["isError"], true);
    assert_eq!(host_methods(&alice_host).await.len(), 2);
}

#[tokio::test]
async fn requests_are_bounded_in_size_and_rate_and_logged_without_content() {
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let alice = assertion(&entries[0]);
    let large = vec![b' '; BODY_LIMIT + 1];
    assert_eq!(
        post_raw(&portal, Some(&alice), &[], large).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let ping = message(1, "ping", json!({}));
    assert_eq!(
        post(
            &portal,
            Some(&alice),
            &[("content-type", "text/plain")],
            ping.clone()
        )
        .await
        .status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    // The refused request above counted against the account's budget too.
    for _ in 0..ACCOUNT_RATE - 1 {
        assert_eq!(
            post(&portal, Some(&alice), &[], ping.clone())
                .await
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        post(&portal, Some(&alice), &[], ping.clone())
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // Another account keeps its own budget.
    assert_eq!(
        post(&portal, Some(&assertion(&entries[1])), &[], ping)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        line("tools/call", "ok", Duration::from_millis(12)),
        "remote_mcp operation=tools/call code=ok ms=12"
    );
    assert_eq!(tool_label("variable_list\ncode=forged"), "unknown_tool");
    assert_eq!(tool_label("variable_list"), "variable_list");
    assert_eq!(method_label("tools/call\nsecret"), "unknown");
}

#[tokio::test(start_paused = true)]
async fn a_timeout_is_reported_and_never_replayed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let dispatch: tools::Dispatch = Arc::new(move |_request| {
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    });
    let args = json!({"machine":uuid::Uuid::new_v4().to_string(),"item":{"provider":"github","url":"https://github.com/synthetic/repo/issues/1"},
        "context":"Synthetic","directories":["/synthetic"],"provider":"codex","model":"synthetic","effort":"low","confirmed":true});
    let (result, code) = execute(dispatch, "implementation_start", &args).await;
    assert_eq!(code, "timeout");
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not repeated")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_implementation_start_goes_once_to_the_account_host_with_the_chosen_computer() {
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let (alice_host, _lock) = fake_host(&portal, &entries[0]).await;
    let alice = assertion(&entries[0]);
    let machine = uuid::Uuid::new_v4().to_string();
    let arguments = json!({"machine":machine,"item":{"provider":"github","url":"https://github.com/synthetic/repo/issues/1"},
        "context":"Synthetic context","directories":["/synthetic/root/repo"],"provider":"codex","model":"synthetic","effort":"low","confirmed":true});
    let reply = result(&portal, &alice, call(1, "implementation_start", arguments)).await;
    assert_eq!(reply["result"]["isError"], false);
    let received = alice_host.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["method"], "implementation_start");
    assert_eq!(body["args"]["machine"], machine);
    assert_eq!(body["args"]["directories"], json!(["/synthetic/root/repo"]));
    assert_eq!(
        received[0].headers["authorization"],
        "Bearer synthetic-agent-token"
    );
}

/// The real hosts of two accounts, reached through the catalog with the agent's authority.
#[tokio::test]
async fn two_accounts_cannot_see_or_change_each_others_data_and_no_value_leaves() {
    use authority::Transport::{Agent, Browser, Machine};
    const SECRET: &str = "synthetic-alice-secret-9c41d2e7";
    let (_dir, portal, entries, _issuer) = configured_fixture().await;
    let hosts: Vec<_> = entries
        .iter()
        .map(|account| {
            let profile = profile_dir(&portal.root, account);
            Arc::new(Host {
                state: init_state(&profile, &profile.join("data")).unwrap(),
                shell: Box::new(NativeShell),
            })
        })
        .collect();
    async fn invoke(
        host: &Arc<Host>,
        transport: authority::Transport,
        method: &str,
        args: Value,
    ) -> control::Reply {
        let mut request = control::Request::new(method);
        request.args = args;
        authority::within(
            authority::Invocation {
                scope: host.state.authority.clone(),
                transport,
            },
            control::dispatch(host, request),
        )
        .await
    }
    let agent = |host: &Arc<Host>| -> tools::Dispatch {
        let host = host.clone();
        Arc::new(move |request: control::Request| {
            let host = host.clone();
            Box::pin(async move {
                Ok(authority::within(
                    authority::Invocation {
                        scope: host.state.authority.clone(),
                        transport: Agent,
                    },
                    control::dispatch(&host, request),
                )
                .await)
            })
        })
    };
    let (alice, bob) = (&hosts[0], &hosts[1]);
    let tool = |host: &Arc<Host>, name: &'static str, args: Value| {
        let dispatch = agent(host);
        async move { tools::run_with(dispatch, name, &args).await }
    };

    // Variables: metadata only, per account; an agent asks, the user types the value.
    let created = invoke(
        alice,
        Browser,
        "variable_create",
        json!({"id":uuid::Uuid::new_v4().to_string(),"name":"ALICE_ONLY","description":"Synthetic","value":SECRET}),
    )
    .await;
    assert!(created.ok, "{:?}", created.message);
    let mine = tool(alice, "variable_list", json!({})).await.unwrap();
    assert!(mine.to_string().contains("ALICE_ONLY"));
    assert!(!mine.to_string().contains(SECRET));
    let theirs = tool(bob, "variable_search", json!({"query":"ALICE"}))
        .await
        .unwrap();
    assert!(!theirs.to_string().contains("ALICE_ONLY"));
    for (method, args) in [
        (
            "variable_create",
            json!({"id":uuid::Uuid::new_v4().to_string(),"name":"FROM_AGENT","description":"x","value":"synthetic-agent-value"}),
        ),
        (
            "variable_reveal",
            json!({"id":created.data["variable"]["id"]}),
        ),
        ("variable_launcher_approve", json!({})),
        ("variable_delivery_approve", json!({})),
        ("variable_key_backup", json!({})),
        ("crate_update_apply", json!({})),
    ] {
        let reply = invoke(alice, Agent, method, args).await;
        assert!(!reply.ok, "{method}");
        assert!(!serde_json::to_string(&reply).unwrap().contains(SECRET));
    }
    let entry = tool(
        alice,
        "variable_entry_request",
        json!({"name":"ALICE_PENDING","description":"Typed by the user"}),
    )
    .await
    .unwrap();
    let request = entry["request"].as_str().unwrap().to_owned();
    // Another account's request is unknown to it: neither its name nor its state.
    let foreign = tool(bob, "variable_entry_status", json!({"request":request})).await;
    assert!(
        foreign
            .as_ref()
            .map_or(true, |status| !status.to_string().contains("ALICE_PENDING")),
        "{foreign:?}"
    );
    let completed = invoke(
        alice,
        Browser,
        "variable_entry_complete",
        json!({"request":request,"value":"synthetic-typed-value"}),
    )
    .await;
    assert!(completed.ok, "{:?}", completed.message);
    let status = tool(alice, "variable_entry_status", json!({"request":request}))
        .await
        .unwrap();
    assert!(!status.to_string().contains("synthetic-typed-value"));
    let id = created.data["variable"]["id"].as_str().unwrap().to_owned();
    assert!(
        tool(alice, "variable_rotation_request", json!({"id":id}))
            .await
            .is_ok()
    );
    assert!(
        tool(bob, "variable_rotation_request", json!({"id":id}))
            .await
            .is_err()
    );

    // Notes and records.
    let saved = tool(
        alice,
        "note_save",
        json!({"note":{"title":"Deploy day","text":"Alice deploys on Fridays."}}),
    )
    .await
    .unwrap();
    let note = saved.to_string();
    let found = tool(alice, "notes_search", json!({"query":"Fridays"}))
        .await
        .unwrap();
    assert!(found.to_string().contains("Deploy day"), "{note}");
    let other = tool(bob, "notes_search", json!({"query":"Fridays"}))
        .await
        .unwrap();
    assert!(!other.to_string().contains("Deploy day"));
    if let Some(id) = found
        .as_array()
        .or(found["notes"].as_array())
        .and_then(|notes| notes.first())
        .and_then(|note| note["id"].as_str())
    {
        let _ = tool(bob, "note_remove", json!({"id":id})).await;
    }
    assert!(
        tool(alice, "notes_search", json!({"query":"Fridays"}))
            .await
            .unwrap()
            .to_string()
            .contains("Deploy day")
    );
    for name in [
        "messages_list",
        "activity_history",
        "implementation_history",
    ] {
        let mine = tool(alice, name, json!({})).await;
        assert!(mine.is_ok(), "{name}: {mine:?}");
        assert!(tool(bob, name, json!({})).await.is_ok(), "{name}");
    }

    // Machines: an enrolled computer of one account is invisible to the other.
    let machine = uuid::Uuid::new_v4().to_string();
    let registered = invoke(
        alice,
        Machine,
        "machine_register",
        json!({"id":machine,"name":"laptop","os":"macos"}),
    )
    .await;
    assert!(registered.ok, "{:?}", registered.message);
    let listed = tool(alice, "machine_list", json!({})).await.unwrap();
    assert!(listed.to_string().contains(&machine));
    let listed = tool(bob, "machine_list", json!({})).await.unwrap();
    assert!(!listed.to_string().contains(&machine));
    for (name, args) in [
        ("machine_detail", json!({"machine":machine})),
        ("machine_directories", json!({"machine":machine})),
        (
            "machine_update",
            json!({"machine":machine,"name":"Impostor","confirmed":true}),
        ),
        (
            "machine_revoke",
            json!({"machine":machine,"confirmed":true}),
        ),
    ] {
        assert!(tool(bob, name, args).await.is_err(), "{name}");
    }
    let detail = tool(alice, "machine_detail", json!({"machine":machine}))
        .await
        .unwrap();
    assert_eq!(detail["name"], "laptop");
    assert!(
        !detail
            .to_string()
            .contains(registered.data["token"].as_str().unwrap())
    );
    // The host computer's own enrollment and maintenance are not the agent's.
    for method in [
        "machine_save",
        "machine_connect",
        "crate_update_stage",
        "crate_update_unattended",
    ] {
        assert!(
            !invoke(alice, Agent, method, json!({})).await.ok,
            "{method}"
        );
    }
}

/// Synthetic arguments that satisfy a tool's schema.
fn arguments(tool: &tools::Tool) -> Value {
    let schema = (tool.schema)();
    let mut args = json!({});
    for name in schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let property = &schema["properties"][name];
        args[name] = if let Some(first) = property["enum"].as_array().and_then(|v| v.first()) {
            first.clone()
        } else {
            match property["type"].as_str() {
                Some("boolean") => json!(true),
                Some("integer" | "number") => json!(1),
                Some("array") => json!([]),
                Some("object") => {
                    json!({"id":"synthetic","provider":"github","url":"https://github.com/synthetic/repo/issues/1"})
                }
                _ => json!(uuid::Uuid::new_v4().to_string()),
            }
        };
    }
    match tool.name {
        "list_azure_tasks" => json!({"day":"2026-01-05"}),
        "get_azure_hu" | "get_azure_task" => json!({"id":1}),
        "get_github_epic" | "get_github_issue" => json!({"id":1,"project":"synthetic/repo"}),
        "organization_update" => json!({"name":"Synthetic"}),
        "teams_reading_update" => json!({"all_chats":false}),
        "source_authorize" => json!({"id":"synthetic","enabled":false}),
        "assistant_mode" => json!({"mode":"observe"}),
        "assistant_control" => json!({"action":"stop"}),
        _ => args,
    }
}

#[tokio::test]
async fn every_offered_tool_reaches_only_operations_both_allowlists_admit() {
    let reached = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = reached.clone();
    let dispatch: tools::Dispatch = Arc::new(move |request: control::Request| {
        seen.lock().unwrap().push(request.method.clone());
        let data = if request.method == "snapshot" {
            json!({"config":{"activity_registration":{},"policy":{},"message_style":{},"teams_reading":{}},
                "map":{"resources":[{"id":"synthetic"}],"contexts":[],"people":[],"questions":[],"work_item_guides":[],"agent_prompts":[],"organization":{"entries":[]}}})
        } else {
            json!({})
        };
        Box::pin(async move { Ok(control::Reply::success(data)) })
    });
    let mut covered = 0;
    for tool in tools::TOOLS {
        if agent::tool(tool.name) != agent::Remote::Offered {
            continue;
        }
        let args = arguments(tool);
        if tools::validate(tool.name, &args).is_err() {
            continue;
        }
        reached.lock().unwrap().clear();
        let _ = tools::run_with(dispatch.clone(), tool.name, &args).await;
        let methods = reached.lock().unwrap().clone();
        if methods.is_empty() {
            continue; // Refused before any host operation with these synthetic arguments.
        }
        for method in methods {
            assert!(
                agent::check_method(&method).is_ok(),
                "{} reaches {method}, which the agent transport refuses",
                tool.name
            );
            assert!(
                super::super::profiles::PORTAL_METHODS.contains(&method.as_str()),
                "{} reaches {method}, which the portal does not forward",
                tool.name
            );
        }
        covered += 1;
    }
    let offered = tools::TOOLS
        .iter()
        .filter(|tool| agent::tool(tool.name) == agent::Remote::Offered)
        .count();
    assert!(
        covered * 10 >= offered * 9,
        "only {covered} of {offered} offered tools were exercised"
    );
    eprintln!("exercised {covered} of {offered} offered tools");
    // Every operation the transport admits is one the portal forwards.
    for method in agent::METHODS {
        assert!(
            super::super::profiles::PORTAL_METHODS.contains(method),
            "{method}"
        );
    }
}
