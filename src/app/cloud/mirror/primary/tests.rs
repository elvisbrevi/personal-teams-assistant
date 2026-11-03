//! Primary mode on the host: configuration edits reach the account store first, a stale or
//! unanswered one saves nothing, and what the store changed comes back into the profile. The
//! engine's rules are tested in `crate::mirror`; here the host's stores and operations.
use super::*;
use crate::app::variables::tests::host as new_host;
use crate::mirror::tests::Worker;
use authority::Transport::Local as LocalTransport;

async fn call(host: &Arc<Host>, name: &str, args: Value) -> Reply {
    let mut request = control::Request::new(name);
    request.args = args;
    authority::within(
        authority::Invocation {
            scope: host.state.authority.clone(),
            transport: LocalTransport,
        },
        control::dispatch(host, request),
    )
    .await
}

/// A profile whose account store is the in-memory `worker`, mirrored and verified, in `mode`.
async fn linked(root: &Path, worker: &Arc<Worker>, content: Option<&key::Content>) -> Arc<Host> {
    let host = new_host(root);
    std::fs::create_dir_all(host.state.settings.config().unwrap().server.data_dir).unwrap();
    *host.state.cloud.memory.lock().unwrap() = Some(worker.clone());
    match content {
        Some(content) => {
            key::import(&keys_dir(&host), &content.backup()).unwrap();
        }
        None => {
            key::create(&keys_dir(&host)).unwrap();
        }
    }
    let mut config = host.state.settings.config().unwrap();
    config.cloud = Cloud {
        endpoint: "https://store.example.com".into(),
        installation: uuid::Uuid::new_v4().to_string(),
        mode: CloudMode::Mirror,
        token_expires_at: 0,
    };
    host.state.settings.save_config(&config).unwrap();
    host
}

fn content(host: &Host) -> key::Content {
    key::load(&keys_dir(host)).unwrap().unwrap()
}

async fn promote(host: &Arc<Host>) -> Reply {
    call(host, "cloud_promote", json!({"action": "promote"})).await
}

fn snapshot(host: &Host) -> (Value, Value) {
    let state = &host.state;
    (
        serde_json::to_value(state.settings.config().unwrap()).unwrap(),
        serde_json::to_value(state.settings.map().unwrap()).unwrap(),
    )
}

async fn save_greeting(host: &Arc<Host>, greeting: &str) -> Reply {
    let mut config = host.state.settings.config().unwrap();
    config.policy.greeting = greeting.into();
    let map = host.state.settings.map().unwrap();
    call(
        host,
        "save_settings",
        json!({"config": config, "map": map, "tunnel_config": ""}),
    )
    .await
}

#[tokio::test]
async fn records_computed_before_a_commit_are_the_ones_the_stores_export_after_it() {
    let root = tempfile::tempdir().unwrap();
    let host = new_host(root.path());
    let state = &host.state;
    let mut config = state.settings.config().unwrap();
    config.policy.greeting = "en".into();
    config
        .secrets
        .insert("EMBEDDINGS".into(), "SYNTHETIC_REFERENCE".into());
    let mut map = state.settings.map().unwrap();
    // A map without questions gains its initial one when it is read back.
    map.questions = None;
    map.organization.name = "Contoso".into();
    let before = changes(&config, &map).unwrap();
    state.settings.commit(&config, &map, "").unwrap();
    let local = HostLocal(&host);
    let mut after = local.collect(Collection::Settings).unwrap();
    after.extend(local.collect(Collection::Organization).unwrap());
    assert_eq!(before.len(), after.len());
    for (change, exported) in before.iter().zip(&after) {
        assert_eq!(change.id, exported.id);
        assert_eq!(
            change.value.as_ref(),
            Some(&exported.value),
            "{}",
            change.id
        );
    }
    assert!(!format!("{before:?}").contains("SYNTHETIC_REFERENCE"));
}

