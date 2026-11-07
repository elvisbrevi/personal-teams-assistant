use super::*;
use crate::store::settings::SettingsStore;
use crate::workflows::{Agent, LauncherRef, Repository, Stage};
use authority::Transport::{Local, Machine, Window};

const REPO: &str = "0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f";
const DIGEST: &str = "4be0a7134be0a7134be0a7134be0a7134be0a7134be0a7134be0a7134be0a713";

struct TestShell;
impl Shell for TestShell {
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
fn workflow(id: &str, machine: &str) -> Workflow {
    Workflow {
        id: id.into(),
        name: "Payments".into(),
        organization: "https://dev.azure.com/example".into(),
        project: "Payments".into(),
        repositories: vec![Repository {
            id: REPO.into(),
            name: "payments-api".into(),
        }],
        machine: machine.into(),
        checkouts: BTreeMap::from([(REPO.into(), PathBuf::from("/work/payments-api"))]),
        agent: Agent {
            provider: "codex".into(),
            model: "gpt-6.1-sol".into(),
            effort: "medium".into(),
        },
        ..Default::default()
    }
}
fn instruction(id: &str) -> Value {
    json!({"id":id,"name":"Base branch","kind":"instruction","text":"Branch from feature/222."})
}
fn notification(id: &str) -> Value {
    json!({"id":id,"name":"Tell Victoria","kind":"teams_notification","stage":"done",
        "chat":{"id":"19:synthetic@thread.v2","name":"Releases"},"mentions":["Victoria"],
        "template":"{item} is released: {link}"})
}
fn launcher_block(id: &str, launcher: &str, digest: &str) -> Value {
    json!({"id":id,"name":"Release in desa","kind":"launcher","stage":"pull_requests_completed",
        "launcher":{"id":launcher,"digest":digest}})
}
fn launcher(id: &str, approved: bool) -> crate::consumers::Launcher {
    crate::consumers::Launcher {
        id: id.into(),
        description: "Synthetic release".into(),
        program: PathBuf::from("/usr/bin/true"),
        digest: DIGEST.into(),
        args: Vec::new(),
        directory: PathBuf::from("/tmp"),
        env: BTreeMap::new(),
        timeout_seconds: 60,
        created_at: 0,
        approved_at: approved.then_some(1),
    }
}
/// A workflow placed in the map as a definition the operations saved earlier would be.
fn store(host: &Arc<Host>, workflows: Vec<Workflow>) {
    let mut map = host.state.settings.map().unwrap();
    map.workflows = workflows;
    host.state.settings.save_map(&map).unwrap();
}

#[tokio::test]
async fn acting_blocks_are_authorized_when_they_are_saved() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let saved = call(
        &host,
        Local,
        "workflow_block_save",
        json!({"block":instruction("base")}),
    )
    .await;
    assert!(saved.ok, "{:?}", saved.message);

    // A notification needs confirmation, and both the refusal and the reply show chat and text.
    let unconfirmed = call(
        &host,
        Local,
        "workflow_block_save",
        json!({"block":notification("notify")}),
    )
    .await;
    assert_eq!(unconfirmed.code, "invalid_input");
    let message = unconfirmed.message.unwrap();
    assert!(
        message.contains("Releases") && message.contains("{item} is released"),
        "{message}"
    );
    let confirmed = call(
        &host,
        Local,
        "workflow_block_save",
        json!({"block":notification("notify"),"confirmed":true}),
    )
    .await;
    assert!(confirmed.ok, "{:?}", confirmed.message);
    assert_eq!(confirmed.data["chat"]["name"], "Releases");
    assert_eq!(confirmed.data["template"], "{item} is released: {link}");
    // Without a Microsoft session here, the chat is saved with a warning that it was not checked.
    let warnings = confirmed.data["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0]
            .as_str()
            .unwrap()
            .contains("could not be checked")
    );

