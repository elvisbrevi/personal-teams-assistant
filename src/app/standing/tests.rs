//! The fence around an installation signed in to a coordinator: outages, refused credentials,
//! sign-out and account switches, driven through the host's own operations with the coordinator
//! protocol in place of the network.
use super::*;
use crate::security::AccountSecrets;
use crate::variables as vault;
use authority::Transport::{Local, Machine, Window};
use control::Reply;
use std::sync::atomic::{AtomicU8, Ordering};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const KEY: &str = "synthetic-api-key-4be0a713";
const OTHER: &str = "synthetic-api-key-d95c2f08";
const LOCAL: &str = "synthetic-local-key-77aa01cd";

const UP: u8 = 0;
const DOWN: u8 = 1;
const REJECTED: u8 = 2;

struct TestShell;
impl Shell for TestShell {
    fn headless(&self) -> bool {
        true
    }
    fn show(&self) -> Result<()> {
        Ok(())
    }
    fn hide(&self) -> Result<()> {
        Ok(())
    }
    fn open_url(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn exit(&self) {}
}
fn host(root: &Path) -> Arc<Host> {
    Arc::new(Host {
        state: init_state(&root.join("profile"), &root.join("data")).unwrap(),
        shell: Box::new(TestShell),
    })
}
async fn call(
    host: &Arc<Host>,
    transport: authority::Transport,
    method: &str,
    args: Value,
) -> Reply {
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
fn text(reply: &Reply) -> String {
    serde_json::to_string(reply).unwrap()
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
async fn store(host: &Arc<Host>, name: &str, value: &str) -> String {
    let entry = id();
    let reply = call(
        host,
        Local,
        "variable_create",
        json!({"id":entry,"name":name,"description":format!("Synthetic {name}"),"value":value}),
    )
    .await;
    assert!(reply.ok, "{:?}", reply.message);
    entry
}

/// Which coordinator the installation reaches, and its machine credential there.
struct Target {
    host: Arc<Host>,
    machine: String,
    token: String,
}
struct Rig {
    root: tempfile::TempDir,
    coordinator: Arc<Host>,
    native: Arc<Host>,
    profile: PathBuf,
    net: Arc<AtomicU8>,
    target: Arc<std::sync::Mutex<Target>>,
    /// On the next call of this method, the installation signs in to account 2002 first.
    switch_on: Arc<std::sync::Mutex<Option<String>>>,
    server: MockServer,
}

fn connect(profile: &Path, account: u64, machine: &str) {
    fs::create_dir_all(profile.join("implementation")).unwrap();
    fs::write(
        profile.join("implementation").join("connection.json"),
        json!({"url":"https://coordinator.example","account":account,"id":machine}).to_string(),
    )
    .unwrap();
}
async fn enroll(coordinator: &Arc<Host>) -> (String, String) {
    let machine = id();
    let registered = call(
        coordinator,
        Machine,
        "machine_register",
        json!({"id":machine,"name":"Laptop","os":"linux"}),
    )
    .await;
    assert!(registered.ok, "{:?}", registered.message);
    (
        machine,
        registered.data["token"].as_str().unwrap().to_owned(),
    )
}

async fn rig() -> Rig {
    let root = tempfile::tempdir().unwrap();
    let coordinator = host(&root.path().join("a"));
    let native = host(&root.path().join("native"));
    let (machine, token) = enroll(&coordinator).await;
    let profile = root.path().join("native").join("profile");
    connect(&profile, 1001, &machine);
    let net = Arc::new(AtomicU8::new(UP));
    let target = Arc::new(std::sync::Mutex::new(Target {
        host: coordinator.clone(),
        machine,
        token,
    }));
    let switch_on: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
    {
        let (net, target, switch_on, profile) = (
            net.clone(),
            target.clone(),
            switch_on.clone(),
            profile.clone(),
        );
        native.state.variables.use_transport(move |method, args| {
            let (net, target, switch_on, profile) =
                (net.clone(), target.clone(), switch_on.clone(), profile.clone());
            Box::pin(async move {
                if switch_on.lock().unwrap().as_deref() == Some(method.as_str()) {
                    *switch_on.lock().unwrap() = None;
                    let machine = target.lock().unwrap().machine.clone();
                    connect(&profile, 2002, &machine);
                }
                match net.load(Ordering::SeqCst) {
                    DOWN => Err(Refusal::Unavailable.into()),
                    REJECTED => Err(Refusal::Rejected.into()),
                    _ => {
                        let (host, machine, token) = {
                            let t = target.lock().unwrap();
                            (t.host.clone(), t.machine.clone(), t.token.clone())
                        };
                        let reply = call(
                            &host,
                            Machine,
                            "machine_protocol",
                            json!({"id":machine,"token":token,"action":"call","method":method,"args":args}),
                        )
                        .await;
                        Ok(serde_json::to_value(reply)?)
                    }
                }
            })
        });
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    *native.state.variables.consumers.origin.lock().unwrap() = Some(server.uri());
    Rig {
        root,
        coordinator,
        native,
        profile,
        net,
        target,
        switch_on,
        server,
    }
}
impl Rig {
    fn set(&self, mode: u8) {
        self.net.store(mode, Ordering::SeqCst);
    }
    async fn bearers(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| {
                r.headers
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
    async fn bind(&self, entry: &str) -> Reply {
        call(
            &self.native,
            Local,
            "variable_consumer_bind",
            json!({"consumer":"deepseek","variable":entry}),
        )
        .await
    }
    async fn check(&self) -> Reply {
        call(
            &self.native,
            Local,
            "variable_consumer_check",
            json!({"consumer":"deepseek"}),
        )
        .await
    }
    async fn standing(&self) -> Value {
        let status = call(&self.native, Local, "portable_status", json!({})).await;
        assert!(status.ok, "{:?}", status.message);
        status.data["standing"].clone()
    }
}

#[tokio::test]
async fn an_offline_client_reads_its_cached_state_but_cannot_use_account_variables() {
    let rig = rig().await;
    let entry = store(&rig.coordinator, "DEEPSEEK_KEY", KEY).await;
    let listed = call(&rig.native, Local, "variable_list", json!({})).await;
    assert!(listed.ok && listed.data["sync"]["state"] == "synced");
    assert!(rig.bind(&entry).await.ok);
    assert!(rig.check().await.ok);
    assert_eq!(rig.bearers().await, [format!("Bearer {KEY}")]);
    assert_eq!(rig.standing().await["state"], "live");

    rig.set(DOWN);
    // The cached configuration stays inspectable, with an honest state.
    let cached = call(&rig.native, Local, "variable_list", json!({})).await;
    assert!(cached.ok);
    assert_eq!(cached.data["variables"][0]["name"], "DEEPSEEK_KEY");
    assert_eq!(cached.data["sync"]["state"], "synced");
    // A new use, a new choice and a reveal are all denied; none reaches the consumer.
    let used = rig.check().await;
    assert_eq!(
        (used.ok, used.code.as_str(), used.exit_code),
        (false, "dependency_or_network", 5)
    );
    assert!(used.message.unwrap().contains("cannot be reached"));
    let other = store(&rig.coordinator, "SECOND_KEY", OTHER).await;
    let rebound = rig.bind(&other).await;
    assert_eq!(rebound.code, "dependency_or_network");
    let revealed = call(&rig.native, Window, "variable_reveal", json!({"id":entry})).await;
    assert!(!revealed.ok && !text(&revealed).contains(KEY));
    assert_eq!(rig.bearers().await.len(), 1);
    let standing = rig.standing().await;
    assert_eq!(standing["state"], "offline");
    assert_eq!(standing["variables_usable"], false);
    assert!(standing["last_live_at"].is_number());
    // The binding survived the failed rebind; removing authority never needs the network.
    let consumers = call(&rig.native, Local, "variable_consumer_list", json!({})).await;
    assert_eq!(consumers.data["consumers"][0]["binding"]["id"], entry);
    let unbound = call(
        &rig.native,
        Local,
        "variable_consumer_unbind",
        json!({"consumer":"deepseek"}),
    )
    .await;
    assert!(unbound.ok && unbound.data["unbound"] == true);

    // Back online the same operations work again with a fresh live check.
    rig.set(UP);
    assert!(rig.bind(&entry).await.ok);
    assert!(rig.check().await.ok);
    assert_eq!(rig.standing().await["state"], "live");
    assert_eq!(rig.bearers().await.len(), 2);
}

#[tokio::test]
async fn a_refused_credential_denies_use_and_never_falls_back_to_a_local_store() {
    let rig = rig().await;
    // A value in this installation's own store, bound before it signed in.
    let local = rig.native.state.variables.store(&rig.native).unwrap();
    let local_entry = id();
    local
        .create(vault::NewVariable {
            id: local_entry.clone(),
            name: "LOCAL_KEY".into(),
            description: "Synthetic local".into(),
            value: vault::Secret::new(LOCAL),
        })
        .unwrap();
    local.bind("deepseek", "api_key", &local_entry).unwrap();
    let entry = store(&rig.coordinator, "DEEPSEEK_KEY", KEY).await;
    call(&rig.native, Local, "variable_sync", json!({})).await;
    assert!(rig.bind(&entry).await.ok);
    assert!(rig.check().await.ok);

    rig.set(REJECTED);
    let denied = rig.check().await;
    assert_eq!(
        (denied.ok, denied.code.as_str(), denied.exit_code),
        (false, "not_ready", 3)
    );
    let message = denied.message.clone().unwrap();
    assert!(message.contains("no longer accepts") && message.contains("Sign in"));
    for token in [KEY, LOCAL] {
        assert!(!text(&denied).contains(token));
    }
    // Neither the coordinator's value nor the local one was sent anywhere after the refusal.
    assert_eq!(rig.bearers().await, [format!("Bearer {KEY}")]);
    let standing = rig.standing().await;
    assert_eq!(standing["state"], "revoked");
    assert!(standing["recovery"].as_str().unwrap().contains("Sign in"));
    // Synchronization reports the same refusal with the same recovery.
    let synced = call(&rig.native, Local, "variable_sync", json!({})).await;
    assert_eq!(synced.code, "not_ready");
    // Reveal, edits and launchers stay out of reach too.
    let edit = call(
        &rig.native,
        Local,
        "variable_create",
        json!({"id":id(),"name":"NEW_ONE","description":"x","value":OTHER}),
    )
    .await;
    assert!(!edit.ok && !text(&edit).contains(OTHER));
}

#[tokio::test]
async fn the_credential_lookup_uses_only_a_value_resolved_for_the_operation() {
    let rig = rig().await;
    let entry = store(&rig.coordinator, "DEEPSEEK_KEY", KEY).await;
    call(&rig.native, Local, "variable_sync", json!({})).await;
    assert!(rig.bind(&entry).await.ok);
    let resolver = consumers::Resolver::new(&rig.native);
    assert!(resolver.bound("DEEPSEEK_API_KEY").unwrap());
    // Nothing is held until an operation resolves it online.
    assert!(resolver.value("DEEPSEEK_API_KEY").is_err());
    consumers::stage(&rig.native.state).await;
    assert_eq!(resolver.value("DEEPSEEK_API_KEY").unwrap(), KEY);

    // An outage or refusal at the next operation replaces the earlier value with a denial.
    rig.set(DOWN);
    consumers::stage(&rig.native.state).await;
    let outage = resolver.value("DEEPSEEK_API_KEY").unwrap_err().to_string();
    assert!(outage.contains("cannot be reached") && !outage.contains(KEY));
    rig.set(REJECTED);
    consumers::stage(&rig.native.state).await;
    assert!(
        resolver
            .value("DEEPSEEK_API_KEY")
            .unwrap_err()
            .to_string()
            .contains("Sign in")
    );

    // Losing authority withdraws what an operation had staged.
    rig.set(UP);
    consumers::stage(&rig.native.state).await;
    assert_eq!(resolver.value("DEEPSEEK_API_KEY").unwrap(), KEY);
    rig.set(REJECTED);
    assert!(
        !call(&rig.native, Local, "variable_sync", json!({}))
            .await
            .ok
    );
    assert!(resolver.value("DEEPSEEK_API_KEY").is_err());
    // Unbinding returns to the configured named variable, never a legacy credential.
    call(
        &rig.native,
        Local,
        "variable_consumer_unbind",
        json!({"consumer":"deepseek"}),
    )
    .await;
    // Removing a binding keeps the name managed by Variables, with no legacy fallback.
    assert!(resolver.bound("DEEPSEEK_API_KEY").unwrap());
    assert!(resolver.value("DEEPSEEK_API_KEY").is_err());
}

#[tokio::test]
async fn signing_out_and_switching_accounts_clear_the_former_account_and_repeat_safely() {
    let rig = rig().await;
    let entry = store(&rig.coordinator, "ALICE_KEY", KEY).await;
    assert!(
        call(&rig.native, Local, "variable_list", json!({}))
            .await
            .ok
    );
    assert!(rig.bind(&entry).await.ok);
    let request = call(
        &rig.native,
        Local,
        "variable_entry_start",
        json!({"name":"PENDING_ONE","description":"Waiting for the user"}),
    )
    .await;
    assert!(request.ok, "{:?}", request.message);
    let mirror = rig.profile.join("variables").join("catalog-mirror.json");
    assert!(mirror.exists());

    // Signing out works from any state and can be repeated; the coordinator keeps the account.
    rig.set(DOWN);
    for _ in 0..2 {
        let out = call(&rig.native, Window, "machine_disconnect", json!({})).await;
        assert!(out.ok, "{:?}", out.message);
    }
    assert!(
        !rig.profile
            .join("implementation")
            .join("connection.json")
            .exists()
    );
    assert!(!mirror.exists());
    let signed_out = call(&rig.native, Local, "portable_status", json!({})).await;
    assert_eq!(signed_out.data["standing"]["state"], "signed_out");
    let own = call(&rig.native, Local, "variable_list", json!({})).await;
    assert_eq!(own.data["variables"].as_array().unwrap().len(), 0);
    assert_eq!(own.data["pending"].as_array().unwrap().len(), 0);
    rig.set(UP);
    let kept = call(&rig.coordinator, Local, "variable_list", json!({})).await;
    assert_eq!(kept.data["variables"][0]["name"], "ALICE_KEY");

    // Another account signs in on the same installation: nothing of the first one appears.
    let bob = host(&rig.root.path().join("b"));
    let bob_entry = store(&bob, "BOB_KEY", OTHER).await;
    let (machine, token) = enroll(&bob).await;
    *rig.target.lock().unwrap() = Target {
        host: bob.clone(),
        machine: machine.clone(),
        token,
    };
    connect(&rig.profile, 2002, &machine);
    let listed = call(&rig.native, Local, "variable_list", json!({})).await;
    assert!(listed.ok, "{:?}", listed.message);
    let names: Vec<_> = listed.data["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["BOB_KEY"]);
    assert!(!text(&listed).contains("ALICE_KEY") && !text(&listed).contains("PENDING_ONE"));
    let consumers = call(&rig.native, Local, "variable_consumer_list", json!({})).await;
    assert_eq!(consumers.data["consumers"][0]["binding"], Value::Null);
    assert_eq!(rig.check().await.code, "invalid_input");
    // Alice's reference is not Bob's: it does not resolve there.
    let foreign = rig.bind(&entry).await;
    assert!(!foreign.ok && !text(&foreign).contains(KEY));
    assert!(rig.bind(&bob_entry).await.ok);
}

#[tokio::test]
async fn a_switch_during_an_exchange_never_fills_the_next_account() {
    let rig = rig().await;
    let entry = store(&rig.coordinator, "ALICE_KEY", KEY).await;
    call(&rig.native, Local, "variable_sync", json!({})).await;
    assert!(rig.bind(&entry).await.ok);
    let resolver = consumers::Resolver::new(&rig.native);

    // The answer to an operation's lookup arrives after another account signed in.
    *rig.switch_on.lock().unwrap() = Some("variable_resolve".into());
    consumers::stage(&rig.native.state).await;
    assert!(resolver.value("DEEPSEEK_API_KEY").is_err());
    // Removing a binding keeps the name managed by Variables, with no legacy fallback.
    assert!(resolver.bound("DEEPSEEK_API_KEY").unwrap());
    assert!(resolver.value("DEEPSEEK_API_KEY").is_err());

    // So does a catalog read that was in flight.
    connect(
        &rig.profile,
        1001,
        &rig.target.lock().unwrap().machine.clone(),
    );
    *rig.switch_on.lock().unwrap() = Some("variable_catalog".into());
    let late = call(&rig.native, Local, "variable_sync", json!({})).await;
    assert!(!late.ok);
    let status = call(&rig.native, Local, "variable_sync_status", json!({})).await;
    assert_eq!(status.data["sync"]["entries"], Value::Null);
    assert_eq!(status.data["sync"]["revision"], Value::Null);
}

#[tokio::test]
async fn a_machine_revoked_at_the_coordinator_is_denied_at_its_next_use() {
    let rig = rig().await;
    let entry = store(&rig.coordinator, "DEEPSEEK_KEY", KEY).await;
    call(&rig.native, Local, "variable_sync", json!({})).await;
    assert!(rig.bind(&entry).await.ok);
    let machine = rig.target.lock().unwrap().machine.clone();
    assert!(
        call(
            &rig.coordinator,
            Window,
            "machine_revoke",
            json!({"machine":machine})
        )
        .await
        .ok
    );
    let denied = rig.check().await;
    assert!(!denied.ok && !text(&denied).contains(KEY));
    assert_eq!(denied.code, "not_ready");
    assert_eq!(rig.bearers().await.len(), 0);
    // The coordinator recorded no successful use for the revoked machine.
    let audit = call(&rig.coordinator, Local, "variable_audit", json!({})).await;
    let uses = audit.data["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "use" && e["outcome"] == "ok")
        .count();
    assert_eq!(uses, 1);
}

#[cfg(unix)]
#[tokio::test]
async fn a_launcher_resolves_its_values_online_for_every_run() {
    use std::os::unix::fs::PermissionsExt;
    let rig = rig().await;
    let work = rig.root.path().join("work");
    fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();
    // An owner allows the directory while standalone; the roots then stay with the machine.
    let link = rig.profile.join("implementation").join("connection.json");
    let kept = fs::read(&link).unwrap();
    fs::remove_file(&link).unwrap();
    let saved = call(
        &rig.native,
        Window,
        "machine_save",
        json!({"name":"Synthetic","roots":[work]}),
    )
    .await;
    assert!(saved.ok, "{:?}", saved.message);
    fs::write(&link, kept).unwrap();
    let entry = store(&rig.coordinator, "DEPLOY_SECRET", KEY).await;
    call(&rig.native, Local, "variable_sync", json!({})).await;
    let seen = work.join("seen");
    let tool = work.join("tool.sh");
    fs::write(
        &tool,
        format!(
            "#!/bin/sh\nprintf '%s' \"$DEPLOY_TOKEN\" > {}\n",
            seen.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let tool = tool.canonicalize().unwrap();
    let defined = call(
        &rig.native,
        Window,
        "variable_launcher_define",
        json!({"id":"deploy","description":"Synthetic","program":tool,"directory":work,"env":{"DEPLOY_TOKEN":entry}}),
    )
    .await;
    assert!(defined.ok, "{:?}", defined.message);
    let listed = call(&rig.native, Window, "variable_consumer_list", json!({})).await;
    let digest = listed.data["launchers"][0]["digest"].clone();
    let approved = call(
        &rig.native,
        Window,
        "variable_launcher_approve",
        json!({"id":"deploy","digest":digest}),
    )
    .await;
    assert!(approved.ok, "{:?}", approved.message);
    let run = call(
        &rig.native,
        Window,
        "variable_launcher_run",
        json!({"id":"deploy"}),
    )
    .await;
    assert!(run.ok, "{:?}", run.message);
    assert_eq!(fs::read_to_string(&seen).unwrap(), KEY);
    assert!(!text(&run).contains(KEY));
    fs::remove_file(&seen).unwrap();
    for (mode, code) in [(DOWN, "dependency_or_network"), (REJECTED, "not_ready")] {
        rig.set(mode);
        let denied = call(
            &rig.native,
            Window,
            "variable_launcher_run",
            json!({"id":"deploy"}),
        )
        .await;
        assert_eq!(denied.code, code);
        assert!(
            !seen.exists(),
            "the program must not start without a live answer"
        );
    }
    // The launcher list belongs to this account only.
    connect(
        &rig.profile,
        2002,
        &rig.target.lock().unwrap().machine.clone(),
    );
    rig.set(UP);
    let listed = call(&rig.native, Window, "variable_consumer_list", json!({})).await;
    assert_eq!(listed.data["launchers"].as_array().unwrap().len(), 0);
}

#[test]
fn the_standing_distinguishes_signed_out_unknown_live_offline_and_revoked() {
    let state = State::default();
    let link = Link {
        origin: "https://coordinator.example".into(),
        account: 7,
        machine: id(),
    };
    assert_eq!(state.view(None)["state"], "signed_out");
    assert_eq!(state.view(Some(&link))["state"], "unknown");
    let live: anyhow::Result<()> = Ok(());
    state.observe(&link, &live);
    let seen = state.view(Some(&link));
    assert_eq!(
        (seen["state"].as_str(), seen["variables_usable"].as_bool()),
        (Some("live"), Some(true))
    );
    let down: anyhow::Result<()> = Err(Refusal::Unavailable.into());
    assert_eq!(state.observe(&link, &down), Observed::Offline);
    assert!(state.view(Some(&link))["last_live_at"].is_number());
    // An operation refusal says nothing about reachability.
    let refused: anyhow::Result<()> = Err(anyhow::anyhow!("Machine operation failed."));
    assert_eq!(state.observe(&link, &refused), Observed::Offline);
    let revoked: anyhow::Result<()> = Err(Refusal::Rejected.into());
    assert_eq!(state.observe(&link, &revoked), Observed::Revoked);
    assert_eq!(state.view(Some(&link))["state"], "revoked");
    // Another account never inherits the observation.
    let other = Link {
        account: 8,
        ..link.clone()
    };
    assert_eq!(state.view(Some(&other))["state"], "unknown");
    state.reset();
    assert_eq!(state.view(Some(&link))["state"], "unknown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_application_variables_resolve_live_without_a_binding_or_legacy_fallback() {
    let rig = rig().await;
    let entry = store(&rig.coordinator, "DEEPSEEK_API_KEY", KEY).await;
    assert!(
        call(&rig.native, Local, "variable_sync", json!({}))
            .await
            .ok
    );
    let resolver = consumers::Resolver::new(&rig.native);
    assert!(resolver.bound("DEEPSEEK_API_KEY").unwrap());
    assert_eq!(resolver.value("DEEPSEEK_API_KEY").unwrap(), KEY);
    let store = rig
        .coordinator
        .state
        .variables
        .store(&rig.coordinator)
        .unwrap();
    store
        .update(&entry, 1, None, Some(&vault::Secret::new(OTHER)))
        .unwrap();
    // Names come from the synchronized catalog; values are resolved anew even after rotation.
    assert_eq!(resolver.value("DEEPSEEK_API_KEY").unwrap(), OTHER);
    rig.set(DOWN);
    assert!(resolver.value("DEEPSEEK_API_KEY").is_err());
    rig.set(REJECTED);
    assert!(resolver.value("DEEPSEEK_API_KEY").is_err());
    rig.set(UP);
    *rig.switch_on.lock().unwrap() = Some("variable_resolve".into());
    assert!(resolver.value("DEEPSEEK_API_KEY").is_err());
}
