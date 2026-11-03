//! The host's side of the mirror: what it reads from the profile's stores, how it talks to the
//! Worker and who may run it. The engine's rules are tested in `crate::mirror`.
use super::*;
use crate::app::variables::tests::host as new_host;
use crate::mirror::tests::Worker;
use crate::store::activity::ActivityStore;
use crate::store::notes::NotesStore;
use authority::Transport::{Local as LocalTransport, Machine};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn call(host: &Arc<Host>, transport: authority::Transport, name: &str, args: Value) -> Reply {
    let mut request = control::Request::new(name);
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

fn data_dir(host: &Host) -> PathBuf {
    host.state.settings.config().unwrap().server.data_dir
}

/// A profile with one record in most collections, written through the ordinary stores.
fn filled(host: &Host) {
    let data = data_dir(host);
    std::fs::create_dir_all(&data).unwrap();
    let mut config = host.state.settings.config().unwrap();
    config
        .secrets
        .insert("EMBEDDINGS".into(), "SYNTHETIC_REFERENCE".into());
    host.state.settings.save_config(&config).unwrap();
    let index = Index::open(&data.join(index::FILE)).unwrap();
    LocalNotes::new(&index)
        .save_note(
            serde_json::from_value(json!({
                "title": "Deploy window",
                "text": "Releases go out on Tuesdays at contoso.",
            }))
            .unwrap(),
        )
        .unwrap();
    let store = Store::open(&data.join("assistant.db")).unwrap();
    store.enqueue("chats/c/messages/1").unwrap();
    store
        .record(
            "chats/c/messages/1",
            &crate::state::Audit {
                status: "sent".into(),
                sent: Some("An answer".into()),
                ..Default::default()
            },
        )
        .unwrap();
    store.enqueue("chats/c/messages/2").unwrap();
    let activity = ActivityDatabase::open(&data.join("activity-registration.db")).unwrap();
    LocalActivity::new(&activity)
        .save_day(&crate::tools::tasks::Day {
            day: "2026-11-02".into(),
            ..Default::default()
        })
        .unwrap();
}

#[tokio::test]
async fn the_profiles_finished_records_are_read_and_local_ones_stay() {
    let root = tempfile::tempdir().unwrap();
    let host = new_host(root.path());
    filled(&host);
    let local = HostLocal(&host);
    let settings = local.collect(Collection::Settings).unwrap();
    assert_eq!(
        settings.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["config", "knowledge_map"]
    );
    assert!(settings[0].value.get("secrets").is_none());
    assert!(settings[0].value.get("cloud").is_none());
    assert!(
        !settings[0]
            .value
            .to_string()
            .contains("SYNTHETIC_REFERENCE")
    );
    assert!(settings[1].value.get("organization").is_none());
    assert!(settings[1].value.get("work_item_guides").is_none());
    assert_eq!(local.collect(Collection::Organization).unwrap().len(), 1);
    assert_eq!(local.collect(Collection::Notes).unwrap().len(), 1);
    // Only the finished message: the queued one is the worker's.
    let audits = local.collect(Collection::Audits).unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].id, "chats/c/messages/1");
    assert_eq!(audits[0].value["audit"]["sent"], "An answer");
    assert_eq!(
        local.collect(Collection::ActivitySnapshots).unwrap().len(),
        1
    );
    assert!(
        local
            .collect(Collection::ImplementationRuns)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_profile_mirrors_and_verifies_against_a_worker() {
    let root = tempfile::tempdir().unwrap();
    let host = new_host(root.path());
    filled(&host);
    let worker = Worker::default();
    let ledger = Ledger::open(&ledger_path(&host)).unwrap();
    let content = key::create(&keys_dir(&host)).unwrap();
    let local = HostLocal(&host);
    let engine = Engine {
        local: &local,
        transport: &worker,
        ledger: &ledger,
        content: &content,
        cas: false,
    };
    let flushed = engine.flush(now(), Limits::default()).await.unwrap();
    assert_eq!(flushed.sent, 6, "{flushed:?}");
    assert!(engine.verify().await.unwrap().unwrap().ok);
    // Nothing readable reaches the Worker.
    for stored in worker.records.lock().unwrap().values() {
        let sealed = stored.sealed.as_ref().unwrap();
        for needle in [&b"Tuesdays"[..], b"An answer", b"chats/c", b"2026-11-02"] {
            assert!(!sealed.windows(needle.len()).any(|w| w == needle));
        }
        assert!(!stored.id.contains("chats"));
    }
    // A later answer is pending until the next pass.
    let store = Store::open(&data_dir(&host).join("assistant.db")).unwrap();
    store
        .record(
            "chats/c/messages/2",
            &crate::state::Audit {
                status: "ignored".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(engine.plan(now()).unwrap(), 1);
    let report = engine.verify().await.unwrap().unwrap();
    assert!(report.ok);
    assert_eq!(
        report
            .collections
            .iter()
            .find(|c| c.name == "audits")
            .unwrap()
            .pending,
        1
    );
    assert_eq!(
        engine.flush(now(), Limits::default()).await.unwrap().sent,
        1
    );
    // The status reads the outbox and the key locally.
    let described = describe(&host, &host.state.settings.config().unwrap().cloud);
    assert_eq!(described["key"]["present"], true);
    assert_eq!(described["outbox"]["pending"], 0);
    assert_eq!(described["outbox"]["synced"], 7);
    assert!(
        !described.to_string().contains(&content.account),
        "the content account never leaves the key file"
    );
}

fn http(server: &MockServer) -> Http {
    Http {
        endpoint: server.uri(),
        credentials: Credentials {
            client_id: "synthetic.access".into(),
            client_secret: "synthetic-client-secret".into(),
            token: Some(format!("ptac_{}", "A".repeat(43))),
        },
    }
}

fn wire(id: &str) -> Wire {
    Wire {
        collection: Collection::Notes,
        id: id.into(),
        revision: 2,
        digest: crate::mirror::hex_sha256(b"sealed"),
        schema: 1,
        deleted: false,
        sealed: Some(b"sealed".to_vec()),
        base: None,
    }
}

#[tokio::test]
async fn the_protocol_is_read_strictly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/records"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[
            {"collection":"notes","id":"a","revision":2,"result":"created"},
            {"collection":"notes","id":"b","revision":2,"result":"stale"}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/records/notes"))
        .and(query_param("since", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"records":[
            {"collection":"notes","id":"a","revision":2,"digest":"d","schema":1,"deleted":false,"size":6,"generation":4,"updated_at":1,"sealed":STANDARD.encode(b"sealed")},
            {"collection":"notes","id":"t","revision":3,"digest":"e","schema":1,"deleted":true,"size":0,"generation":5,"updated_at":1,"sealed":null}],
            "generation":5,"more":false})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/records/notes/missing"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error":{"code":"not_found"}})),
        )
        .mount(&server)
        .await;
    let http = http(&server);
    assert_eq!(
        http.write(&[wire("a"), wire("b")], false).await.unwrap(),
        [Outcome::Created, Outcome::Stale]
    );
    // An answer about other records than the ones sent is not an answer.
    assert_eq!(
        http.write(&[wire("x"), wire("b")], false).await,
        Err(Failure::Refused("bad_reply".into()))
    );
    assert_eq!(
        http.write(&[wire("a")], false).await,
        Err(Failure::Refused("bad_reply".into()))
    );
    // Compare-and-swap fields go only when used, so older Workers keep accepting the mirror.
    let based = Wire {
        base: Some(1),
        ..wire("a")
    };
    http.write(&[based, wire("b")], true).await.unwrap();
    let bodies: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "POST")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert!(bodies[0].get("atomic").is_none());
    assert!(bodies[0]["records"][0].get("base_revision").is_none());
    let last = bodies.last().unwrap();
    assert_eq!(last["atomic"], json!(true));
    assert_eq!(last["records"][0]["base_revision"], json!(1));
    assert!(last["records"][1].get("base_revision").is_none());
    let page = http.page(Collection::Notes, 0, 500).await.unwrap();
    assert_eq!(page.records.len(), 2);
    assert_eq!(page.records[0].sealed.as_deref(), Some(&b"sealed"[..]));
    assert!(page.records[1].deleted && page.records[1].sealed.is_none());
    assert_eq!(http.get(Collection::Notes, "missing").await, Ok(None));
}

