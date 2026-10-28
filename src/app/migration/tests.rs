//! Credential migration through the host's own operations, with synthetic credentials.
use super::*;
use crate::app::variables::{
    sync_tests::{Pair, pair},
    tests::{call, create, host, id, text},
};
use authority::Transport::{Local, Machine, Window};

const DEEPSEEK: &str = "synthetic-deepseek-9c41e7a2";
const SERVICE: &str = "synthetic-service-token-5d3b80f1";
const STATE_KEY: &str = "synthetic-state-key-aa11bb22cc33";

/// Make the installation's own stores hold synthetic credentials, and configure `SVC_TOKEN`.
fn stock(host: &Arc<Host>, values: &[(&str, &str)]) {
    let path = &host.state.config_path;
    let mut config = read_config(path).unwrap();
    config
        .secrets
        .insert("secret://service".into(), "SVC_TOKEN".into());
    fs::write(path, toml::to_string(&config).unwrap()).unwrap();
    let profile = path.parent().unwrap().to_path_buf();
    let mut stored = STORED.lock().unwrap();
    for (name, value) in values {
        stored.insert((profile.clone(), (*name).into()), (*value).into());
    }
}
fn forget(host: &Arc<Host>, name: &str) {
    let profile = host.state.config_path.parent().unwrap().to_path_buf();
    STORED.lock().unwrap().remove(&(profile, name.into()));
}
fn stocked(host: &Arc<Host>) {
    stock(
        host,
        &[
            ("DEEPSEEK_API_KEY", DEEPSEEK),
            ("SVC_TOKEN", SERVICE),
            ("STATE_ENCRYPTION_KEY", STATE_KEY),
            ("PTA_MACHINE_TOKEN", "synthetic-machine-token-77"),
            ("GRAPH_WEBHOOK_SECRET", "synthetic-webhook-secret-88"),
        ],
    );
}