#[tokio::test]
async fn promoting_needs_a_verified_mirror_and_a_coordinator_free_installation() {
    let root = tempfile::tempdir().unwrap();
    let worker = Arc::new(Worker::default());
    let host = linked(root.path(), &worker, None).await;
    let mut config = host.state.settings.config().unwrap();
    config.cloud.mode = CloudMode::Off;
    host.state.settings.save_config(&config).unwrap();
    assert_eq!(promote(&host).await.code, "not_ready");
    config.cloud.mode = CloudMode::Mirror;
    host.state.settings.save_config(&config).unwrap();
    let promoted = promote(&host).await;
    assert!(promoted.ok, "{:?}", promoted.message);
    assert_eq!(promoted.data["changed"], true);
    assert_eq!(
        host.state.settings.config().unwrap().cloud.mode,
        CloudMode::Primary
    );
    // The copy holds this installation's configuration.
    assert!(!worker.records.lock().unwrap().is_empty());
    let again = promote(&host).await;
    assert_eq!(again.data["changed"], false);
    let status = call(&host, "cloud_status", json!({})).await;
    assert_eq!(status.data["mirror"]["primary"]["conflicts"], 0);
    let demoted = call(&host, "cloud_promote", json!({"action": "demote"})).await;
    assert!(demoted.ok);
    assert_eq!(
        host.state.settings.config().unwrap().cloud.mode,
        CloudMode::Mirror
    );
    assert_eq!(
        call(&host, "cloud_promote", json!({"action": "erase"}))
            .await
            .code,
        "invalid_input"
    );
}

#[tokio::test]
async fn an_edit_reaches_the_account_store_first_and_a_refusal_saves_nothing() {
    let root = tempfile::tempdir().unwrap();
    let worker = Arc::new(Worker::default());
    let host = linked(root.path(), &worker, None).await;
    assert!(promote(&host).await.ok);

    let saved = save_greeting(&host, "en").await;
    assert!(saved.ok, "{:?}", saved.message);
    assert_eq!(host.state.settings.config().unwrap().policy.greeting, "en");
    let verify = call(&host, "cloud_verify", json!({})).await;
    assert_eq!(verify.data["ok"], true, "{}", verify.data);
    let ledger = Ledger::open(&ledger_path(&host)).unwrap();
    assert_eq!(ledger.outbox(None, now()).unwrap().pending, 0);

    // Without an answer the configuration is read-only: nothing is saved or queued.
    let before = snapshot(&host);
    *worker.refuse.lock().unwrap() = Some(Failure::Offline);
    let offline = save_greeting(&host, "pt").await;
    assert_eq!(
        offline.code, "dependency_or_network",
        "{:?}",
        offline.message
    );
    assert_eq!(snapshot(&host), before);
    assert_eq!(ledger.outbox(None, now()).unwrap().pending, 0);

    // A note too: the store accepts it first, with its final ID and times.
    let note = call(
        &host,
        "notes_save",
        json!({"note": {"title": "Deploy window", "text": "Releases go out on Tuesdays."}}),
    )
    .await;
    assert!(note.ok, "{:?}", note.message);
    let id = note.data["id"].as_str().unwrap().to_owned();
    assert!(call(&host, "cloud_verify", json!({})).await.data["ok"] == true);
    let removed = call(&host, "notes_remove", json!({"id": id})).await;
    assert!(removed.ok, "{:?}", removed.message);
    assert!(call(&host, "cloud_verify", json!({})).await.data["ok"] == true);
}