    // A launcher block: only the owner at the GUI, for a launcher approved with that digest.
    consumers::add_test_launcher(&host.state, launcher("release-desa", true));
    consumers::add_test_launcher(&host.state, launcher("waiting", false));
    let block = launcher_block("desa", "release-desa", DIGEST);
    for transport in [Local, Machine] {
        let refused = call(
            &host,
            transport,
            "workflow_block_save",
            json!({"block":block,"confirmed":true}),
        )
        .await;
        assert!(!refused.ok, "{transport:?}");
    }
    let from_cli = call(
        &host,
        Local,
        "workflow_block_save",
        json!({"block":block,"confirmed":true}),
    )
    .await;
    assert_eq!(from_cli.code, "state_conflict");
    // A launcher block is confirmed too, even by the owner at the GUI.
    let unconfirmed = call(&host, Window, "workflow_block_save", json!({"block":block})).await;
    assert_eq!(unconfirmed.code, "invalid_input");
    assert!(unconfirmed.message.unwrap().contains("release-desa"));
    for (block, code) in [
        (
            launcher_block("desa", "release-desa", &"b".repeat(64)),
            "state_conflict",
        ),
        (launcher_block("desa", "waiting", DIGEST), "state_conflict"),
        (launcher_block("desa", "missing", DIGEST), "invalid_input"),
    ] {
        let refused = call(
            &host,
            Window,
            "workflow_block_save",
            json!({"block":block,"confirmed":true}),
        )
        .await;
        assert_eq!(refused.code, code, "{:?}", refused.message);
    }
    let map = host.state.settings.map().unwrap();
    assert!(!map.workflow_blocks.iter().any(|b| b.id == "desa"));
    let approved = call(
        &host,
        Window,
        "workflow_block_save",
        json!({"block":block,"confirmed":true}),
    )
    .await;
    assert!(approved.ok, "{:?}", approved.message);
    let listed = call(&host, Local, "workflow_blocks_list", json!({})).await;
    let ids: Vec<&str> = listed.data["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["base", "notify", "desa"]);
    assert!(
        listed.data["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["updated_at"].as_i64().unwrap() > 0)
    );
}

#[tokio::test]
async fn a_block_in_use_and_an_unconfirmed_workflow_removal_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let own = implementation::test_machine(&host, None).await;
    call(
        &host,
        Local,
        "workflow_block_save",
        json!({"block":instruction("base")}),
    )
    .await;
    let mut w = workflow("payments", &own);
    w.blocks = vec!["base".into()];
    store(&host, vec![w]);
    let refused = call(&host, Local, "workflow_block_remove", json!({"id":"base"})).await;
    assert_eq!(refused.code, "state_conflict");
    assert!(refused.message.unwrap().contains("payments"));
    let listed = call(&host, Local, "workflow_blocks_list", json!({})).await;
    assert_eq!(listed.data["used_by"]["base"], json!(["payments"]));
    let shown = call(&host, Local, "workflow_show", json!({"id":"payments"})).await;
    assert_eq!(shown.data["blocks"][0]["id"], "base");
    assert_eq!(shown.data["editable"], true);

    let unconfirmed = call(&host, Local, "workflow_remove", json!({"id":"payments"})).await;
    assert_eq!(unconfirmed.code, "invalid_input");
    assert_eq!(host.state.settings.map().unwrap().workflows.len(), 1);
    let removed = call(
        &host,
        Local,
        "workflow_remove",
        json!({"id":"payments","confirmed":true}),
    )
    .await;
    assert!(removed.ok, "{:?}", removed.message);
    let removed = call(&host, Local, "workflow_block_remove", json!({"id":"base"})).await;
    assert!(removed.ok, "{:?}", removed.message);
    assert!(
        host.state
            .settings
            .map()
            .unwrap()
            .workflow_blocks
            .is_empty()
    );
    let unknown = call(&host, Local, "workflow_block_remove", json!({"id":"base"})).await;
    assert_eq!(unknown.code, "invalid_input");
}

#[tokio::test]
async fn enabling_changes_only_enabled_and_other_editors_keep_the_workflows() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let own = implementation::test_machine(&host, None).await;
    let mut w = workflow("payments", &own);
    w.updated_at = 42;
    store(&host, vec![w.clone()]);
    let enabled = call(
        &host,
        Local,
        "workflow_save",
        json!({"id":"payments","enabled":true}),
    )
    .await;
    assert!(enabled.ok, "{:?}", enabled.message);
    let after = host.state.settings.map().unwrap().workflows;
    assert_eq!(
        after,
        vec![Workflow {
            enabled: true,
            ..w.clone()
        }]
    );
    let disabled = call(
        &host,
        Local,
        "workflow_save",
        json!({"id":"payments","enabled":false}),
    )
    .await;
    assert!(disabled.ok);
    assert_eq!(
        host.state.settings.map().unwrap().workflows,
        vec![w.clone()]
    );
    let unknown = call(
        &host,
        Local,
        "workflow_save",
        json!({"id":"other","enabled":true}),
    )
    .await;
    assert_eq!(unknown.code, "invalid_input");

    // A settings editor that dropped or changed them leaves the stored ones in place.
    let snapshot = call(&host, Local, "snapshot", json!({})).await;
    assert!(snapshot.ok, "{:?}", snapshot.message);
    let mut map = snapshot.data["map"].clone();
    map.as_object_mut().unwrap().remove("workflows");
    map["workflow_blocks"] = json!([instruction("sneaky")]);
    let saved = call(
        &host,
        Local,
        "save_settings",
        json!({"config":snapshot.data["config"],"map":map,"tunnel_config":""}),
    )
    .await;
    assert!(saved.ok, "{:?}", saved.message);
    let map = host.state.settings.map().unwrap();
    assert_eq!(map.workflows, vec![w]);
    assert!(map.workflow_blocks.is_empty());
}