#[tokio::test]
async fn mirroring_needs_the_owner_a_link_and_a_key() {
    let root = tempfile::tempdir().unwrap();
    let host = new_host(root.path());
    filled(&host);
    for name in ["cloud_mirror", "cloud_verify"] {
        let reply = call(&host, Machine, name, json!({})).await;
        assert_eq!(reply.code, "authority_denied", "{name}");
        assert!(authority::check_shared_operation(&control::Request::new(name)).is_err());
    }
    // The plan needs nothing but the local stores and sends nothing.
    let plan = call(
        &host,
        LocalTransport,
        "cloud_mirror",
        json!({"action":"plan"}),
    )
    .await;
    assert!(plan.ok, "{:?}", plan.message);
    assert_eq!(plan.data["plan"]["collections"]["notes"]["records"], 1);
    assert_eq!(plan.data["key"], false);
    let start = call(
        &host,
        LocalTransport,
        "cloud_mirror",
        json!({"action":"start"}),
    )
    .await;
    assert_eq!(start.code, "not_ready");
    assert!(
        key::load(&keys_dir(&host)).unwrap().is_none(),
        "no key without a link"
    );
    let verify = call(&host, LocalTransport, "cloud_verify", json!({})).await;
    assert_eq!(verify.code, "not_ready");
    let unknown = call(
        &host,
        LocalTransport,
        "cloud_mirror",
        json!({"action":"erase"}),
    )
    .await;
    assert_eq!(unknown.code, "invalid_input");
    let extra = call(
        &host,
        LocalTransport,
        "cloud_mirror",
        json!({"account":"other"}),
    )
    .await;
    assert!(!extra.ok);
    let stop = call(
        &host,
        LocalTransport,
        "cloud_mirror",
        json!({"action":"stop"}),
    )
    .await;
    assert!(stop.ok);
    assert_eq!(
        host.state.settings.config().unwrap().cloud.mode,
        CloudMode::Off
    );
}