#[tokio::test]
async fn another_installation_receives_the_configuration_and_a_stale_edit_is_a_conflict() {
    let first_root = tempfile::tempdir().unwrap();
    let worker = Arc::new(Worker::default());
    let first = linked(first_root.path(), &worker, None).await;
    let mut config = first.state.settings.config().unwrap();
    config.policy.greeting = "en".into();
    config
        .secrets
        .insert("EMBEDDINGS".into(), "SYNTHETIC_REFERENCE".into());
    let mut map = first.state.settings.map().unwrap();
    map.organization.name = "Contoso".into();
    first.state.settings.commit(&config, &map, "").unwrap();
    assert!(promote(&first).await.ok);

    // A second installation with the same content key already has a configuration of its own:
    // a pull never overwrites it, and adopting the account's needs an explicit replace. Its own
    // secrets, link and data directory stay.
    let second_root = tempfile::tempdir().unwrap();
    let second = linked(second_root.path(), &worker, Some(&content(&first))).await;
    let mut own = second.state.settings.config().unwrap();
    own.cloud.mode = CloudMode::Primary;
    second.state.settings.save_config(&own).unwrap();
    let pulled = call(&second, "cloud_pull", json!({})).await;
    assert!(pulled.ok, "{:?}", pulled.message);
    assert!(pulled.data["conflicts"].as_u64().unwrap() > 0);
    assert_ne!(
        second.state.settings.config().unwrap().policy.greeting,
        "en"
    );
    let pulled = call(&second, "cloud_pull", json!({"replace": true})).await;
    assert!(pulled.ok, "{:?}", pulled.message);
    assert_eq!(pulled.data["conflicts"], 0);
    let theirs = second.state.settings.config().unwrap();
    assert_eq!(theirs.policy.greeting, "en");
    assert!(theirs.secrets.is_empty());
    assert_eq!(theirs.cloud.installation, own.cloud.installation);
    assert_eq!(theirs.server.data_dir, own.server.data_dir);
    assert_eq!(
        second.state.settings.map().unwrap().organization.name,
        "Contoso"
    );

    // The second one edits; the first, still on the older revision, is refused and synchronized.
    assert!(save_greeting(&second, "pt").await.ok);
    let stale = save_greeting(&first, "fr").await;
    assert_eq!(stale.code, "state_conflict", "{:?}", stale.message);
    assert_eq!(first.state.settings.config().unwrap().policy.greeting, "en");
    assert!(call(&first, "cloud_pull", json!({})).await.ok);
    assert_eq!(first.state.settings.config().unwrap().policy.greeting, "pt");
    assert!(save_greeting(&first, "fr").await.ok);
}

#[tokio::test]
async fn a_hand_edit_that_meets_a_newer_store_copy_waits_for_the_user_and_replace_backs_it_up() {
    let first_root = tempfile::tempdir().unwrap();
    let worker = Arc::new(Worker::default());
    let first = linked(first_root.path(), &worker, None).await;
    assert!(promote(&first).await.ok);
    let second_root = tempfile::tempdir().unwrap();
    let second = linked(second_root.path(), &worker, Some(&content(&first))).await;
    let mut own = second.state.settings.config().unwrap();
    own.cloud.mode = CloudMode::Primary;
    second.state.settings.save_config(&own).unwrap();
    assert!(
        call(&second, "cloud_pull", json!({"replace": true}))
            .await
            .ok
    );
    assert!(save_greeting(&second, "pt").await.ok);

    // A change on the first one that never reached the store (a hand edit).
    let mut config = first.state.settings.config().unwrap();
    config.policy.greeting = "de".into();
    first.state.settings.save_config(&config).unwrap();
    let pulled = call(&first, "cloud_pull", json!({})).await;
    assert_eq!(pulled.data["conflicts"], 1, "{}", pulled.data);
    assert_eq!(first.state.settings.config().unwrap().policy.greeting, "de");
    let blocked = save_greeting(&first, "fr").await;
    assert_eq!(blocked.code, "state_conflict");
    let readiness = call(&first, "setup_readiness", json!({})).await;
    let step = readiness.data["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "account_store")
        .cloned()
        .unwrap();
    assert_eq!(step["state"], "attention", "{step}");

    let replaced = call(&first, "cloud_pull", json!({"replace": true})).await;
    assert!(replaced.ok, "{:?}", replaced.message);
    assert_eq!(replaced.data["conflicts"], 0);
    assert_eq!(first.state.settings.config().unwrap().policy.greeting, "pt");
    let backup = replaced.data["backup"].as_str().unwrap();
    assert!(std::fs::read_to_string(backup).unwrap().contains("\"de\""));
    assert!(save_greeting(&first, "fr").await.ok);
    // The ordinary loop pass pulls and uploads without conflicts afterwards.
    assert_eq!(pass(&first).await, Ok(None));
}