async fn key(host: &Arc<Host>) -> String {
    let inspected = call(host, Local, "variable_migration_inspect", json!({})).await;
    assert!(inspected.ok, "{:?}", inspected.message);
    inspected.data["destination"]["key"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn migrate(host: &Arc<Host>, destination: &str, items: Value) -> Reply {
    call(
        host,
        Local,
        "variable_migration_run",
        json!({"destination":destination,"items":items}),
    )
    .await
}
fn item(credential: &str) -> Value {
    json!({"credential":credential,"description":format!("Synthetic {credential}")})
}
async fn names(host: &Arc<Host>) -> Vec<String> {
    let listed = call(host, Local, "variable_list", json!({})).await;
    listed.data["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_owned())
        .collect()
}
fn row<'a>(reply: &'a Reply, credential: &str) -> &'a Value {
    reply.data["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["credential"] == credential)
        .unwrap_or_else(|| panic!("{credential} is not listed"))
}
fn leaks(text: &str) {
    for secret in [DEEPSEEK, SERVICE, STATE_KEY, "synthetic-machine-token-77"] {
        assert!(!text.contains(secret), "a value leaked: {secret}");
    }
}

#[tokio::test]
async fn discovery_reports_configured_names_and_exclusions_never_values() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    stocked(&host);
    let inspected = call(&host, Window, "variable_migration_inspect", json!({})).await;
    assert!(inspected.ok, "{:?}", inspected.message);
    leaks(&text(&inspected));
    assert_eq!(row(&inspected, "DEEPSEEK_API_KEY")["status"], "eligible");
    assert_eq!(
        row(&inspected, "DEEPSEEK_API_KEY")["consumer"]["id"],
        "deepseek"
    );
    assert_eq!(row(&inspected, "SVC_TOKEN")["status"], "eligible");
    assert_eq!(row(&inspected, "SVC_TOKEN")["consumer"], Value::Null);
    for (name, why) in [
        ("STATE_ENCRYPTION_KEY", "internal"),
        ("PTA_MACHINE_TOKEN", "machine"),
        ("GRAPH_WEBHOOK_SECRET", "webhook"),
    ] {
        assert_eq!(row(&inspected, name)["status"], "excluded", "{name}");
        assert_eq!(row(&inspected, name)["excluded"], why);
    }
    // Only configured names appear: nothing is scanned from the environment.
    let listed: Vec<&str> = inspected.data["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["credential"].as_str().unwrap())
        .collect();
    assert!(
        listed
            .iter()
            .all(|n| !n.starts_with("PATH") && *n != "HOME")
    );
    assert_eq!(inspected.data["destination"]["mode"], "own");
    assert!(inspected.data["remains_local"].as_array().unwrap().len() >= 3);
    // A machine credential never reaches the installation's own stores.
    let refused = call(&host, Machine, "variable_migration_inspect", json!({})).await;
    assert!(!refused.ok);
}

#[tokio::test]
async fn a_reviewed_selection_is_copied_verified_and_only_then_switched() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    stocked(&host);
    let destination = key(&host).await;
    let reply = migrate(
        &host,
        &destination,
        json!([
            {"credential":"DEEPSEEK_API_KEY","description":"DeepSeek key for the assistant","bind":true},
            item("SVC_TOKEN"),
        ]),
    )
    .await;
    assert!(reply.ok, "{:?} {}", reply.message, text(&reply));
    assert_eq!(reply.data["complete"], true);
    leaks(&text(&reply));
    assert!(
        reply.data["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["stage"] == "complete")
    );
    assert_eq!(names(&host).await, ["DEEPSEEK_API_KEY", "SVC_TOKEN"]);
    // The consumer now uses the account variable; the original value is untouched.
    let consumers = call(&host, Local, "variable_consumer_list", json!({})).await;
    assert_eq!(
        consumers.data["consumers"][0]["binding"]["variable"]["name"],
        "DEEPSEEK_API_KEY"
    );
    let id = reply.data["items"][0]["id"].as_str().unwrap();
    let revealed = call(&host, Window, "variable_reveal", json!({"id":id})).await;
    assert_eq!(revealed.data["value"], DEEPSEEK);
    assert_eq!(read(&host, "DEEPSEEK_API_KEY").unwrap(), DEEPSEEK);
    // The record on disk has no value and the audit names no credential.
    let journal = fs::read_to_string(journal_path(&host, &destination)).unwrap();
    leaks(&journal);
    let audit = call(&host, Local, "variable_audit", json!({})).await;
    leaks(&text(&audit));
    assert!(text(&audit).contains("migrate"));
    // Status shows the durable outcome; inspecting again finds the entries already in place.
    let status = call(&host, Local, "variable_migration_status", json!({})).await;
    assert_eq!(status.data["complete"], true);
    let again = call(&host, Local, "variable_migration_inspect", json!({})).await;
    assert_eq!(row(&again, "SVC_TOKEN")["existing"]["same_entry"], true);
}

#[tokio::test]
async fn the_wrong_destination_or_an_excluded_credential_transfers_nothing() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    stocked(&host);
    let wrong = migrate(&host, &"0".repeat(32), json!([item("SVC_TOKEN")])).await;
    assert_eq!(
        (wrong.code.as_str(), wrong.exit_code),
        ("state_conflict", 6)
    );
    let destination = key(&host).await;
    for excluded in [
        "STATE_ENCRYPTION_KEY",
        "PTA_MACHINE_TOKEN",
        "GRAPH_WEBHOOK_SECRET",
    ] {
        let refused = migrate(&host, &destination, json!([item(excluded)])).await;
        assert_eq!(
            (refused.code.as_str(), refused.exit_code),
            ("invalid_input", 2)
        );
        assert_eq!(refused.data["transferred"], false);
        leaks(&text(&refused));
    }
    // A name that is not configured at all is not read from anywhere.
    let unknown = migrate(&host, &destination, json!([item("HOME")])).await;
    assert!(!unknown.ok);
    // Descriptions are required and checked before anything is read or written.
    let blank = migrate(
        &host,
        &destination,
        json!([{"credential":"SVC_TOKEN","description":""}]),
    )
    .await;
    assert!(!blank.ok);
    assert!(names(&host).await.is_empty());
}

#[tokio::test]
async fn collisions_need_an_explicit_resolution_at_the_reviewed_version() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    stocked(&host);
    let existing = create("SVC_TOKEN", "synthetic-other-value-0123");
    let existing_id = existing["id"].as_str().unwrap().to_owned();
    assert!(call(&host, Local, "variable_create", existing).await.ok);
    let destination = key(&host).await;

    let silent = migrate(&host, &destination, json!([item("SVC_TOKEN")])).await;
    assert_eq!(
        (silent.code.as_str(), silent.exit_code),
        ("state_conflict", 6)
    );
    assert_eq!(silent.data["transferred"], false);

    // A resolution for a version nobody reviewed is refused.
    let stale = migrate(
        &host,
        &destination,
        json!([{"credential":"SVC_TOKEN","description":"Synthetic SVC_TOKEN","existing":{"action":"replace","version":9}}]),
    )
    .await;
    assert_eq!(stale.exit_code, 6);

    // Using the existing entry keeps its description and must hold the same value.
    let differs = migrate(
        &host,
        &destination,
        json!([{"credential":"SVC_TOKEN","description":"Synthetic SVC_TOKEN","bind":false,"existing":{"action":"use","version":1}}]),
    )
    .await;
    assert!(!differs.ok);
    assert_eq!(differs.data["items"][0]["stage"], "verification_failed");
    let unchanged = call(&host, Window, "variable_reveal", json!({"id":existing_id})).await;
    assert_eq!(unchanged.data["value"], "synthetic-other-value-0123");

    // Replacing rotates the value at the reviewed version, keeping the entry.
    let replaced = migrate(
        &host,
        &destination,
        json!([{"credential":"SVC_TOKEN","description":"Synthetic SVC_TOKEN","existing":{"action":"replace","version":1}}]),
    )
    .await;
    assert!(replaced.ok, "{:?} {}", replaced.message, text(&replaced));
    assert_eq!(replaced.data["items"][0]["id"], existing_id);
    assert_eq!(replaced.data["items"][0]["version"], 2);
    let now = call(&host, Window, "variable_reveal", json!({"id":existing_id})).await;
    assert_eq!(now.data["value"], SERVICE);

    // A different name avoids the collision altogether.
    let renamed = migrate(
        &host,
        &destination,
        json!([{"credential":"DEEPSEEK_API_KEY","name":"SVC_TOKEN","description":"Another","bind":true}]),
    )
    .await;
    assert_eq!(renamed.exit_code, 6);
    let renamed = migrate(
        &host,
        &destination,
        json!([{"credential":"DEEPSEEK_API_KEY","name":"DEEPSEEK_MIGRATED","description":"Another","bind":true}]),
    )
    .await;
    assert!(renamed.ok, "{:?}", renamed.message);
}

