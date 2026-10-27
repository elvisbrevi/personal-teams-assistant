//! Synchronization of the account's portable configuration between a coordinator and an
//! installation signed in to it, driven through the hosts' own operations with the coordinator
//! protocol in place of the network.
use super::*;
use crate::knowledge::Resource;
use crate::questions::Question;
use authority::Transport::{Local, Machine, Window};

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

type Tweak = Arc<dyn Fn(&str, &mut Value) + Send + Sync>;

struct Pair {
    coordinator: Arc<Host>,
    native: Arc<Host>,
    machine: String,
    token: String,
    online: Arc<std::sync::atomic::AtomicBool>,
}

fn connect(profile: &Path, account: u64, machine: &str) {
    fs::create_dir_all(profile.join("implementation")).unwrap();
    fs::write(
        profile.join("implementation").join("connection.json"),
        json!({"url":"https://coordinator.example","account":account,"id":machine}).to_string(),
    )
    .unwrap();
}

/// A coordinator and an installation connected to it; calls travel through the real
/// `machine_protocol` of the coordinator, optionally altered on the way back.
async fn pair(root: &Path, tweak: Option<Tweak>) -> Pair {
    let coordinator = host(&root.join("coordinator"));
    let native = host(&root.join("native"));
    let machine = uuid::Uuid::new_v4().to_string();
    let registered = call(
        &coordinator,
        Machine,
        "machine_register",
        json!({"id":machine,"name":"Laptop","os":"linux"}),
    )
    .await;
    assert!(registered.ok, "{:?}", registered.message);
    let token = registered.data["token"].as_str().unwrap().to_owned();
    connect(&root.join("native").join("profile"), 1001, &machine);
    let online = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (target, id_, token_, up) = (
        coordinator.clone(),
        machine.clone(),
        token.clone(),
        online.clone(),
    );
    native
        .state
        .portable
        .use_transport(Arc::new(move |method, args| {
            let (target, id_, token_, tweak, up) = (
                target.clone(),
                id_.clone(),
                token_.clone(),
                tweak.clone(),
                up.clone(),
            );
            Box::pin(async move {
                if !up.load(std::sync::atomic::Ordering::SeqCst) {
                    anyhow::bail!("offline");
                }
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
        online,
    }
}

fn url_source(id: &str, enabled: bool) -> Resource {
    Resource {
        id: id.into(),
        description: format!("Synthetic page {id}"),
        topics: vec!["billing".into()],
        enabled,
        external_processing: enabled,
        allowed_conversations: if enabled {
            vec!["chat-a".into()]
        } else {
            vec![]
        },
        allowed_senders: if enabled {
            vec!["sender-1".into()]
        } else {
            vec![]
        },
        access: Access::Url {
            url: "https://example.com/page".into(),
        },
    }
}

/// Give the coordinator an account configuration of its own.
fn configure(host: &Host) {
    let state = &host.state;
    let mut config = read_config(&state.config_path).unwrap();
    config.llm.language = crate::config::Language::En;
    config.llm.style = "Synthetic account style".into();
    config.policy.greeting = "Hello from the account".into();
    config.graph.user_id = "00000000-0000-0000-0000-0000000000a1".into();
    config.graph.allowed_chats = vec!["19:coordinator-only@thread.v2".into()];
    let mut map = KnowledgeMap::load(&state.map_path).unwrap();
    let activity = map.resources[0].id.clone();
    map.resources.push(url_source("billing-site", true));
    map.contexts.push(crate::knowledge::Context {
        id: "billing".into(),
        name: "Billing".into(),
        keywords: vec!["invoice".into()],
        sources: vec!["billing-site".into()],
        ..Default::default()
    });
    map.questions.as_mut().unwrap().push(Question {
        id: "how-to-invoice".into(),
        text: "How do I issue an invoice?".into(),
        sources: vec!["billing-site".into(), activity],
        examples: vec![],
    });
    commit_settings(state, &config, &map, "").unwrap();
    let index = Index::open(&config.server.data_dir.join(crate::index::FILE)).unwrap();
    index
        .save_note(Note {
            title: "Invoice rule".into(),
            text: "Invoices are issued on the first business day.".into(),
            contexts: vec!["billing".into()],
            ..Default::default()
        })
        .unwrap();
}

fn notes_of(host: &Host) -> Vec<Note> {
    let config = read_config(&host.state.config_path).unwrap();
    Index::open(&config.server.data_dir.join(crate::index::FILE))
        .unwrap()
        .notes(None, 1000)
        .unwrap()
}
fn map_of(host: &Host) -> KnowledgeMap {
    KnowledgeMap::load(&host.state.map_path).unwrap()
}
fn config_of(host: &Host) -> Config {
    read_config(&host.state.config_path).unwrap()
}
async fn save(host: &Arc<Host>, config: &Config, map: &KnowledgeMap) -> Reply {
    call(
        host,
        Window,
        "save_settings",
        json!({"config":config,"map":map,"tunnel_config":""}),
    )
    .await
}
async fn sync(host: &Arc<Host>, args: Value) -> Reply {
    call(host, Local, "portable_sync", args).await
}
async fn status(host: &Arc<Host>) -> Value {
    call(host, Local, "portable_status", json!({})).await.data["sync"].clone()
}
fn profile_text(root: &Path) -> String {
    let mut out = String::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(text) = fs::read_to_string(&path) {
                out.push_str(&text);
            }
        }
    }
    out
}

#[tokio::test]
async fn a_fresh_installation_restores_the_accounts_configuration_and_nothing_local() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    configure(&pair.coordinator);
    let before = config_of(&pair.native);
    assert_eq!(status(&pair.native).await["state"], "never");

    let synced = sync(&pair.native, json!({})).await;
    assert!(synced.ok, "{:?}", synced.message);
    assert_eq!(synced.data["sync"]["state"], "synced");
    assert_eq!(synced.data["sync"]["loaded"], synced.data["sync"]["saved"]);
    assert_eq!(synced.data["changes"]["changed"], true);

    // Preferences, model choices, sources with their exact audiences, contexts, questions, notes.
    let config = config_of(&pair.native);
    assert_eq!(config.llm.language, crate::config::Language::En);
    assert_eq!(config.llm.style, "Synthetic account style");
    assert_eq!(config.policy.greeting, "Hello from the account");
    assert_eq!(config.llm.chain, config_of(&pair.coordinator).llm.chain);
    let map = map_of(&pair.native);
    let source = map
        .resources
        .iter()
        .find(|r| r.id == "billing-site")
        .unwrap();
    assert!(source.enabled && source.external_processing);
    assert_eq!(source.allowed_conversations, ["chat-a"]);
    assert_eq!(source.allowed_senders, ["sender-1"]);
    assert_eq!(map.contexts[0].id, "billing");
    assert!(
        map.questions
            .unwrap()
            .iter()
            .any(|q| q.id == "how-to-invoice")
    );
    assert_eq!(notes_of(&pair.native)[0].title, "Invoice rule");

    // Everything local stays this computer's own: identity, data, bind, Teams, roots.
    assert_eq!(config.server.data_dir, before.server.data_dir);
    assert_eq!(config.server.bind, before.server.bind);
    assert_eq!(config.graph.user_id, before.graph.user_id);
    assert!(config.graph.allowed_chats.is_empty());
    assert_eq!(config.knowledge_map, before.knowledge_map);
    let native_text = profile_text(&root.path().join("native"));
    let coordinator_data = config_of(&pair.coordinator)
        .server
        .data_dir
        .display()
        .to_string();
    for private in [
        coordinator_data.as_str(),
        "19:coordinator-only",
        "0000000000a1",
    ] {
        assert!(
            !native_text.contains(private),
            "{private} reached the installation"
        );
    }
    // Restoring never starts Teams, enables a root or selects the Teams machine.
    assert!(pair.native.state.running.lock().await.is_none());
    assert!(
        implementation::roots(&pair.native)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        synced.data["sync"]["local_setup"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "roots")
    );

    // A second look finds nothing to do, and the coordinator reports the same revision.
    let again = sync(&pair.native, json!({})).await;
    assert_eq!(again.data["changes"]["changed"], false);
    let authoritative = status(&pair.coordinator).await;
    assert_eq!(authoritative["mode"], "authoritative");
    assert_eq!(authoritative["revision"], again.data["sync"]["loaded"]);
    assert_eq!(authoritative["notes"], 1);
}

#[tokio::test]
async fn the_coordinator_snapshot_contains_only_the_allowlist_and_only_machines_may_read_it() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    configure(&pair.coordinator);
    let reply = call(
        &pair.coordinator,
        Machine,
        "machine_protocol",
        json!({"id":pair.machine,"token":pair.token,"action":"call","method":"portable_snapshot","args":{"section":"config"}}),
    )
    .await;
    assert!(reply.ok, "{:?}", reply.message);
    let body = text(&reply);
    let config = config_of(&pair.coordinator);
    for private in [
        config.server.data_dir.display().to_string(),
        "19:coordinator-only".to_owned(),
        "0000000000a1".to_owned(),
        "public_url".to_owned(),
        "tenant_id".to_owned(),
        "webhook".to_owned(),
        "knowledge_map".to_owned(),
        "repositories".to_owned(),
        "STATE_ENCRYPTION_KEY".to_owned(),
    ] {
        assert!(!body.contains(&private), "{private} is in the snapshot");
    }
    assert_eq!(
        reply.data["document"]["settings"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "chain",
            "greeting",
            "language",
            "max_context_chars",
            "max_message_age_seconds",
            "sensitive_patterns",
            "style"
        ]
        .map(String::from)
        .into()
    );
    // Neither a window, the CLI nor an unregistered machine can call the machine methods.
    for transport in [Window, Local] {
        let denied = call(
            &pair.coordinator,
            transport,
            "portable_snapshot",
            json!({"section":"config"}),
        )
        .await;
        assert!(!denied.ok);
    }
    let forged = call(
        &pair.coordinator,
        Machine,
        "machine_protocol",
        json!({"id":pair.machine,"token":"forged","action":"call","method":"portable_snapshot","args":{"section":"config"}}),
    )
    .await;
    assert!(!forged.ok);
    // A change that names a local-only field is not even parsed.
    let hostile = call(
        &pair.coordinator,
        Machine,
        "machine_protocol",
        json!({"id":pair.machine,"token":pair.token,"action":"call","method":"portable_apply","args":{
            "base": "x", "patch": {"settings": {"language":"es","style":"s","chain":[],"greeting":"g",
            "max_context_chars":1000,"max_message_age_seconds":60,"sensitive_patterns":[],
            "data_dir":"/elsewhere"}}}}),
    )
    .await;
    assert!(!hostile.ok);
    assert_ne!(
        config_of(&pair.coordinator).server.data_dir,
        PathBuf::from("/elsewhere")
    );
}

