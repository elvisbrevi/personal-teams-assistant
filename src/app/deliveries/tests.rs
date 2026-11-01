//! Deliveries driven through the host's own operations, against a local double of the destination.
use super::*;
use crate::app::variables::{
    sync_tests::pair,
    tests::{SECRET, call, create, host, text},
};
use authority::Transport::{Local, Machine, Window};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

const TOKEN: &str = "synthetic-import-key-81f3";

fn point_at(host: &Arc<Host>, server: &MockServer) {
    *host.state.variables.deliveries.origin.lock().unwrap() = Some(server.uri());
}
async fn stored(host: &Arc<Host>, name: &str, value: &str) -> String {
    let body = create(name, value);
    let reply = call(host, Local, "variable_create", body.clone()).await;
    assert!(reply.ok, "{:?}", reply.message);
    body["id"].as_str().unwrap().to_owned()
}
fn editor(id: &str, value: &str, auth: &str) -> Value {
    json!({
        "id": id,
        "description": "Blog editor: OpenAI key in Settings",
        "target": {
            "kind": "http",
            "url": "https://editor.example.com/api/import/secrets/OPENAI_API_KEY",
            "method": "PUT",
            "headers": {"X-Blog-Editor": "1"},
            "body": {"value": "{{value}}"},
            "auth": {"header": "Authorization", "prefix": "Bearer ", "variable": auth},
            "confirm_field": "/last4",
        },
        "variables": [value],
    })
}
async fn editor_double() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/import/secrets/OPENAI_API_KEY"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(header("x-blog-editor", "1"))
        .and(body_json(json!({"value": SECRET})))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"ok": true, "last4": &SECRET[SECRET.len() - 4..], "echo": SECRET}),
        ))
        .mount(&server)
        .await;
    server
}
fn audit(reply: &Reply) -> Vec<(String, String)> {
    reply.data["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["action"].as_str().unwrap().to_owned(),
                e["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn only_a_person_approves_and_the_host_delivers_without_the_value_leaving() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let server = editor_double().await;
    point_at(&host, &server);
    let value = stored(&host, "BLOG_EDITOR_OPENAI_API_KEY", SECRET).await;
    let auth = stored(&host, "BLOG_EDITOR_IMPORT_KEY", TOKEN).await;
    let other = stored(&host, "UNRELATED_TOKEN", "synthetic-unrelated-0d9a").await;

    // An agent proposes; nothing can be sent before a person approves it at the GUI.
    let proposed = call(
        &host,
        Local,
        "variable_delivery_define",
        editor("blog-editor-openai", &value, &auth),
    )
    .await;
    assert!(proposed.ok, "{:?}", proposed.message);
    assert_eq!(proposed.data["destination"]["approved"], false);
    assert_eq!(
        proposed.data["destination"]["variables"][0]["name"],
        "BLOG_EDITOR_OPENAI_API_KEY"
    );
    let deliver = |variable: &str| json!({"destination":"blog-editor-openai","variable":variable});
    let early = call(&host, Local, "variable_deliver", deliver(&value)).await;
    assert_eq!(early.code, "state_conflict");
    assert!(early.message.unwrap().contains("approval"));
    let digest = proposed.data["destination"]["digest"]
        .as_str()
        .unwrap()
        .to_owned();
    for transport in [Local, Machine] {
        let refused = call(
            &host,
            transport,
            "variable_delivery_approve",
            json!({"id":"blog-editor-openai","digest":digest}),
        )
        .await;
        assert!(!refused.ok, "{transport:?}");
    }
    let wrong = call(
        &host,
        Window,
        "variable_delivery_approve",
        json!({"id":"blog-editor-openai","digest":"0".repeat(64)}),
    )
    .await;
    assert_eq!(wrong.code, "state_conflict");
    let approved = call(
        &host,
        Window,
        "variable_delivery_approve",
        json!({"id":"blog-editor-openai","digest":digest}),
    )
    .await;
    assert!(approved.ok, "{:?}", approved.message);
    assert!(server.received_requests().await.unwrap().is_empty());

    // Only the approved variables go there, and a machine cannot ask.
    let foreign = call(&host, Local, "variable_deliver", deliver(&other)).await;
    assert!(foreign.message.unwrap().contains("not approved to receive"));
    assert!(
        !call(&host, Machine, "variable_deliver", deliver(&value))
            .await
            .ok
    );

    let sent = call(&host, Local, "variable_deliver", deliver(&value)).await;
    assert!(sent.ok, "{:?}", sent.message);
    assert_eq!(sent.data["result"]["status"], "ok");
    assert_eq!(sent.data["result"]["http_status"], 200);
    assert_eq!(sent.data["result"]["confirmed_by_destination"], true);
    assert_eq!(sent.data["variable"]["name"], "BLOG_EDITOR_OPENAI_API_KEY");
    // The value, the destination's credential and its reply never reach the caller.
    for secret in [SECRET, TOKEN, &SECRET[SECRET.len() - 4..]] {
        assert!(!text(&sent).contains(secret), "{secret}");
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    // A repeat inside a minute is refused before anything is resolved or sent.
    let again = call(&host, Local, "variable_deliver", deliver(&value)).await;
    assert_eq!(again.code, "state_conflict");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    // The list shows the definition and names, never a value; removal stops future deliveries.
    let listed = call(&host, Local, "variable_delivery_list", json!({})).await;
    assert_eq!(listed.data["destinations"][0]["approved"], true);
    assert_eq!(
        listed.data["destinations"][0]["target"]["auth"]["variable"]["name"],
        "BLOG_EDITOR_IMPORT_KEY"
    );
    assert!(!text(&listed).contains(SECRET) && !text(&listed).contains(TOKEN));
    let events = call(&host, Local, "variable_audit", json!({})).await;
    let actions = audit(&events);
    for expected in [
        ("delivery_define", "ok"),
        ("delivery_approve", "denied_client"),
        ("delivery_approve", "denied"),
        ("delivery_approve", "ok"),
        ("deliver", "ok"),
        ("deliver", "denied"),
    ] {
        assert!(
            actions.contains(&(expected.0.into(), expected.1.into())),
            "{expected:?} in {actions:?}"
        );
    }
    assert!(!text(&events).contains(SECRET));
    assert!(
        call(
            &host,
            Local,
            "variable_delivery_remove",
            json!({"id":"blog-editor-openai"})
        )
        .await
        .ok
    );
    let gone = call(&host, Local, "variable_deliver", deliver(&value)).await;
    assert_eq!(gone.code, "invalid_input");
}

#[tokio::test]
async fn a_refusing_destination_and_unknown_variables_are_reported_without_the_reply() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(401).set_body_string(SECRET))
        .mount(&server)
        .await;
    point_at(&host, &server);
    let value = stored(&host, "BLOG_EDITOR_OPENAI_API_KEY", SECRET).await;
    let auth = stored(&host, "BLOG_EDITOR_IMPORT_KEY", TOKEN).await;
    let unknown = call(
        &host,
        Local,
        "variable_delivery_define",
        editor("typo", &value, "aaaaaaaa-0000-4000-8000-00000000ffff"),
    )
    .await;
    assert_eq!(unknown.code, "invalid_input");
    let proposed = call(
        &host,
        Local,
        "variable_delivery_define",
        editor("blog-editor-openai", &value, &auth),
    )
    .await;
    let digest = proposed.data["destination"]["digest"].clone();
    assert!(
        call(
            &host,
            Window,
            "variable_delivery_approve",
            json!({"id":"blog-editor-openai","digest":digest})
        )
        .await
        .ok
    );
    let refused = call(
        &host,
        Local,
        "variable_deliver",
        json!({"destination":"blog-editor-openai","variable":value}),
    )
    .await;
    assert!(refused.ok, "{:?}", refused.message);
    assert_eq!(refused.data["result"]["status"], "rejected");
    assert_eq!(refused.data["result"]["http_status"], 401);
    assert_eq!(refused.data["result"]["confirmed_by_destination"], false);
    assert!(!text(&refused).contains(SECRET));
}

#[tokio::test]
async fn a_signed_in_installation_delivers_only_with_a_live_coordinator() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    let server = editor_double().await;
    point_at(&pair.native, &server);
    let value = stored(&pair.native, "BLOG_EDITOR_OPENAI_API_KEY", SECRET).await;
    let auth = stored(&pair.native, "BLOG_EDITOR_IMPORT_KEY", TOKEN).await;
    let proposed = call(
        &pair.native,
        Local,
        "variable_delivery_define",
        editor("blog-editor-openai", &value, &auth),
    )
    .await;
    assert!(proposed.ok, "{:?}", proposed.message);
    let digest = proposed.data["destination"]["digest"].clone();
    assert!(
        call(
            &pair.native,
            Window,
            "variable_delivery_approve",
            json!({"id":"blog-editor-openai","digest":digest})
        )
        .await
        .ok
    );
    let sent = call(
        &pair.native,
        Local,
        "variable_deliver",
        json!({"destination":"blog-editor-openai","variable":value}),
    )
    .await;
    assert!(sent.ok, "{:?}", sent.message);
    assert_eq!(sent.data["result"]["confirmed_by_destination"], true);
    // The value came from the coordinator for this use and was never stored here.
    assert!(!pair.profile.join("variables/variables.db").exists());
    let uses = audit(&call(&pair.coordinator, Local, "variable_audit", json!({})).await);
    assert!(uses.iter().filter(|(a, o)| a == "use" && o == "ok").count() >= 2);

    // Offline: nothing is resolved locally or sent, and the outcome says so.
    pair.native
        .state
        .variables
        .use_transport(|_, _| Box::pin(async { Err(anyhow::anyhow!("connection refused")) }));
    *pair.native.state.variables.deliveries.rate.lock().unwrap() = Rate::default();
    let offline = call(
        &pair.native,
        Local,
        "variable_deliver",
        json!({"destination":"blog-editor-openai","variable":value}),
    )
    .await;
    assert_eq!(offline.code, "dependency_or_network");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn usage_lists_a_destination_as_a_path_once_approved() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let value = stored(&host, "BLOG_EDITOR_OPENAI_API_KEY", SECRET).await;
    let auth = stored(&host, "BLOG_EDITOR_IMPORT_KEY", TOKEN).await;
    let proposed = call(
        &host,
        Local,
        "variable_delivery_define",
        editor("blog-editor-openai", &value, &auth),
    )
    .await;
    let path = |reply: &Reply| {
        reply.data["variables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == value.as_str())
            .unwrap()["paths"][0]
            .clone()
    };
    let before = call(
        &host,
        Local,
        "variable_usage",
        json!({"query":"openai editor"}),
    )
    .await;
    assert_eq!(path(&before)["kind"], "delivery");
    assert_eq!(path(&before)["approved"], false);
    assert_eq!(path(&before)["available_here"], false);
    assert!(
        call(
            &host,
            Window,
            "variable_delivery_approve",
            json!({"id":"blog-editor-openai","digest":proposed.data["destination"]["digest"]})
        )
        .await
        .ok
    );
    let after = call(
        &host,
        Local,
        "variable_usage",
        json!({"query":"openai editor"}),
    )
    .await;
    assert_eq!(path(&after)["operations"], json!(["variable_deliver"]));
    assert_eq!(path(&after)["available_here"], true);
    assert!(!text(&after).contains(SECRET) && !text(&after).contains(TOKEN));
}
