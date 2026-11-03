//! The migration, rollback and erasure on a host whose account store is in memory: a backup
//! always comes first, nothing local is ever deleted, and the copy stays until it is erased.
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

/// A profile connected to the in-memory `worker`, mode off and no key yet: nothing uploaded.
fn connected(root: &Path, worker: &Arc<Worker>) -> Arc<Host> {
    let host = new_host(root);
    std::fs::create_dir_all(host.state.settings.config().unwrap().server.data_dir).unwrap();
    *host.state.cloud.memory.lock().unwrap() = Some(worker.clone());
    let mut config = host.state.settings.config().unwrap();
    config.policy.greeting = "en".into();
    config.cloud = Cloud {
        endpoint: "https://store.example.com".into(),
        installation: uuid::Uuid::new_v4().to_string(),
        mode: CloudMode::Off,
        token_expires_at: 0,
    };
    host.state.settings.save_config(&config).unwrap();
    host
}

fn mode(host: &Host) -> CloudMode {
    host.state.settings.config().unwrap().cloud.mode
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

fn backups(host: &Host) -> Vec<PathBuf> {
    held_backups(&backups_root(host))
}

#[tokio::test]
async fn a_dry_run_reports_what_would_move_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let worker = Arc::new(Worker::default());
    let host = connected(root.path(), &worker);
    let plan = call(&host, "cloud_migrate", json!({"action": "plan"})).await;
    assert!(plan.ok, "{:?}", plan.message);
    assert_eq!(plan.data["mode"], "off");
    assert_eq!(plan.data["key"], false);
    assert!(
        plan.data["plan"]["collections"]["settings"]["records"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        plan.data["stays_local"].as_array().unwrap().len(),
        STAYS_LOCAL.len()
    );
    assert!(plan.data["backup"]["files"].as_u64().unwrap() > 0);
    // Nothing was written: no backup, no key, no ledger, no request.
    assert!(backups(&host).is_empty());
    assert!(key::load(&keys_dir(&host)).unwrap().is_none());
    assert!(!ledger_path(&host).exists());
    assert!(worker.records.lock().unwrap().is_empty());
    assert_eq!(
        call(&host, "cloud_migrate", json!({"action": "erase"}))
            .await
            .code,
        "invalid_input"
    );
}

#[tokio::test]
async fn migrating_backs_up_first_then_uploads_verifies_and_mirrors() {
    let root = tempfile::tempdir().unwrap();
    let worker = Arc::new(Worker::default());
    let host = connected(root.path(), &worker);
    let migrated = call(&host, "cloud_migrate", json!({"action": "start"})).await;
    assert!(migrated.ok, "{:?}", migrated.message);
    assert_eq!(migrated.data["complete"], true);
    assert_eq!(migrated.data["key_created"], true);
    assert_eq!(mode(&host), CloudMode::Mirror);
    assert!(!worker.records.lock().unwrap().is_empty());

    // The backup is private, holds the settings files and says where each came from.
    let path = PathBuf::from(migrated.data["backup"]["path"].as_str().unwrap());
    assert_eq!(backups(&host), vec![path.clone()]);
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(path.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["reason"], "migrate");
    let config = host.state.settings.profile().join("config.toml");
    let saved = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["from"] == json!(config))
        .expect("config.toml is backed up");
    // As it was before the switch to mirror.
    let copy = std::fs::read_to_string(path.join(saved["file"].as_str().unwrap())).unwrap();
    assert_eq!(
        copy,
        std::fs::read_to_string(&config)
            .unwrap()
            .replace("mode = \"mirror\"", "mode = \"off\"")
    );
    assert!(copy.contains("mode = \"off\""));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let status = call(&host, "cloud_status", json!({})).await;
    assert_eq!(status.data["mirror"]["backups"]["count"], 1);
    assert_eq!(
        status.data["mirror"]["backups"]["latest"]["reason"],
        "migrate"
    );

    // Again: a new backup, nothing more to upload, and only the newest three are kept.
    for _ in 0..3 {
        let again = call(&host, "cloud_migrate", json!({"action": "start"})).await;
        assert!(again.ok, "{:?}", again.message);
        assert_eq!(again.data["uploaded"], 0);
    }
    let held = backups(&host);
    assert_eq!(held.len(), BACKUPS_KEPT);
    assert!(!held.contains(&path));

    // The older method is the migration too: its start backs up first.
    let old = call(&host, "cloud_mirror", json!({"action": "start"})).await;
    assert!(old.ok, "{:?}", old.message);
    assert!(old.data["backup"]["path"].is_string());
}