#[tokio::test]
async fn an_edit_on_the_installation_is_decided_by_the_coordinator_and_never_widens_access() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    configure(&pair.coordinator);
    assert!(sync(&pair.native, json!({})).await.ok);

    // A preference changes at the account, then here.
    let mut config = config_of(&pair.native);
    let map = map_of(&pair.native);
    config.policy.greeting = "Hello, edited here".into();
    let saved = save(&pair.native, &config, &map).await;
    assert!(saved.ok, "{:?}", saved.message);
    assert_eq!(
        config_of(&pair.coordinator).policy.greeting,
        "Hello, edited here"
    );
    let after = status(&pair.native).await;
    assert_eq!(after["state"], "synced");
    assert_eq!(after["loaded"], status(&pair.coordinator).await["revision"]);

    // A new source starts disabled, without audiences or external processing.
    let mut map = map_of(&pair.native);
    map.resources.push(url_source("new-page", true));
    let widened = save(&pair.native, &config_of(&pair.native), &map).await;
    assert!(!widened.ok);
    assert_eq!(widened.code, "invalid_input");
    assert!(widened.message.unwrap().contains("starts disabled"));
    assert!(
        !map_of(&pair.native)
            .resources
            .iter()
            .any(|r| r.id == "new-page")
    );
    assert!(
        !map_of(&pair.coordinator)
            .resources
            .iter()
            .any(|r| r.id == "new-page")
    );
    let mut map = map_of(&pair.native);
    map.resources.push(url_source("new-page", false));
    assert!(save(&pair.native, &config_of(&pair.native), &map).await.ok);
    let created = map_of(&pair.coordinator);
    let new_page = created
        .resources
        .iter()
        .find(|r| r.id == "new-page")
        .unwrap();
    assert!(
        !new_page.enabled
            && !new_page.external_processing
            && new_page.allowed_conversations.is_empty()
    );

    // Granting access, or enabling what is disabled, is done at the coordinator.
    let mut map = map_of(&pair.native);
    map.resources
        .iter_mut()
        .find(|r| r.id == "new-page")
        .unwrap()
        .enabled = true;
    assert!(!save(&pair.native, &config_of(&pair.native), &map).await.ok);
    let mut map = map_of(&pair.native);
    map.resources
        .iter_mut()
        .find(|r| r.id == "billing-site")
        .unwrap()
        .allowed_conversations
        .push("*".into());
    assert!(!save(&pair.native, &config_of(&pair.native), &map).await.ok);
    let exact = map_of(&pair.coordinator);
    assert_eq!(
        exact
            .resources
            .iter()
            .find(|r| r.id == "billing-site")
            .unwrap()
            .allowed_conversations,
        ["chat-a"]
    );
    // Narrowing is allowed.
    let mut map = map_of(&pair.native);
    map.resources
        .iter_mut()
        .find(|r| r.id == "billing-site")
        .unwrap()
        .enabled = false;
    assert!(save(&pair.native, &config_of(&pair.native), &map).await.ok);
    assert!(
        !map_of(&pair.coordinator)
            .resources
            .iter()
            .find(|r| r.id == "billing-site")
            .unwrap()
            .enabled
    );

    // Local-only changes never need the coordinator.
    pair.online
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut local_only = config_of(&pair.native);
    local_only.server.bind = "127.0.0.1:39001".into();
    assert!(
        save(&pair.native, &local_only, &map_of(&pair.native))
            .await
            .ok
    );
    assert_eq!(config_of(&pair.native).server.bind, "127.0.0.1:39001");
    assert_eq!(config_of(&pair.coordinator).server.bind, "127.0.0.1:38655");
    // A portable change while offline is refused and saves nothing.
    let mut offline = config_of(&pair.native);
    offline.llm.style = "Offline edit".into();
    let refused = save(&pair.native, &offline, &map_of(&pair.native)).await;
    assert!(!refused.ok);
    assert_eq!(refused.code, "dependency_or_network");
    assert_ne!(config_of(&pair.native).llm.style, "Offline edit");
    // The synchronized state stays inspectable offline.
    let kept = status(&pair.native).await;
    assert_eq!(kept["state"], "synced");
    assert!(
        map_of(&pair.native)
            .resources
            .iter()
            .any(|r| r.id == "billing-site")
    );
}