#[tokio::test]
async fn a_credential_without_a_consumer_is_stored_but_cannot_be_switched() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    stocked(&host);
    let destination = key(&host).await;
    let refused = migrate(
        &host,
        &destination,
        json!([{"credential":"SVC_TOKEN","description":"Synthetic","bind":true}]),
    )
    .await;
    assert_eq!(refused.exit_code, 2);
    assert!(names(&host).await.is_empty());
}

#[tokio::test]
async fn a_missing_key_or_an_unreadable_original_fails_closed_and_leaves_the_setup_usable() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    stocked(&host);
    // The account store exists, then its master key disappears.
    assert!(
        call(
            &host,
            Local,
            "variable_create",
            create("EARLIER", "synthetic-earlier-value")
        )
        .await
        .ok
    );
    let keys = root.path().join("profile").join("keys");
    let backup = root.path().join("keys-away");
    fs::rename(&keys, &backup).unwrap();
    let destination = key(&host).await;
    let failed = migrate(
        &host,
        &destination,
        json!([{"credential":"DEEPSEEK_API_KEY","description":"Key","bind":true}]),
    )
    .await;
    assert!(!failed.ok);
    leaks(&text(&failed));
    assert_ne!(failed.data["items"][0]["stage"], "complete");
    assert_eq!(read(&host, "DEEPSEEK_API_KEY").unwrap(), DEEPSEEK);
    let consumers = call(&host, Local, "variable_consumer_list", json!({})).await;
    assert!(consumers.data["consumers"][0]["binding"].is_null());
    // The key comes back: the same migration resumes and completes.
    fs::rename(&backup, &keys).unwrap();
    let resumed = migrate(
        &host,
        &destination,
        json!([{"credential":"DEEPSEEK_API_KEY","description":"Key","bind":true}]),
    )
    .await;
    assert!(resumed.ok, "{:?} {}", resumed.message, text(&resumed));
    // An original that cannot be read transfers nothing.
    forget(&host, "SVC_TOKEN");
    let gone = migrate(&host, &destination, json!([item("SVC_TOKEN")])).await;
    assert_eq!(gone.exit_code, 2);
}

#[tokio::test]
async fn a_repeated_migration_changes_nothing_further() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    stocked(&host);
    let destination = key(&host).await;
    let first = migrate(&host, &destination, json!([item("SVC_TOKEN")])).await;
    assert!(first.ok);
    let again = migrate(&host, &destination, json!([item("SVC_TOKEN")])).await;
    assert!(again.ok);
    assert_eq!(again.data["items"][0]["version"], 1);
    assert_eq!(names(&host).await, ["SVC_TOKEN"]);
}