#[tokio::test]
async fn the_key_backup_carries_the_content_key_for_the_owner() {
    let root = tempfile::tempdir().unwrap();
    let host = new_host(root.path());
    assert!(
        call(
            &host,
            LocalTransport,
            "variable_create",
            crate::app::variables::tests::create("ONE", "synthetic-value")
        )
        .await
        .ok
    );
    let content = key::create(&keys_dir(&host)).unwrap();
    let file = root.path().join("keys.json");
    let backup = call(
        &host,
        LocalTransport,
        "variable_key_backup",
        json!({"output": file}),
    )
    .await;
    assert!(backup.ok, "{:?}", backup.message);
    assert_eq!(backup.data["cloud_content_key"], true);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["cloud_content"]["account"], content.account);

    let other = tempfile::tempdir().unwrap();
    let fresh = new_host(other.path());
    let restored = call(
        &fresh,
        LocalTransport,
        "variable_key_restore",
        json!({"backup": saved}),
    )
    .await;
    assert!(restored.ok, "{:?}", restored.message);
    assert_eq!(restored.data["cloud_content_key_restored"], true);
    assert_eq!(
        key::load(&keys_dir(&fresh)).unwrap().unwrap().backup(),
        content.backup()
    );
    // A different key already installed is never replaced, and nothing else is restored.
    let third = tempfile::tempdir().unwrap();
    let busy = new_host(third.path());
    key::create(&keys_dir(&busy)).unwrap();
    let refused = call(
        &busy,
        LocalTransport,
        "variable_key_restore",
        json!({"backup": saved}),
    )
    .await;
    assert_eq!(refused.code, "state_conflict");
}