#[tokio::test]
async fn concurrent_edits_conflict_and_unsaved_local_changes_are_never_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    configure(&pair.coordinator);
    assert!(sync(&pair.native, json!({})).await.ok);

    // The account changes elsewhere; the installation's stale edit is a conflict.
    let mut elsewhere = config_of(&pair.coordinator);
    elsewhere.llm.style = "Changed at the account".into();
    commit_settings(
        &pair.coordinator.state,
        &elsewhere,
        &map_of(&pair.coordinator),
        "",
    )
    .unwrap();
    let mut stale = config_of(&pair.native);
    stale.policy.greeting = "Late edit".into();
    let conflict = save(&pair.native, &stale, &map_of(&pair.native)).await;
    assert!(!conflict.ok);
    assert_eq!(conflict.code, "state_conflict");
    assert_eq!(
        config_of(&pair.coordinator).policy.greeting,
        "Hello from the account"
    );
    assert_eq!(
        config_of(&pair.native).policy.greeting,
        "Hello from the account"
    );
    assert_eq!(status(&pair.native).await["state"], "conflict");

    // Fresh state first (an explicit synchronization), then the edit is accepted.
    let refreshed = sync(&pair.native, json!({})).await;
    assert!(refreshed.ok, "{:?}", refreshed.message);
    assert_eq!(refreshed.data["sync"]["state"], "synced");
    assert_eq!(config_of(&pair.native).llm.style, "Changed at the account");
    assert!(save(&pair.native, &stale, &map_of(&pair.native)).await.ok);
    assert_eq!(config_of(&pair.coordinator).policy.greeting, "Late edit");

    // Changes made on this computer by other means are kept, not replaced.
    let mut local = config_of(&pair.native);
    local.llm.style = "Hand edited".into();
    commit_settings(&pair.native.state, &local, &map_of(&pair.native), "").unwrap();
    let mut account = config_of(&pair.coordinator);
    account.llm.language = crate::config::Language::Es;
    commit_settings(
        &pair.coordinator.state,
        &account,
        &map_of(&pair.coordinator),
        "",
    )
    .unwrap();
    let blocked = sync(&pair.native, json!({})).await;
    assert!(!blocked.ok);
    assert_eq!(blocked.code, "state_conflict");
    assert_eq!(config_of(&pair.native).llm.style, "Hand edited");
    let view = status(&pair.native).await;
    assert_eq!(view["local_changes"], true);
    assert_eq!(view["state"], "diverged");
    assert_ne!(view["loaded"], view["saved"]);
    // Edits are refused while they would build on a state that is not the account's.
    let mut again = config_of(&pair.native);
    again.policy.greeting = "More".into();
    assert!(!save(&pair.native, &again, &map_of(&pair.native)).await.ok);
    // Explicit recovery: adopt the account's state; what was replaced is kept privately.
    let reset = sync(&pair.native, json!({"reset":true})).await;
    assert!(reset.ok, "{:?}", reset.message);
    assert_eq!(
        config_of(&pair.native).llm.style,
        config_of(&pair.coordinator).llm.style
    );
    assert_eq!(
        config_of(&pair.native).llm.language,
        crate::config::Language::Es
    );
    let backups: Vec<_> = fs::read_dir(root.path().join("native/profile/portable/backups"))
        .unwrap()
        .collect();
    assert_eq!(backups.len(), 1);
    assert!(
        fs::read_to_string(backups[0].as_ref().unwrap().path())
            .unwrap()
            .contains("Hand edited")
    );
    assert_eq!(status(&pair.native).await["state"], "synced");
}

