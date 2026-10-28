//! Catalog synchronization between a coordinator and an installation signed in to it, driven
//! through the host's own operations with the coordinator protocol in place of the network.
use super::tests::{SECRET, call, create, host, id, text};
use super::*;
use authority::Transport::{Local, Machine, Window};
use std::sync::atomic::{AtomicUsize, Ordering};

type Tweak = Arc<dyn Fn(&str, &mut Value) + Send + Sync>;

pub(in crate::app) struct Pair {
    pub(in crate::app) coordinator: Arc<Host>,
    pub(in crate::app) native: Arc<Host>,
    pub(in crate::app) machine: String,
    pub(in crate::app) token: String,
    pub(in crate::app) profile: PathBuf,
}

fn link_file(profile: &Path) -> PathBuf {
    profile.join("implementation").join("connection.json")
}
pub(in crate::app) fn connect(profile: &Path, account: u64, machine: &str) {
    fs::create_dir_all(profile.join("implementation")).unwrap();
    fs::write(
        link_file(profile),
        json!({"url":"https://coordinator.example","account":account,"id":machine}).to_string(),
    )
    .unwrap();
}

/// A coordinator and an installation connected to it; calls travel through the real
/// `machine_protocol` of the coordinator, optionally altered on the way back.
pub(in crate::app) async fn pair(root: &Path, tweak: Option<Tweak>) -> Pair {
    let coordinator = host(&root.join("coordinator"));
    let native = host(&root.join("native"));
    let machine = id();
    let registered = call(
        &coordinator,
        Machine,
        "machine_register",
        json!({"id":machine,"name":"Laptop","os":"linux"}),
    )
    .await;
    assert!(registered.ok, "{:?}", registered.message);
    let token = registered.data["token"].as_str().unwrap().to_owned();
    let profile = root.join("native").join("profile");
    connect(&profile, 1001, &machine);
    let (target, id_, token_) = (coordinator.clone(), machine.clone(), token.clone());
    native
        .state
        .variables
        .catalog
        .use_transport(Arc::new(move |method, args| {
            let (target, id_, token_, tweak) =
                (target.clone(), id_.clone(), token_.clone(), tweak.clone());
            Box::pin(async move {
                let reply = call(
                    &target,
                    Machine,
                    "machine_protocol",
                    json!({"id":id_,"token":token_,"action":"call","method":method,"args":args}),
                )
                .await;
                let mut value = serde_json::to_value(reply)?;
                if let Some(tweak) = &tweak {
                    tweak(&method, &mut value);
                }
                Ok(value)
            })
        }));
    Pair {
        coordinator,
        native,
        machine,
        token,
        profile,
    }
}