#[tokio::test]
async fn migrating_needs_a_connected_installation_and_backs_up_nothing_otherwise() {
    let root = tempfile::tempdir().unwrap();
    let host = new_host(root.path());
    let refused = call(&host, "cloud_migrate", json!({"action": "start"})).await;
    assert_eq!(refused.code, "not_ready", "{:?}", refused.message);
    assert!(backups(&host).is_empty());
}

#[tokio::test]
async fn rolling_back_from_mirror_keeps_everything_local_and_the_copy_in_the_store() {
    let root = tempfile::tempdir().unwrap();
    let worker = Arc::new(Worker::default());
    let host = connected(root.path(), &worker);
    assert!(
        call(&host, "cloud_migrate", json!({"action": "start"}))
            .await
            .ok
    );
    let held = worker.records.lock().unwrap().len();
    let before = serde_json::to_value(host.state.settings.config().unwrap()).unwrap();

    let rolled = call(&host, "cloud_rollback", json!({})).await;
    assert!(rolled.ok, "{:?}", rolled.message);
    assert_eq!(rolled.data["changed"], true);
    assert_eq!(rolled.data["only_in_store"], 0);
    assert_eq!(rolled.data["verify"]["ok"], true);
    assert_eq!(mode(&host), CloudMode::Off);
    assert_eq!(worker.records.lock().unwrap().len(), held);
    let mut after = serde_json::to_value(host.state.settings.config().unwrap()).unwrap();
    after["cloud"]["mode"] = before["cloud"]["mode"].clone();
    assert_eq!(after, before);
    assert_eq!(backups(&host).len(), 2);
    let status = call(&host, "cloud_status", json!({})).await;
    assert!(status.data["mirror"]["rolled_back_at"].is_number());

    // Off already: nothing to do. Migrating again continues from the copy.
    assert_eq!(
        call(&host, "cloud_rollback", json!({})).await.data["changed"],
        false
    );
    let again = call(&host, "cloud_migrate", json!({"action": "start"})).await;
    assert!(again.ok, "{:?}", again.message);
    assert_eq!(again.data["uploaded"], 0);
    assert_eq!(mode(&host), CloudMode::Mirror);
}