#[tokio::test]
async fn an_already_configured_installation_needs_an_explicit_reset_before_it_adopts_an_account() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    configure(&pair.coordinator);
    let mut mine = config_of(&pair.native);
    mine.llm.style = "My own style".into();
    commit_settings(&pair.native.state, &mine, &map_of(&pair.native), "").unwrap();
    let first = sync(&pair.native, json!({})).await;
    assert!(!first.ok);
    assert_eq!(first.code, "state_conflict");
    assert_eq!(config_of(&pair.native).llm.style, "My own style");
    assert!(sync(&pair.native, json!({"reset":true})).await.ok);
    assert_eq!(config_of(&pair.native).llm.style, "Synthetic account style");
}

#[tokio::test]
async fn sources_bound_to_a_missing_repository_are_unavailable_and_survive_edits() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    configure(&pair.coordinator);
    // The account reads a repository that only the coordinator has a folder for.
    let repository = root.path().join("handbook");
    fs::create_dir_all(repository.join(".git")).unwrap();
    fs::write(repository.join("guide.md"), "Synthetic guide").unwrap();
    let mut map = map_of(&pair.coordinator);
    map.repositories.insert("handbook".into(), repository);
    map.resources.push(Resource {
        id: "handbook-docs".into(),
        description: "Synthetic handbook".into(),
        topics: vec![],
        enabled: true,
        external_processing: true,
        allowed_conversations: vec!["chat-a".into()],
        allowed_senders: vec![],
        access: Access::Repository {
            repository: "handbook".into(),
        },
    });
    map.contexts[0].sources.push("handbook-docs".into());
    map.questions.as_mut().unwrap().push(Question {
        id: "where-is-the-guide".into(),
        text: "Where is the guide?".into(),
        sources: vec!["handbook-docs".into()],
        examples: vec![],
    });
    commit_settings(
        &pair.coordinator.state,
        &config_of(&pair.coordinator),
        &map,
        "",
    )
    .unwrap();

    let synced = sync(&pair.native, json!({})).await;
    assert!(synced.ok, "{:?}", synced.message);
    let view = &synced.data["sync"];
    assert_eq!(view["unavailable"]["sources"][0]["id"], "handbook-docs");
    assert_eq!(view["unavailable"]["sources"][0]["repository"], "handbook");
    assert_eq!(view["unavailable"]["questions"][0], "where-is-the-guide");
    assert!(
        view["local_setup"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "repository:handbook")
    );
    let local = map_of(&pair.native);
    assert!(local.repositories.is_empty());
    assert!(!local.resources.iter().any(|r| r.id == "handbook-docs"));
    assert_eq!(local.contexts[0].sources, ["billing-site"]);

    // Editing the context here keeps the reference only the account can resolve.
    let mut edited = map_of(&pair.native);
    edited.contexts[0].name = "Billing renamed".into();
    let saved = save(&pair.native, &config_of(&pair.native), &edited).await;
    assert!(saved.ok, "{:?}", saved.message);
    let account = map_of(&pair.coordinator);
    assert_eq!(account.contexts[0].name, "Billing renamed");
    assert_eq!(
        account.contexts[0].sources,
        ["billing-site", "handbook-docs"]
    );
    assert!(account.resources.iter().any(|r| r.id == "handbook-docs"));
    assert!(
        account
            .questions
            .unwrap()
            .iter()
            .any(|q| q.id == "where-is-the-guide")
    );
    assert_eq!(status(&pair.native).await["state"], "synced");
    assert!(pair.native.state.running.lock().await.is_none());
}