fn restrict(pair: &Pair, lose_reply_once: Arc<std::sync::atomic::AtomicBool>) {
    let (target, machine, token) = (
        pair.coordinator.clone(),
        pair.machine.clone(),
        pair.token.clone(),
    );
    pair.native
        .state
        .variables
        .use_transport(move |method, args| {
            let (target, machine, token, lose) = (
                target.clone(),
                machine.clone(),
                token.clone(),
                lose_reply_once.clone(),
            );
            Box::pin(async move {
                let reply = call(
                    &target,
                    Machine,
                    "machine_protocol",
                    json!({"id":machine,"token":token,"action":"call","method":method,"args":args}),
                )
                .await;
                // The write happened, but its reply never arrives.
                if method == "variable_create"
                    && lose.swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    anyhow::bail!("connection reset");
                }
                Ok(serde_json::to_value(reply)?)
            })
        });
}

#[tokio::test]
async fn an_uncertain_write_is_inspected_and_resumed_without_a_duplicate() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    stocked(&pair.native);
    restrict(&pair, Arc::new(std::sync::atomic::AtomicBool::new(true)));
    let destination = key(&pair.native).await;
    assert_eq!(
        call(&pair.native, Local, "variable_migration_inspect", json!({}))
            .await
            .data["destination"]["mode"],
        "connected"
    );
    let first = migrate(&pair.native, &destination, json!([item("SVC_TOKEN")])).await;
    assert!(!first.ok);
    assert_eq!(first.code, "dependency_or_network");
    assert_eq!(first.data["items"][0]["stage"], "uncertain");
    leaks(&text(&first));
    // The coordinator did apply it, once.
    assert_eq!(names(&pair.coordinator).await, ["SVC_TOKEN"]);

    let resumed = migrate(&pair.native, &destination, json!([item("SVC_TOKEN")])).await;
    assert!(resumed.ok, "{:?} {}", resumed.message, text(&resumed));
    assert_eq!(resumed.data["items"][0]["version"], 1);
    assert_eq!(names(&pair.coordinator).await, ["SVC_TOKEN"]);
    // No value or ciphertext stays on the installation.
    assert!(
        !root
            .path()
            .join("native/profile/variables/variables.db")
            .exists()
    );
    leaks(&fs::read_to_string(journal_path(&pair.native, &destination)).unwrap());
}

#[tokio::test]
async fn a_signed_in_installation_switches_a_consumer_only_after_a_live_read_back() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    stocked(&pair.native);
    let destination = key(&pair.native).await;
    let reply = migrate(
        &pair.native,
        &destination,
        json!([{"credential":"DEEPSEEK_API_KEY","description":"DeepSeek key","bind":true}]),
    )
    .await;
    assert!(reply.ok, "{:?} {}", reply.message, text(&reply));
    let consumers = call(&pair.native, Local, "variable_consumer_list", json!({})).await;
    assert_eq!(
        consumers.data["consumers"][0]["binding"]["variable"]["name"],
        "DEEPSEEK_API_KEY"
    );
    // Switching accounts makes the earlier review the wrong destination.
    connect_other(&root.path().join("native/profile"), &pair.machine);
    let wrong = migrate(&pair.native, &destination, json!([item("SVC_TOKEN")])).await;
    assert_eq!(
        (wrong.code.as_str(), wrong.exit_code),
        ("state_conflict", 6)
    );
    let status = call(&pair.native, Local, "variable_migration_status", json!({})).await;
    assert!(status.data["items"].as_array().unwrap().is_empty());
}

fn connect_other(profile: &Path, machine: &str) {
    crate::app::variables::sync_tests::connect(profile, 2002, machine);
}

#[tokio::test]
async fn account_settings_can_be_adopted_only_on_a_signed_in_installation() {
    let root = tempfile::tempdir().unwrap();
    let alone = host(root.path());
    stocked(&alone);
    let destination = key(&alone).await;
    let refused = call(
        &alone,
        Local,
        "variable_migration_run",
        json!({"destination":destination,"items":[item("SVC_TOKEN")],"settings":"adopt_account"}),
    )
    .await;
    assert_eq!(refused.exit_code, 2);
    assert!(names(&alone).await.is_empty());
    let unknown = call(
        &alone,
        Local,
        "variable_migration_run",
        json!({"destination":destination,"items":[item("SVC_TOKEN")],"surprise":true}),
    )
    .await;
    assert_eq!(unknown.exit_code, 2);
    let _ = id();
}