#[tokio::test]
async fn rolling_back_from_primary_brings_the_store_changes_and_refuses_a_conflict() {
    let first_root = tempfile::tempdir().unwrap();
    let worker = Arc::new(Worker::default());
    let first = connected(first_root.path(), &worker);
    assert!(
        call(&first, "cloud_migrate", json!({"action": "start"}))
            .await
            .ok
    );
    let promoted = call(&first, "cloud_promote", json!({"action": "promote"})).await;
    assert!(promoted.ok, "{:?}", promoted.message);
    assert_eq!(promoted.data["backup"]["reason"], "promote");

    // Another installation of the account changes the greeting in the store.
    let second_root = tempfile::tempdir().unwrap();
    let second = connected(second_root.path(), &worker);
    key::import(
        &keys_dir(&second),
        &key::load(&keys_dir(&first)).unwrap().unwrap().backup(),
    )
    .unwrap();
    let mut own = second.state.settings.config().unwrap();
    own.cloud.mode = CloudMode::Primary;
    second.state.settings.save_config(&own).unwrap();
    assert!(
        call(&second, "cloud_pull", json!({"replace": true}))
            .await
            .ok
    );
    assert!(save_greeting(&second, "pt").await.ok);

    // A hand edit on the first one meets it: a conflict, so the rollback changes nothing.
    let mut config = first.state.settings.config().unwrap();
    config.policy.greeting = "de".into();
    first.state.settings.save_config(&config).unwrap();
    let refused = call(&first, "cloud_rollback", json!({})).await;
    assert_eq!(refused.code, "state_conflict", "{:?}", refused.message);
    assert!(refused.data["backup"]["path"].is_string());
    assert_eq!(mode(&first), CloudMode::Primary);
    assert_eq!(first.state.settings.config().unwrap().policy.greeting, "de");

    // Keeping this computer's version: off, the hand edit stays, the store keeps its copy.
    let kept = call(&first, "cloud_rollback", json!({"keep_local": true})).await;
    assert!(kept.ok, "{:?}", kept.message);
    assert_eq!(kept.data["kept_local"], true);
    assert_eq!(mode(&first), CloudMode::Off);
    assert_eq!(first.state.settings.config().unwrap().policy.greeting, "de");
    assert!(!worker.records.lock().unwrap().is_empty());

    // Without a conflict the last pull brings what the store accepted.
    assert!(save_greeting(&second, "fr").await.ok);
    let rolled = call(&second, "cloud_rollback", json!({})).await;
    assert!(rolled.ok, "{:?}", rolled.message);
    assert_eq!(mode(&second), CloudMode::Off);
    assert_eq!(
        second.state.settings.config().unwrap().policy.greeting,
        "fr"
    );
}

#[tokio::test]
async fn erasing_needs_a_rollback_and_then_empties_the_store_but_nothing_local() {
    let root = tempfile::tempdir().unwrap();
    let worker = Arc::new(Worker::default());
    let host = connected(root.path(), &worker);
    assert!(
        call(&host, "cloud_migrate", json!({"action": "start"}))
            .await
            .ok
    );
    let old_key = key::load(&keys_dir(&host)).unwrap().unwrap();

    let refused = call(&host, "cloud_erase", json!({})).await;
    assert_eq!(refused.code, "not_ready", "{:?}", refused.message);
    assert!(!worker.records.lock().unwrap().is_empty());
    assert_eq!(
        call(&host, "cloud_erase", json!({"everything": true}))
            .await
            .code,
        "invalid_input"
    );

    assert!(call(&host, "cloud_rollback", json!({})).await.ok);
    let local_before = HostLocal(&host).collect(Collection::Settings).unwrap();
    let erased = call(&host, "cloud_erase", json!({})).await;
    assert!(erased.ok, "{:?}", erased.message);
    assert!(erased.data["erased"].as_u64().unwrap() > 0);
    assert!(worker.records.lock().unwrap().is_empty());
    assert!(erased.data["recovery"].as_str().unwrap().contains("30-day"));

    // The key is set aside (still private, still readable) and the ledger forgets everything.
    let retired = PathBuf::from(erased.data["key_retired"].as_str().unwrap());
    assert!(retired.exists());
    assert!(key::load(&keys_dir(&host)).unwrap().is_none());
    let ledger = Ledger::open(&ledger_path(&host)).unwrap();
    assert_eq!(ledger.outbox(None, now()).unwrap().synced, 0);
    assert!(ledger.meta("erased_at").unwrap().is_some());
    assert_eq!(mode(&host), CloudMode::Off);
    assert_eq!(
        HostLocal(&host)
            .collect(Collection::Settings)
            .unwrap()
            .len(),
        local_before.len()
    );

    // A new migration uploads everything again under a new key.
    let again = call(&host, "cloud_migrate", json!({"action": "start"})).await;
    assert!(again.ok, "{:?}", again.message);
    assert_eq!(again.data["key_created"], true);
    assert!(again.data["uploaded"].as_u64().unwrap() > 0);
    let new_key = key::load(&keys_dir(&host)).unwrap().unwrap();
    assert_ne!(new_key.backup(), old_key.backup());
    assert_eq!(
        call(&host, "cloud_verify", json!({})).await.data["ok"],
        true
    );
}