#[tokio::test]
async fn notes_follow_the_account_and_a_deleted_note_stays_deleted() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    configure(&pair.coordinator);
    assert!(sync(&pair.native, json!({})).await.ok);
    let invoice = notes_of(&pair.native)[0].clone();

    // A note saved here is decided by the coordinator and kept in both places.
    let saved = call(
        &pair.native,
        Window,
        "notes_save",
        json!({"note":{"title":"Payment day","text":"Payments go out on Fridays.","contexts":["billing"]}}),
    )
    .await;
    assert!(saved.ok, "{:?}", saved.message);
    let id = saved.data["id"].as_str().unwrap().to_owned();
    assert!(notes_of(&pair.coordinator).iter().any(|n| n.id == id));
    assert_eq!(notes_of(&pair.native).len(), 2);
    assert_eq!(status(&pair.native).await["state"], "synced");

    // The account edits it; the stale edit here is a conflict, not an overwrite.
    let coordinator_edit = call(
        &pair.coordinator,
        Window,
        "notes_save",
        json!({"note":{"id":id,"title":"Payment day","text":"Payments go out on Thursdays.","contexts":["billing"]}}),
    )
    .await;
    assert!(coordinator_edit.ok);
    let stale = call(
        &pair.native,
        Window,
        "notes_save",
        json!({"note":{"id":id,"title":"Payment day","text":"Stale text.","contexts":["billing"]}}),
    )
    .await;
    assert!(!stale.ok);
    assert_eq!(stale.code, "state_conflict");
    assert!(
        notes_of(&pair.coordinator)
            .iter()
            .any(|n| n.text.contains("Thursdays"))
    );
    assert!(sync(&pair.native, json!({})).await.ok);
    assert!(
        notes_of(&pair.native)
            .iter()
            .any(|n| n.text.contains("Thursdays"))
    );

    // A removal is decided the same way and does not come back with the next synchronization.
    let removed = call(
        &pair.native,
        Window,
        "notes_remove",
        json!({"id":invoice.id}),
    )
    .await;
    assert!(removed.ok, "{:?}", removed.message);
    assert!(
        !notes_of(&pair.coordinator)
            .iter()
            .any(|n| n.id == invoice.id)
    );
    assert!(sync(&pair.native, json!({})).await.ok);
    assert!(!notes_of(&pair.native).iter().any(|n| n.id == invoice.id));
    // The account deletes the other one; the installation follows.
    assert!(
        call(&pair.coordinator, Window, "notes_remove", json!({"id":id}))
            .await
            .ok
    );
    assert!(sync(&pair.native, json!({})).await.ok);
    assert!(notes_of(&pair.native).is_empty());
    // Sensitive data is refused by the coordinator exactly as locally.
    let sensitive = call(
        &pair.native,
        Window,
        "notes_save",
        json!({"note":{"title":"Contact","text":"Write to someone@example.com"}}),
    )
    .await;
    assert!(!sensitive.ok);
    assert!(notes_of(&pair.coordinator).is_empty());
}