#[tokio::test]
async fn a_save_is_refused_before_anything_is_read_when_the_map_alone_refuses_it() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let own = implementation::test_machine(&host, None).await;
    let save = |w: &Workflow| json!({"workflow":w});
    let mut w = workflow("payments", &own);
    w.instructions = "x".repeat(crate::workflows::MAX_INSTRUCTIONS + 1);
    let refused = call(&host, Local, "workflow_save", save(&w)).await;
    assert_eq!(refused.code, "invalid_input");
    assert!(refused.message.unwrap().contains("8000"));
    let mut w = workflow("payments", &own);
    w.blocks = vec!["missing".into()];
    let refused = call(&host, Local, "workflow_save", save(&w)).await;
    assert_eq!(refused.code, "invalid_input");
    assert!(refused.message.unwrap().contains("missing"));
    store(
        &host,
        (0..crate::workflows::MAX_WORKFLOWS)
            .map(|n| workflow(&format!("w{n}"), &own))
            .collect(),
    );
    let refused = call(
        &host,
        Local,
        "workflow_save",
        save(&workflow("one-more", &own)),
    )
    .await;
    assert_eq!(refused.code, "invalid_input");
    assert!(refused.message.unwrap().contains("50"));
    store(&host, Vec::new());
    // A machine credential never reaches the workflow operations.
    let refused = call(
        &host,
        Machine,
        "workflow_save",
        save(&workflow("payments", &own)),
    )
    .await;
    assert_eq!(refused.code, "authority_denied");
    let refused = call(&host, Machine, "workflows_list", json!({})).await;
    assert_eq!(refused.code, "authority_denied");
    // An unknown machine is refused before the Azure catalog is read; then the source is needed.
    let stranger = uuid::Uuid::new_v4().to_string();
    let refused = call(
        &host,
        Local,
        "workflow_save",
        save(&workflow("payments", &stranger)),
    )
    .await;
    assert_eq!(refused.code, "invalid_input");
    assert!(
        refused
            .message
            .unwrap()
            .contains("not an execution machine")
    );
    let refused = call(
        &host,
        Local,
        "workflow_save",
        save(&workflow("payments", &own)),
    )
    .await;
    assert_eq!(refused.code, "not_ready", "{:?}", refused.message);
    assert!(host.state.settings.map().unwrap().workflows.is_empty());
    let refused = call(
        &host,
        Local,
        "workflow_save",
        json!({"workflow":{"id":"x","unknown_field":true}}),
    )
    .await;
    assert_eq!(refused.code, "invalid_input");
    let refused = call(
        &host,
        Local,
        "workflow_repositories",
        json!({"project":"Payments"}),
    )
    .await;
    assert_eq!(refused.code, "not_ready");
}

#[tokio::test]
async fn checkouts_are_inspected_by_their_machine_and_an_older_worker_is_unknown() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let own = implementation::test_machine(&host, None).await;
    let allowed = root.path().join("allowed");
    let repo = allowed.join("payments-api");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(
        repo.join(".git/config"),
        "[remote \"origin\"]\n\turl = git@ssh.dev.azure.com:v3/example/Payments/payments-api\n",
    )
    .unwrap();
    let outside = root.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    implementation::test_machine(&host, Some(vec![allowed.canonicalize().unwrap()])).await;
    let facts = implementation::machine_facts(&host, &own)
        .await
        .unwrap()
        .unwrap();
    assert!(facts.own && facts.inspect);
    let canonical = repo.canonicalize().unwrap();
    match one(inspect(&host, &own, &facts, std::slice::from_ref(&canonical)).await) {
        Inspection::Answered {
            directory,
            link,
            checkout,
            origin,
        } => {
            assert!(!link);
            assert_eq!(directory, implementation::paths::plain(&canonical));
            assert!(checkout);
            assert_eq!(
                origin.as_deref(),
                Some("git@ssh.dev.azure.com:v3/example/Payments/payments-api")
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        one(inspect(&host, &own, &facts, std::slice::from_ref(&outside)).await),
        Inspection::Outside
    );
    // Every checkout of a workflow is asked in one call, answers in order.
    let all = inspect(
        &host,
        &own,
        &facts,
        &[canonical.clone(), outside.clone(), allowed.join("missing")],
    )
    .await;
    assert!(matches!(
        all[0],
        Inspection::Answered { checkout: true, .. }
    ));
    assert_eq!(all[1..], [Inspection::Outside, Inspection::Missing]);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, allowed.join("escape")).unwrap();
        assert_eq!(
            one(inspect(&host, &own, &facts, &[allowed.join("escape")]).await),
            Inspection::Outside
        );
    }
    // A worker of a version without checkout_inspect is never asked: its answer is unknown.
    let worker = uuid::Uuid::new_v4().to_string();
    implementation::register(&host, &json!({"id":worker,"name":"Worker","os":"linux"}))
        .await
        .unwrap();
    let older = implementation::machine_facts(&host, &worker)
        .await
        .unwrap()
        .unwrap();
    assert!(!older.own && !older.inspect);
    let online = implementation::MachineFacts {
        online: true,
        ..older
    };
    assert!(matches!(
        one(inspect(&host, &worker, &online, std::slice::from_ref(&canonical)).await),
        Inspection::Unknown(reason) if reason.contains("version")
    ));
    let offline = implementation::MachineFacts {
        online: false,
        inspect: true,
        ..online
    };
    assert!(matches!(
        one(inspect(&host, &worker, &offline, std::slice::from_ref(&canonical)).await),
        Inspection::Unknown(reason) if reason.contains("offline")
    ));
    assert!(
        implementation::machine_facts(&host, &uuid::Uuid::new_v4().to_string())
            .await
            .unwrap()
            .is_none()
    );
}