fn names(reply: &Reply) -> Vec<String> {
    reply.data["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_newly_trusted_machine_restores_its_catalog_and_follows_changes_without_values() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    let a = create("DEPLOY_ENDPOINT", SECRET);
    let b = create("SIGNING_KEY", "second-synthetic-value-9z8y");
    for body in [&a, &b] {
        assert!(
            call(&pair.coordinator, Local, "variable_create", body.clone())
                .await
                .ok
        );
    }
    let status = call(&pair.native, Local, "variable_sync_status", json!({})).await;
    assert_eq!(status.data["sync"]["state"], "never");
    assert_eq!(status.data["sync"]["revision"], Value::Null);

    // The first look restores the names, descriptions and versions.
    let first = call(&pair.native, Local, "variable_list", json!({})).await;
    assert!(first.ok, "{:?}", first.message);
    assert_eq!(names(&first), ["DEPLOY_ENDPOINT", "SIGNING_KEY"]);
    assert_eq!(first.data["sync"]["state"], "synced");
    assert_eq!(first.data["sync"]["revision"], 2);
    assert_eq!(first.data["variables"][0]["version"], 1);
    assert_eq!(
        first.data["variables"][0]["description"],
        "Synthetic DEPLOY_ENDPOINT"
    );

    // Another client edits and deletes: an explicit synchronization brings both.
    let a_id = a["id"].as_str().unwrap();
    let b_id = b["id"].as_str().unwrap();
    assert!(
        call(
            &pair.coordinator,
            Window,
            "variable_update",
            json!({"id":a_id,"version":1,"description":"Edited elsewhere"})
        )
        .await
        .ok
    );
    assert!(
        call(
            &pair.coordinator,
            Window,
            "variable_delete",
            json!({"id":b_id,"version":1})
        )
        .await
        .ok
    );
    let synced = call(&pair.native, Local, "variable_sync", json!({})).await;
    assert!(synced.ok, "{:?}", synced.message);
    assert_eq!(
        synced.data["changes"],
        json!({"added":0,"updated":1,"removed":1})
    );
    let second = call(&pair.native, Local, "variable_list", json!({})).await;
    assert_eq!(names(&second), ["DEPLOY_ENDPOINT"]);
    assert_eq!(
        second.data["variables"][0]["description"],
        "Edited elsewhere"
    );
    assert_eq!(second.data["variables"][0]["version"], 2);
    let authoritative = call(&pair.coordinator, Local, "variable_sync_status", json!({})).await;
    assert_eq!(authoritative.data["sync"]["mode"], "authoritative");
    assert_eq!(
        authoritative.data["sync"]["revision"],
        second.data["sync"]["revision"]
    );
    assert_eq!(second.data["sync"]["revision"], 4);

    // Search works on the mirror, and nothing secret is on this computer or in any reply.
    let found = call(
        &pair.native,
        Local,
        "variable_list",
        json!({"query":"elsewhere"}),
    )
    .await;
    assert_eq!(names(&found), ["DEPLOY_ENDPOINT"]);
    let cache = fs::read_to_string(pair.profile.join("variables/catalog-mirror.json")).unwrap();
    assert!(!cache.contains(SECRET) && !cache.contains("second-synthetic"));
    assert!(!pair.profile.join("variables/variables.db").exists());
    for reply in [&first, &synced, &second, &found] {
        assert!(!text(reply).contains(SECRET) && !text(reply).contains("second-synthetic"));
    }
    // A mirror cannot be revealed from: values stay with the coordinator.
    let reveal = call(&pair.native, Window, "variable_reveal", json!({"id":a_id})).await;
    assert!(!reveal.ok);
    assert!(reveal.message.unwrap().contains("keeps no copy"));
    let status = call(&pair.native, Local, "variable_sync_status", json!({})).await;
    assert_eq!(status.data["sync"]["state"], "synced");
    assert!(status.data["sync"]["synced_at"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn edits_from_a_connected_installation_are_decided_by_the_coordinator() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    let body = create("API_ENDPOINT", SECRET);
    let created = call(&pair.native, Local, "variable_create", body.clone()).await;
    assert!(created.ok, "{:?}", created.message);
    assert_eq!(created.data["variable"]["version"], 1);
    assert_eq!(created.data["sync"]["state"], "synced");
    assert!(!text(&created).contains(SECRET));
    // A repeat after a lost reply is a replay, not a duplicate; the coordinator holds the value.
    let again = call(&pair.native, Local, "variable_create", body.clone()).await;
    assert!(again.ok && again.data["replayed"] == true);
    let entry = body["id"].as_str().unwrap();
    let held = call(
        &pair.coordinator,
        Window,
        "variable_reveal",
        json!({"id":entry}),
    )
    .await;
    assert_eq!(held.data["value"], SECRET);
    assert!(!pair.profile.join("variables/variables.db").exists());

    // The coordinator's other client edits first; the stale edit is a conflict, not an overwrite.
    assert!(
        call(
            &pair.coordinator,
            Window,
            "variable_update",
            json!({"id":entry,"version":1,"description":"Changed first"})
        )
        .await
        .ok
    );
    let stale = call(
        &pair.native,
        Local,
        "variable_update",
        json!({"id":entry,"version":1,"description":"Late edit"}),
    )
    .await;
    assert!(!stale.ok);
    assert_eq!(
        (stale.code.as_str(), stale.exit_code),
        ("state_conflict", 6)
    );
    let list = call(&pair.native, Local, "variable_list", json!({})).await;
    assert_eq!(list.data["variables"][0]["description"], "Changed first");
    assert_eq!(list.data["variables"][0]["version"], 2);
    assert_eq!(list.data["sync"]["state"], "conflict");
    assert!(
        list.data["sync"]["conflict"]
            .as_str()
            .unwrap()
            .contains("changed since")
    );
    // Fresh state, then an explicit resolution.
    let retry = call(
        &pair.native,
        Local,
        "variable_update",
        json!({"id":entry,"version":2,"description":"Resolved"}),
    )
    .await;
    assert!(retry.ok, "{:?}", retry.message);
    assert_eq!(retry.data["sync"]["state"], "synced");

    // A deletion is final for every client, including one that never saw it.
    let deleted = call(
        &pair.native,
        Local,
        "variable_delete",
        json!({"id":entry,"version":3}),
    )
    .await;
    assert!(deleted.ok, "{:?}", deleted.message);
    assert_eq!(deleted.data["deleted"]["version"], 4);
    let revived = call(
        &pair.coordinator,
        Local,
        "variable_create",
        json!({"id":entry,"name":"API_ENDPOINT","description":"Back","value":SECRET}),
    )
    .await;
    assert!(!revived.ok && revived.message.unwrap().contains("was deleted"));
    let late = call(
        &pair.native,
        Local,
        "variable_update",
        json!({"id":entry,"version":3,"description":"Zombie"}),
    )
    .await;
    assert!(!late.ok && late.message.unwrap().contains("was deleted"));
    assert_eq!(
        names(&call(&pair.native, Local, "variable_list", json!({})).await),
        Vec::<String>::new()
    );

    // The coordinator recorded the machine's edits without a value.
    let events = call(&pair.coordinator, Local, "variable_audit", json!({})).await;
    let actions: Vec<_> = events.data["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap().to_owned())
        .collect();
    assert!(actions.contains(&"update".to_owned()) && actions.contains(&"delete".to_owned()));
    assert!(!text(&events).contains(SECRET));
}

#[tokio::test]
async fn protected_entry_on_a_connected_installation_stores_at_the_coordinator() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    let started = call(
        &pair.native,
        Local,
        "variable_entry_start",
        json!({"name":"DEPLOY_TOKEN","description":"Deploys"}),
    )
    .await;
    assert!(started.ok, "{:?}", started.message);
    let request = started.data["request"].as_str().unwrap();
    let done = call(
        &pair.native,
        Window,
        "variable_entry_complete",
        json!({"request":request,"value":SECRET}),
    )
    .await;
    assert!(done.ok, "{:?}", done.message);
    assert!(!text(&done).contains(SECRET));
    let status = call(
        &pair.native,
        Local,
        "variable_entry_status",
        json!({"request":request}),
    )
    .await;
    assert_eq!(status.data["state"], "completed");
    let held = call(
        &pair.coordinator,
        Window,
        "variable_reveal",
        json!({"id":request}),
    )
    .await;
    assert_eq!(held.data["value"], SECRET);
    // The duplicate check uses the synchronized catalog.
    let duplicate = call(
        &pair.native,
        Local,
        "variable_entry_start",
        json!({"name":"deploy_token","description":"Again"}),
    )
    .await;
    assert!(!duplicate.ok && duplicate.message.unwrap().contains("already exists"));
    // A replacement rotates the version the request saw.
    let replace = call(
        &pair.native,
        Local,
        "variable_rotation_start",
        json!({"id":request}),
    )
    .await;
    let replacement = replace.data["request"].as_str().unwrap();
    let rotated = call(
        &pair.native,
        Window,
        "variable_entry_complete",
        json!({"request":replacement,"value":"rotated-synthetic-value-77"}),
    )
    .await;
    assert!(rotated.ok, "{:?}", rotated.message);
    assert_eq!(rotated.data["variable"]["version"], 2);
    let held = call(
        &pair.coordinator,
        Window,
        "variable_reveal",
        json!({"id":request}),
    )
    .await;
    assert_eq!(held.data["value"], "rotated-synthetic-value-77");
}

#[tokio::test]
async fn synchronization_fails_closed_and_keeps_the_last_good_catalog() {
    let root = tempfile::tempdir().unwrap();
    let mode = Arc::new(std::sync::Mutex::new("ok"));
    let seen = mode.clone();
    let tweak: Tweak = Arc::new(move |method, value| {
        if method != "variable_catalog" {
            return;
        }
        match *seen.lock().unwrap() {
            // A coordinator restored from an older backup: one entry, revision 1.
            "older" => {
                let first = value["data"]["items"][0].clone();
                value["data"]["items"] = json!([first]);
                value["data"]["total"] = json!(1);
                value["data"]["revision"] = json!(1);
            }
            "other_account" => value["data"]["source"] = json!("0123456789abcdef0123456789abcdef"),
            "duplicate" => {
                let first = value["data"]["items"][0].clone();
                value["data"]["items"].as_array_mut().unwrap().push(first);
                value["data"]["total"] = json!(2);
            }
            "oversized" => value["data"]["total"] = json!(501),
            "unsupported" => {
                *value = json!({"ok":false,"message":"Unavailable coordinator operation."})
            }
            "malformed_name" => value["data"]["items"][0]["name"] = json!("not a name"),
            "future_schema" => value["data"]["schema"] = json!(9),
            _ => {}
        }
    });
    let pair = pair(root.path(), Some(tweak)).await;
    assert!(
        call(
            &pair.coordinator,
            Local,
            "variable_create",
            create("ALPHA_ENDPOINT", SECRET)
        )
        .await
        .ok
    );
    assert!(
        call(
            &pair.coordinator,
            Local,
            "variable_create",
            create("BETA_ENDPOINT", SECRET)
        )
        .await
        .ok
    );
    let good = call(&pair.native, Local, "variable_sync", json!({})).await;
    assert!(good.ok, "{:?}", good.message);
    let before = fs::read(pair.profile.join("variables/catalog-mirror.json")).unwrap();

    for (name, code) in [
        ("older", "state_conflict"),
        ("other_account", "state_conflict"),
        ("duplicate", "operation_failed"),
        ("oversized", "operation_failed"),
        ("malformed_name", "operation_failed"),
        ("unsupported", "not_ready"),
        ("future_schema", "not_ready"),
    ] {
        *mode.lock().unwrap() = name;
        let reply = call(&pair.native, Local, "variable_sync", json!({})).await;
        assert!(!reply.ok, "{name} was accepted");
        assert_eq!(reply.code, code, "{name}");
        // The last good catalog is untouched and still readable offline.
        assert_eq!(
            fs::read(pair.profile.join("variables/catalog-mirror.json")).unwrap(),
            before,
            "{name}"
        );
        let list = call(&pair.native, Local, "variable_list", json!({})).await;
        assert_eq!(names(&list), ["ALPHA_ENDPOINT", "BETA_ENDPOINT"], "{name}");
        assert_eq!(list.data["sync"]["state"], "error");
        assert!(
            list.data["sync"]["error"]["message"]
                .as_str()
                .unwrap()
                .len()
                > 20
        );
    }
    // Only an explicit reset accepts a catalog older than the one held (a restored coordinator).
    *mode.lock().unwrap() = "older";
    let reset = call(&pair.native, Local, "variable_sync", json!({"reset":true})).await;
    assert!(reset.ok, "{:?}", reset.message);
    assert_eq!(reset.data["sync"]["revision"], 1);
    *mode.lock().unwrap() = "ok";
    let recovered = call(&pair.native, Local, "variable_sync", json!({})).await;
    assert!(recovered.ok);
    assert_eq!(recovered.data["sync"]["state"], "synced");
    assert_eq!(recovered.data["sync"]["revision"], 2);
}

#[tokio::test]
async fn an_unreachable_coordinator_or_a_revoked_machine_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    assert!(
        call(
            &pair.coordinator,
            Local,
            "variable_create",
            create("OFFLINE_CHECK", SECRET)
        )
        .await
        .ok
    );
    assert!(
        call(&pair.native, Local, "variable_sync", json!({}))
            .await
            .ok
    );

    // Network loss: the transport itself fails.
    pair.native
        .state
        .variables
        .catalog
        .use_transport(Arc::new(|_, _| {
            Box::pin(async {
                Err(anyhow::anyhow!(
                    "connection refused to secret.internal.example"
                ))
            })
        }));
    let lost = call(&pair.native, Local, "variable_sync", json!({})).await;
    assert_eq!(
        (lost.code.as_str(), lost.exit_code),
        ("dependency_or_network", 5)
    );
    assert!(!text(&lost).contains("secret.internal"));
    let offline = call(&pair.native, Local, "variable_list", json!({})).await;
    assert_eq!(names(&offline), ["OFFLINE_CHECK"]);
    assert_eq!(offline.data["sync"]["state"], "error");
    // An edit while offline is not queued or retried: it reports that its outcome is unknown.
    let attempt = call(
        &pair.native,
        Local,
        "variable_create",
        create("LATER_ENTRY", SECRET),
    )
    .await;
    assert_eq!(attempt.code, "dependency_or_network");
    assert!(
        attempt
            .message
            .unwrap()
            .contains("may not have been applied")
    );
    assert!(
        call(&pair.coordinator, Local, "variable_list", json!({}))
            .await
            .data["total"]
            == 1
    );

    // Revocation: the coordinator no longer knows the machine.
    let (target, machine, token) = (
        pair.coordinator.clone(),
        pair.machine.clone(),
        pair.token.clone(),
    );
    assert!(
        call(
            &pair.coordinator,
            Window,
            "machine_revoke",
            json!({"machine":pair.machine})
        )
        .await
        .ok
    );
    pair.native.state.variables.catalog.use_transport(Arc::new(move |method, args| {
        let (target, machine, token) = (target.clone(), machine.clone(), token.clone());
        Box::pin(async move {
            Ok(serde_json::to_value(
                call(&target, Machine, "machine_protocol", json!({"id":machine,"token":token,"action":"call","method":method,"args":args})).await,
            )?)
        })
    }));
    let revoked = call(&pair.native, Local, "variable_sync", json!({})).await;
    assert!(!revoked.ok);
    assert_eq!(revoked.code, "operation_failed");
    let edit = call(
        &pair.native,
        Local,
        "variable_create",
        create("AFTER_REVOKE", SECRET),
    )
    .await;
    assert!(!edit.ok);
    assert_eq!(
        call(&pair.coordinator, Local, "variable_list", json!({}))
            .await
            .data["total"],
        1
    );
}

#[tokio::test]
async fn a_large_catalog_is_read_in_bounded_pages_and_a_racing_change_restarts_the_read() {
    let root = tempfile::tempdir().unwrap();
    let pages = Arc::new(AtomicUsize::new(0));
    let page_counter = pages.clone();
    let tweak: Tweak = Arc::new(move |method, _| {
        if method == "variable_catalog" {
            page_counter.fetch_add(1, Ordering::SeqCst);
        }
    });
    let pair = pair(root.path(), Some(tweak)).await;
    for n in 0..130 {
        let reply = call(
            &pair.coordinator,
            Local,
            "variable_create",
            create(&format!("BULK_{n:03}"), SECRET),
        )
        .await;
        assert!(reply.ok, "{:?}", reply.message);
    }
    let synced = call(&pair.native, Local, "variable_sync", json!({})).await;
    assert!(synced.ok, "{:?}", synced.message);
    assert_eq!(synced.data["sync"]["entries"], 130);
    assert_eq!(synced.data["changes"]["added"], 130);
    assert_eq!(
        pages.load(Ordering::SeqCst),
        2,
        "130 entries need two pages of at most 100"
    );
    let list = call(&pair.native, Local, "variable_list", json!({"limit":10})).await;
    assert_eq!(list.data["variables"].as_array().unwrap().len(), 10);
    assert_eq!(list.data["total"], 130);

    // The catalog changes between the two pages of a read: the read restarts and is consistent.
    let target = pair.coordinator.clone();
    let machine = pair.machine.clone();
    let token = pair.token.clone();
    let once = Arc::new(AtomicUsize::new(0));
    let fired = once.clone();
    pair.native.state.variables.catalog.use_transport(Arc::new(move |method, args| {
        let (target, machine, token, fired) = (target.clone(), machine.clone(), token.clone(), fired.clone());
        Box::pin(async move {
            let offset = args["offset"].as_u64().unwrap_or(0);
            if method == "variable_catalog" && offset == 100 && fired.fetch_add(1, Ordering::SeqCst) == 0 {
                assert!(call(&target, Local, "variable_create", create("RACED_ENTRY", SECRET)).await.ok);
            }
            Ok(serde_json::to_value(
                call(&target, Machine, "machine_protocol", json!({"id":machine,"token":token,"action":"call","method":method,"args":args})).await,
            )?)
        })
    }));
    let raced_sync = call(&pair.native, Local, "variable_sync", json!({})).await;
    assert!(raced_sync.ok, "{:?}", raced_sync.message);
    assert_eq!(raced_sync.data["sync"]["entries"], 131);
    assert_eq!(
        raced_sync.data["changes"],
        json!({"added":1,"updated":0,"removed":0})
    );
}

#[tokio::test]
async fn a_different_coordinator_account_or_a_disconnect_never_reuses_the_catalog() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    assert!(
        call(
            &pair.coordinator,
            Local,
            "variable_create",
            create("ACCOUNT_ONE", SECRET)
        )
        .await
        .ok
    );
    assert_eq!(
        call(&pair.native, Local, "variable_sync", json!({}))
            .await
            .data["sync"]["entries"],
        1
    );
    let cache = pair.profile.join("variables/catalog-mirror.json");
    assert!(cache.exists());

    // The same installation now belongs to another account: the old catalog is not shown.
    connect(&pair.profile, 2002, &pair.machine);
    let status = call(&pair.native, Local, "variable_sync_status", json!({})).await;
    assert_eq!(status.data["sync"]["state"], "never");
    assert_eq!(status.data["sync"]["revision"], Value::Null);
    assert!(!cache.exists());

    // Leaving the account clears the mirror and its status.
    connect(&pair.profile, 1001, &pair.machine);
    assert!(
        call(&pair.native, Local, "variable_sync", json!({}))
            .await
            .ok
    );
    assert!(cache.exists());
    pair.native.state.variables.forget_catalog();
    assert!(!cache.exists());
    fs::remove_file(link_file(&pair.profile)).unwrap();
    let alone = call(&pair.native, Local, "variable_sync_status", json!({})).await;
    assert_eq!(alone.data["sync"]["mode"], "authoritative");
    assert_eq!(
        call(&pair.native, Local, "variable_list", json!({}))
            .await
            .data["sync"]["mode"],
        "authoritative"
    );
}

#[tokio::test]
async fn machine_credentials_reach_only_the_catalog_and_versioned_changes() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    let body = create("SCOPED_ENTRY", SECRET);
    assert!(
        call(&pair.coordinator, Local, "variable_create", body.clone())
            .await
            .ok
    );
    let entry = body["id"].as_str().unwrap();
    for (method, args) in [
        ("variable_reveal", json!({"id":entry})),
        ("variable_list", json!({})),
        ("variable_audit", json!({})),
        ("variable_key_backup", json!({"output":"/tmp/stolen.json"})),
        ("variable_consumer_list", json!({})),
        ("variable_launcher_run", json!({"id":"x","confirmed":true})),
        ("variable_sync", json!({})),
    ] {
        let direct = call(&pair.coordinator, Machine, method, args.clone()).await;
        assert!(!direct.ok, "{method} reached a machine directly");
        let proxied = call(
            &pair.coordinator,
            Machine,
            "machine_protocol",
            json!({"id":pair.machine,"token":pair.token,"action":"call","method":method,"args":args}),
        )
        .await;
        assert!(
            !proxied.ok,
            "{method} reached a machine through the protocol"
        );
        assert!(!text(&proxied).contains(SECRET));
    }
    // Account and transport authority are never taken from arguments.
    let forged = call(
        &pair.coordinator,
        Machine,
        "machine_protocol",
        json!({"id":pair.machine,"token":pair.token,"action":"call","method":"variable_catalog","args":{"account":"other"}}),
    )
    .await;
    assert!(!forged.ok);
    // Another installation's credential is not accepted for this machine.
    let impostor = call(
        &pair.coordinator,
        Machine,
        "machine_protocol",
        json!({"id":pair.machine,"token":"wrong-synthetic-credential-0000000000000","action":"call","method":"variable_catalog","args":{}}),
    )
    .await;
    assert!(!impostor.ok);
    // The catalog page itself carries metadata only.
    let page = call(
        &pair.coordinator,
        Machine,
        "machine_protocol",
        json!({"id":pair.machine,"token":pair.token,"action":"call","method":"variable_catalog","args":{"offset":0,"limit":500}}),
    )
    .await;
    assert!(page.ok, "{:?}", page.message);
    assert_eq!(page.data["items"].as_array().unwrap().len(), 1);
    assert!(
        page.data["items"][0].get("value").is_none()
            && page.data["items"][0].get("ciphertext").is_none()
    );
    assert!(!text(&page).contains(SECRET));
}

#[tokio::test]
async fn a_standalone_installation_reports_that_its_own_catalog_is_authoritative() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    assert!(
        call(&host, Local, "variable_create", create("OWN_ENTRY", SECRET))
            .await
            .ok
    );
    let list = call(&host, Local, "variable_list", json!({})).await;
    assert_eq!(
        list.data["sync"],
        json!({"mode":"authoritative","state":"authoritative","revision":1})
    );
    let sync = call(&host, Local, "variable_sync", json!({"reset":true})).await;
    assert!(sync.ok);
    assert_eq!(sync.data["sync"]["revision"], 1);
    // Unknown arguments are refused rather than ignored.
    assert!(
        !call(&host, Local, "variable_sync", json!({"account":"x"}))
            .await
            .ok
    );
}