#[tokio::test]
async fn a_different_account_or_an_older_state_never_replaces_the_loaded_configuration() {
    let root = tempfile::tempdir().unwrap();
    let swap: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let rewrite = swap.clone();
    let pair = pair(
        root.path(),
        Some(Arc::new(move |_, value: &mut Value| {
            if let Some(source) = rewrite.lock().unwrap().clone()
                && value["data"].get("source").is_some()
            {
                value["data"]["source"] = json!(source);
            }
        })),
    )
    .await;
    configure(&pair.coordinator);
    assert!(sync(&pair.native, json!({})).await.ok);
    let loaded = config_of(&pair.native).llm.style;

    // The coordinator starts serving another account (Bob's): nothing changes, nothing is mixed.
    *swap.lock().unwrap() = Some("b".repeat(32));
    let mut changed = config_of(&pair.coordinator);
    changed.llm.style = "Bob's style".into();
    commit_settings(
        &pair.coordinator.state,
        &changed,
        &map_of(&pair.coordinator),
        "",
    )
    .unwrap();
    let other = sync(&pair.native, json!({})).await;
    assert!(!other.ok);
    assert_eq!(other.code, "state_conflict");
    assert_eq!(config_of(&pair.native).llm.style, loaded);
    assert_eq!(status(&pair.native).await["state"], "error");

    // Signing in to another account (Bob's) discards what was loaded from Alice's: the two are
    // never merged, and an installation configured by Alice's account does not silently adopt
    // Bob's.
    *swap.lock().unwrap() = None;
    connect(
        &root.path().join("native").join("profile"),
        2002,
        &pair.machine,
    );
    assert_eq!(status(&pair.native).await["state"], "never");
    let bob = sync(&pair.native, json!({})).await;
    assert!(!bob.ok);
    assert_eq!(bob.code, "state_conflict");
    assert_eq!(config_of(&pair.native).llm.style, loaded);
    connect(
        &root.path().join("native").join("profile"),
        1001,
        &pair.machine,
    );

    // Leaving the account forgets what was loaded from it; restored settings stay as a profile.
    pair.native.state.portable.forget();
    assert_eq!(status(&pair.native).await["state"], "never");
    assert_eq!(config_of(&pair.native).llm.style, loaded);
}