fn one(mut inspections: Vec<Inspection>) -> Inspection {
    assert_eq!(inspections.len(), 1);
    inspections.remove(0)
}

struct SyntheticToken;
#[async_trait::async_trait]
impl crate::adapters::oauth::AccessToken for SyntheticToken {
    async fn access_token(&self) -> Result<String> {
        Ok("synthetic-graph-token".into())
    }
}

#[tokio::test]
async fn a_notification_names_only_a_chat_the_user_belongs_to() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/me/chats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
            {"id":"19:synthetic@thread.v2","topic":"Releases","chatType":"group","members":[]}
        ]})))
        .mount(&server)
        .await;
    // The session the running assistant would hold; a settings commit drops it, so each save
    // below starts with it again.
    let graph = Arc::new(crate::adapters::graph::Graph {
        client: reqwest::Client::new(),
        token: Arc::new(SyntheticToken),
        base_url: server.uri(),
        config: Arc::new(host.state.settings.config().unwrap()),
        store: Arc::new(crate::state::Store::open(&root.path().join("graph.db")).unwrap()),
        client_state: "synthetic-client-state-of-32-chars".into(),
    });
    let session = || async { *host.state.activity_graph.lock().await = Some(graph.clone()) };
    session().await;
    let member = call(
        &host,
        Local,
        "workflow_block_save",
        json!({"block":notification("notify"),"confirmed":true}),
    )
    .await;
    assert!(member.ok, "{:?}", member.message);
    assert_eq!(member.data["warnings"], json!([]));
    let mut other = notification("other");
    other["chat"] = json!({"id":"19:elsewhere@thread.v2","name":"Not mine"});
    session().await;
    let refused = call(
        &host,
        Local,
        "workflow_block_save",
        json!({"block":other,"confirmed":true}),
    )
    .await;
    assert_eq!(refused.code, "invalid_input");
    assert!(refused.message.unwrap().contains("not a member"));
    let map = host.state.settings.map().unwrap();
    assert_eq!(map.workflow_blocks.len(), 1);
    // The personal chat is always the user's own.
    let mut personal = notification("personal");
    personal["chat"] = json!({"id":"48:notes","name":"Notes"});
    let saved = call(
        &host,
        Local,
        "workflow_block_save",
        json!({"block":personal,"confirmed":true}),
    )
    .await;
    assert!(saved.ok, "{:?}", saved.message);
}

#[tokio::test]
async fn checkout_keys_are_normalized_before_anything_is_read() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let own = implementation::test_machine(&host, None).await;
    let mut w = workflow("payments", &own);
    w.checkouts
        .insert(REPO.to_uppercase(), PathBuf::from("/work/other"));
    let refused = call(&host, Local, "workflow_save", json!({"workflow":w})).await;
    assert_eq!(refused.code, "invalid_input");
    assert!(refused.message.unwrap().contains("two checkouts"));
}

#[test]
fn a_confirmation_names_what_the_block_does() {
    let block: Block = serde_json::from_value(notification("notify")).unwrap();
    let text = describe(&block);
    assert!(text.contains("done") && text.contains("Releases") && text.contains("Victoria"));
    let block = Block {
        id: "desa".into(),
        name: "Release".into(),
        kind: Kind::Launcher,
        stage: Some(Stage::Failed),
        launcher: Some(LauncherRef {
            id: "release-desa".into(),
            digest: DIGEST.into(),
        }),
        ..Default::default()
    };
    assert!(describe(&block).contains(DIGEST));
}