#[tokio::test]
async fn coordinator_records_are_read_through_an_account_authorized_operation_only() {
    let root = tempfile::tempdir().unwrap();
    let pair = pair(root.path(), None).await;
    let records = call(
        &pair.native,
        Local,
        "portable_records",
        json!({"kind":"activity_status"}),
    )
    .await;
    assert!(records.ok, "{:?}", records.message);
    assert_eq!(records.data["source"], "coordinator");
    assert!(records.data["record"]["days"].is_array());
    let unknown = call(
        &pair.native,
        Local,
        "portable_records",
        json!({"kind":"messages"}),
    )
    .await;
    assert!(!unknown.ok);
    assert_eq!(unknown.code, "invalid_input");
    // The coordinator itself has no one to ask.
    let own = call(
        &pair.coordinator,
        Local,
        "portable_records",
        json!({"kind":"activity_status"}),
    )
    .await;
    assert!(!own.ok);
    // A machine credential cannot use the client operations.
    let machine = call(&pair.coordinator, Machine, "portable_sync", json!({})).await;
    assert!(!machine.ok);
    // An older installation host is refused instead of falling back.
    assert!(
        text(&call(&pair.native, Local, "portable_sync", json!({"value":"x"})).await)
            .contains("Invalid arguments")
    );
}
