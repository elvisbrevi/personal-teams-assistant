//! The director's progress summaries and their sending, with a fake director CLI (the planning
//! settings, read from a script) and a mocked Graph.
use super::super::record::{BlockOutcome, Child, Implementation, Item, State as Phase};
use super::*;
use crate::ado::workflow::PullRequest;
use crate::workflows::{Kind, Repository, Stage, Workflow};
use authority::Transport::Local;
use wiremock::{
    Mock, MockServer, Request as MockRequest, ResponseTemplate,
    matchers::{method, path},
};

const ITEM_URL: &str = "https://dev.azure.com/example/Payments/_workitems/edit/41";
const WEB_URL: &str = "https://dev.azure.com/example/Payments/_workitems/edit/101";
const API_URL: &str = "https://dev.azure.com/example/Payments/_workitems/edit/102";
const PULL_URL: &str = "https://dev.azure.com/example/Payments/_git/payments-web/pullrequest/7";
const SESSION: &str = "44444444-4444-4444-8444-444444444444";
const CHAT: &str = "19:team@thread.v2";
const OTHER_CHAT: &str = "19:other@thread.v2";

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
async fn call(host: &Arc<Host>, method: &str, args: Value) -> control::Reply {
    call_as(host, Local, method, args).await
}
async fn call_as(
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

/// A run in the middle of its work, as the engine records it, with things no summary may see:
/// the workflow's instructions and the checkout directory.
fn record(host: &Arc<Host>, machine: &str) -> Run {
    let item = Item {
        provider: "azure".into(),
        id: 41,
        project: "Payments".into(),
        organization: "https://dev.azure.com/example".into(),
        url: ITEM_URL.into(),
        title: "Receipt downloads".into(),
        kind: "User Story".into(),
        category: "requirement".into(),
        state: "Ready".into(),
        source: None,
    };
    let mut run = Run::new(item, 1_700_000_000);
    run.workflow = Some("payments".into());
    run.definition = Some(Workflow {
        id: "payments".into(),
        name: "Payments flow".into(),
        instructions: "SECRET-INSTRUCTIONS: never print this.".into(),
        machine: machine.into(),
        ..Default::default()
    });
    run.classification =
        Some(json!({"workflow":"payments","outcome":"chosen","reason":"Habla de pagos."}));
    run.advance(Phase::Planning, 1_700_000_100, "assigned: payments");
    run.advance(Phase::PlanReview, 1_700_000_200, "plan ready");
    run.advance(Phase::CreatingChildren, 1_700_000_300, "confirmed");
    run.advance(Phase::Coding, 1_700_000_400, "created");
    let repository = Repository {
        id: "0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f".into(),
        name: "payments-web".into(),
    };
    run.children = vec![
        Child {
            id: 101,
            title: "Web form".into(),
            url: WEB_URL.into(),
            kind: "Task".into(),
            repository: Some(repository.clone()),
            directory: Some(PathBuf::from("/secret/checkout/path")),
            coding: Some(Implementation {
                id: "55555555-5555-4555-8555-555555555555".into(),
                machine: machine.into(),
                started: true,
                state: Some("done".into()),
            }),
            branch: Some("feature/web-form".into()),
            pull_requests: vec![PullRequest {
                id: 7,
                repository: repository.id.clone(),
                title: "Web form".into(),
                status: "active".into(),
                url: PULL_URL.into(),
                ..Default::default()
            }],
            outcome: Some("done".into()),
            ..Default::default()
        },
        Child {
            id: 102,
            title: "API endpoint".into(),
            url: API_URL.into(),
            kind: "Task".into(),
            ..Default::default()
        },
    ];
    run.block_outcomes = vec![BlockOutcome {
        block: "notify".into(),
        kind: Kind::TeamsNotification,
        stage: Some(Stage::ChildDone),
        outcome: "sent".into(),
        at: 1_700_000_500,
        occurrence: "child_done:101".into(),
        reason: None,
        exit_code: None,
        duration_ms: None,
        message_id: Some("1700000500000".into()),
    }];
    run.attention = Some("Waiting for the pull requests of the children.".into());
    assert!(host.state.workflows.db.insert(&run).unwrap());
    run
}

fn director(host: &Arc<Host>, machine: &str) {
    let mut map = host.state.settings.map().unwrap();
    map.workflow_settings.director = Some(crate::workflows::director::Director {
        machine: machine.into(),
        provider: "codex".into(),
        model: "test-model".into(),
        effort: "low".into(),
    });
    host.state.settings.save_map(&map).unwrap();
}

/// Install a fake Codex CLI that answers with `text` as the agent's message, recording what it
/// was asked.
fn fake_director(log: &Path, text: &str) {
    std::fs::create_dir_all(log).unwrap();
    let thread = json!({"type":"thread.started","thread_id":SESSION});
    let message = json!({"type":"item.completed","item":{"type":"agent_message","text":text}});
    std::fs::write(log.join("reply.jsonl"), format!("{thread}\n{message}\n")).unwrap();
    let cli = log.join("codex");
    crate::llm::agent::install_fake_cli(
        &cli,
        &format!(
            "#!/bin/sh\n[ \"$1\" = exec ] || exit 3\nprintf '%s\\n' \"$@\" > '{log}/arguments'\ncat > '{log}/prompt'\necho x >> '{log}/asked'\ncat '{log}/reply.jsonl'\n",
            log = log.display()
        ),
    );
    crate::llm::implementation::use_test_cli(Some(cli));
}
fn answer(summary: &str) -> String {
    json!({"summary": summary}).to_string()
}
fn stored(host: &Arc<Host>, run: &str) -> Run {
    host.state.workflows.db.get(run).unwrap().unwrap()
}

#[tokio::test]
async fn a_summary_is_written_from_the_run_record_only_and_stored_with_its_session() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let own = implementation::test_machine(&host, None).await;
    director(&host, &own);
    let run = record(&host, &own);
    let log = root.path().join("log");
    // SAFETY: test-only variable read by no other test.
    unsafe { std::env::set_var("PTA_SUMMARY_TEST_SECRET", "must-not-leak") };
    fake_director(
        &log,
        &answer(
            "La HU #41 avanza: el hijo #101 «Web form» tiene la PR !7 abierta. \
             Ver [la HU](https://evil.example/hu), https://evil.example/pr y <https://evil.example/x>. \
             Avisa a @[Victoria] cuando termine.",
        ),
    );
    let reply = call(&host, "workflow_summary", json!({"run":run.id})).await;
    crate::llm::implementation::use_test_cli(None);
    assert!(reply.ok, "{:?}", reply.message);
    let summary = &reply.data["summary"];
    let text = summary["text"].as_str().unwrap();

    // Only the verified links: the item always, and the child and pull request it names; the
    // URLs the model wrote are gone, and so is its mention marker.
    assert!(!text.contains("evil.example"), "{text}");
    assert!(!text.contains("@["), "{text}");
    assert!(text.contains("@Victoria"));
    for link in [
        format!("]({ITEM_URL})"),
        format!("]({WEB_URL})"),
        format!("]({PULL_URL})"),
    ] {
        assert!(text.contains(&link), "{link} in {text}");
    }
    assert!(
        !text.contains(API_URL),
        "unnamed children are not linked: {text}"
    );
    let urls = regex::Regex::new(r"https?://[^\s)]+").unwrap();
    for url in urls.find_iter(text) {
        assert!(
            [ITEM_URL, WEB_URL, PULL_URL].contains(&url.as_str()),
            "{text}"
        );
    }
    assert!(text.chars().count() <= MAX_CHARS);
    assert!(text.contains("**Enlaces**"), "{text}");

    // Stored on the run with its timestamp, model and native session ID.
    let after = stored(&host, &run.id);
    assert_eq!(after.summaries.len(), 1);
    let kept = &after.summaries[0];
    assert_eq!(kept, summary);
    assert_eq!(kept["model"], "test-model");
    assert_eq!(kept["provider"], "codex");
    assert_eq!(kept["session_id"], SESSION);
    assert!(kept["created_at"].as_i64().unwrap() > 0);
    // The rest of the record is untouched.
    assert_eq!(after.state, run.state);
    assert_eq!(after.timeline, run.timeline);

    // The session is in Agent history, as a summary of that run, read-only.
    let history = call(&host, "implementation_history", json!({})).await;
    assert!(history.ok, "{:?}", history.message);
    let listed = history.data["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == kept["id"])
        .cloned()
        .expect("the summary session is in Agent history");
    assert_eq!(listed["purpose"], "summary");
    assert_eq!(listed["session_id"], SESSION);
    assert_eq!(listed["item"]["url"], ITEM_URL);
    assert_eq!(listed["workflow_run"], run.id.as_str());
    assert_eq!(listed["permission_mode"], "read-only");
    assert_eq!(listed["state"], "done");
    assert!(listed["finished_at"].is_string());

    // The director read the record: direct prompt first, the summarize task code appended, the
    // rules and the closed schema; the item, stages, children, pull request and block outcome;
    // nothing the record does not hold for it (instructions, directories, secrets).
    let prompt = std::fs::read_to_string(log.join("prompt")).unwrap();
    let direct = crate::llm::prompts::WORKFLOW_DIRECT;
    assert!(prompt.starts_with(direct));
    assert!(prompt.find("Task: summarize") > Some(direct.len()));
    assert!(prompt.contains("Write no URLs") && prompt.contains("\"required\":[\"summary\"]"));
    for fact in [
        "Receipt downloads",
        ITEM_URL,
        "Payments flow",
        "plan_review",
        "creating_children",
        "Web form",
        WEB_URL,
        "feature/web-form",
        PULL_URL,
        "\"status\":\"active\"",
        "\"block\":\"notify\"",
        "\"outcome\":\"sent\"",
        "Waiting for the pull requests",
        "Habla de pagos.",
        "\"language\":\"es\"",
    ] {
        assert!(prompt.contains(fact), "{fact} missing from the input");
    }
    for hidden in [
        "SECRET-INSTRUCTIONS",
        "/secret/checkout/path",
        "must-not-leak",
    ] {
        assert!(!prompt.contains(hidden), "{hidden} reached the director");
    }
    let arguments = std::fs::read_to_string(log.join("arguments")).unwrap();
    assert!(arguments.contains("read-only\n") && !arguments.contains("danger-full-access"));
    // Classifying and summarizing are different sessions: nothing was started or written.
    assert_eq!(
        std::fs::read_to_string(log.join("asked"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn the_input_is_the_run_record_bounded() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let mut run = record(&host, "machine");
    run.item.title = "T".repeat(900);
    run.attention = Some("a".repeat(5000));
    run.children = (0..80)
        .map(|n| Child {
            id: 1000 + n,
            title: "c".repeat(500),
            url: format!(
                "https://dev.azure.com/example/Payments/_workitems/edit/{}",
                1000 + n
            ),
            ..Default::default()
        })
        .collect();
    let value = input(&run, crate::config::Language::En, 1_700_001_000);
    assert_eq!(value["language"], "en");
    assert_eq!(
        value["item"]["title"].as_str().unwrap().chars().count(),
        500
    );
    assert_eq!(value["attention"].as_str().unwrap().chars().count(), 1000);
    assert_eq!(value["children"].as_array().unwrap().len(), 50);
    assert_eq!(
        value["children"][0]["title"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        200
    );
    assert_eq!(value["state"], "coding");
    assert_eq!(
        value["stages"].as_array().unwrap().len(),
        run.timeline.len()
    );
    assert!(
        value["stages"][0]["started_at"]
            .as_str()
            .unwrap()
            .starts_with("2023-11-14")
    );
    // Only these keys: nothing of the definition or the engine's bookkeeping.
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for key in [
        "definition",
        "ledger",
        "planning",
        "retry_at",
        "summaries",
        "summary_sends",
    ] {
        assert!(!keys.contains(&key), "{key}");
    }
}

#[tokio::test]
async fn a_malformed_or_empty_answer_is_an_error_and_stores_nothing() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let own = implementation::test_machine(&host, None).await;
    director(&host, &own);
    let run = record(&host, &own);
    let cases = [
        ("not json at all, just prose", "not the JSON"),
        (r#"{"summary":""}"#, "empty"),
        (r#"{"summary":"   \n  "}"#, "empty"),
        // Only links and markers: nothing left once the model's own are dropped.
        (
            r#"{"summary":"[x](https://evil.example) @[Victoria]"}"#,
            "x @Victoria",
        ),
        (r#"{"summary":"https://evil.example/only-a-link"}"#, "empty"),
        (
            r#"{"workflow":"payments","reason":"wrong task"}"#,
            "not the JSON",
        ),
        (r#"{"summary":"ok","extra":1}"#, "not the JSON"),
        (r#"{"summary":42}"#, "not the JSON"),
    ];
    for (n, (reply, expected)) in cases.into_iter().enumerate() {
        let log = root.path().join(format!("log{n}"));
        fake_director(&log, reply);
        let result = call(&host, "workflow_summary", json!({"run":run.id})).await;
        crate::llm::implementation::use_test_cli(None);
        if n == 3 {
            // Words that survive are a summary.
            assert!(result.ok, "{:?}", result.message);
            assert!(
                result.data["summary"]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with(expected)
            );
            continue;
        }
        assert!(!result.ok, "case {n}: {reply}");
        let message = result.message.unwrap();
        assert!(message.contains(expected), "case {n}: {message}");
        assert!(
            message.contains("No summary was stored"),
            "case {n}: {message}"
        );
    }
    // Only the one that had words was stored.
    assert_eq!(stored(&host, &run.id).summaries.len(), 1);
    // The sessions that did not give a summary are recorded as failed.
    let history = call(&host, "implementation_history", json!({})).await;
    let runs = history.data["runs"].as_array().unwrap().clone();
    let summaries: Vec<&Value> = runs.iter().filter(|r| r["purpose"] == "summary").collect();
    assert_eq!(summaries.len(), cases.len());
    assert_eq!(
        summaries.iter().filter(|r| r["state"] == "failed").count(),
        cases.len() - 1
    );
    // A session with no answer at all is no summary either.
    let log = root.path().join("silent");
    std::fs::create_dir_all(&log).unwrap();
    let cli = log.join("codex");
    crate::llm::agent::install_fake_cli(&cli, "#!/bin/sh\ncat > /dev/null\n");
    crate::llm::implementation::use_test_cli(Some(cli));
    let result = call(&host, "workflow_summary", json!({"run":run.id})).await;
    crate::llm::implementation::use_test_cli(None);
    assert!(!result.ok);
    assert_eq!(stored(&host, &run.id).summaries.len(), 1);
}

#[tokio::test]
async fn a_summary_needs_a_run_a_director_and_the_coordinator_and_keeps_the_last_twenty() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let own = implementation::test_machine(&host, None).await;
    let run = record(&host, &own);
    let unknown = call(
        &host,
        "workflow_summary",
        json!({"run":uuid::Uuid::new_v4().to_string()}),
    )
    .await;
    assert_eq!(unknown.code, "invalid_input");
    let malformed = call(&host, "workflow_summary", json!({"run":"nope"})).await;
    assert_eq!(malformed.code, "invalid_input");
    let extra = call(&host, "workflow_summary", json!({"run":run.id,"chat":"x"})).await;
    assert_eq!(extra.code, "invalid_input");
    let none = call(&host, "workflow_summary", json!({"run":run.id})).await;
    assert_eq!(none.code, "not_ready");
    assert!(none.message.unwrap().contains("workflow director"));

    director(&host, &own);
    let mut with_old = stored(&host, &run.id);
    with_old.summaries = (0..MAX_SUMMARIES)
        .map(|n| json!({"id":format!("old-{n}"),"text":format!("Old {n}"),"created_at":n}))
        .collect();
    host.state.workflows.db.save(&with_old).unwrap();
    fake_director(&root.path().join("log"), &answer("Nuevo resumen."));
    let reply = call(&host, "workflow_summary", json!({"run":run.id})).await;
    crate::llm::implementation::use_test_cli(None);
    assert!(reply.ok, "{:?}", reply.message);
    let kept = stored(&host, &run.id).summaries;
    assert_eq!(kept.len(), MAX_SUMMARIES);
    // The oldest was dropped and the new one is last (what a notification renders).
    assert_eq!(kept[0]["id"], "old-1");
    assert!(
        kept.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .starts_with("Nuevo resumen.")
    );
    // The whole record shows them, with the sends.
    let status = call(&host, "workflow_run_status", json!({"run":run.id})).await;
    assert_eq!(
        status.data["run"]["summaries"].as_array().unwrap().len(),
        MAX_SUMMARIES
    );
}

#[test]
fn what_the_model_wrote_is_cleaned_bounded_and_linked_by_code() {
    use crate::config::Language;
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let run = record(&host, "machine");
    // Every form of link, image, autolink, bare URL and mention marker goes; the words stay.
    let dirty = "[uno](http://a.example/x) ![img](http://a.example/i.png) <b>negrita</b> \
        https://a.example/y ftp://a.example/z www.a.example/w mailto:a@a.example [ref][1]\n\n\n\n\
        [1]: https://a.example/ref\n@[Ana Pérez] \u{e000}0\u{e001} fin\u{7}";
    let cleaned = clean(dirty, MAX_CHARS);
    for gone in [
        "http",
        "ftp",
        "www.",
        "mailto",
        "<b>",
        "@[",
        "\u{e000}",
        "\u{7}",
        "a.example",
    ] {
        assert!(!cleaned.contains(gone), "{gone}: {cleaned}");
    }
    for kept in ["uno", "img", "negrita", "@Ana Pérez", "fin", "[ref][1]"] {
        assert!(cleaned.contains(kept), "{kept}: {cleaned}");
    }
    assert!(!cleaned.contains("\n\n\n"));

    // Bounded, including the links: a long text is cut at a break and says so.
    let long = "Una línea del resumen que sigue y sigue.\n".repeat(400);
    let composed = compose(&long, &run, Language::En);
    assert!(
        composed.chars().count() <= MAX_CHARS,
        "{}",
        composed.chars().count()
    );
    assert!(composed.contains('…'));
    assert!(composed.contains("**Links**") && composed.contains(&format!("]({ITEM_URL})")));
    // A text that names every entity links all of them, each once, the item first.
    let names = compose(
        "HU #41, tarea #101, tarea 102 (API endpoint), PR !7 y pull request 7.",
        &run,
        Language::En,
    );
    let links: Vec<&str> = names.lines().filter(|l| l.starts_with("- [")).collect();
    assert_eq!(links.len(), 4, "{names}");
    assert!(links[0].contains(ITEM_URL) && links[3].contains(PULL_URL));
    // Naming nothing links the item alone; a number inside another is not a mention.
    let quiet = compose("Todo sigue en curso (#1010, #4).", &run, Language::Es);
    assert_eq!(quiet.matches("](").count(), 1, "{quiet}");
    assert!(quiet.contains("**Enlaces**"));
    // The links list itself is bounded: many children never push the text out.
    let mut many = run.clone();
    many.children = (0..60)
        .map(|n| Child {
            id: 2000 + n,
            title: format!("Child number {n} with a long title to fill the list up"),
            url: format!(
                "https://dev.azure.com/example/Payments/_workitems/edit/{}",
                2000 + n
            ),
            ..Default::default()
        })
        .collect();
    let everything: String = (0..60).map(|n| format!("#{} ", 2000 + n)).collect();
    let bounded = compose(&format!("{everything} Resumen."), &many, Language::En);
    assert!(bounded.chars().count() <= MAX_CHARS);
    assert!(bounded.contains(&format!("]({ITEM_URL})")));
    assert!(bounded.lines().filter(|l| l.starts_with("- [")).count() < 40);
}

// --- Sending -------------------------------------------------------------------------------

struct SyntheticToken;
#[async_trait::async_trait]
impl crate::adapters::oauth::AccessToken for SyntheticToken {
    async fn access_token(&self) -> Result<String> {
        Ok("synthetic-graph-token".into())
    }
}

/// A host with a run holding two stored summaries, and the running assistant's Graph session
/// pointing at a mocked Graph: the user's chats (a group with Ana and Victoria, another group,
/// a one-to-one chat), `me` and the members of the group.
struct Sending {
    root: tempfile::TempDir,
    host: Arc<Host>,
    server: MockServer,
    run: String,
    graph: Arc<crate::adapters::graph::Graph>,
}
impl Sending {
    async fn new(post: ResponseTemplate) -> Self {
        Self::with(|_| post).await
    }
    /// The POST is answered by `post`, given the host (a test may change it meanwhile).
    async fn with<R: wiremock::Respond + 'static>(post: impl FnOnce(Arc<Host>) -> R) -> Self {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let own = implementation::test_machine(&host, None).await;
        let mut run = record(&host, &own);
        run.summaries = vec![
            json!({"id":"summary-1","text":"Primer resumen: **va bien**.","created_at":1,"provider":"codex","model":"m","session_id":null}),
            json!({"id":"summary-2","text":"Segundo resumen.","created_at":2,"provider":"codex","model":"m","session_id":null}),
        ];
        host.state.workflows.db.save(&run).unwrap();
        let config = host.state.settings.config().unwrap();
        let me = config.graph.user_id.clone();
        let server = MockServer::start().await;
        let member = |id: &str, name: &str| json!({"userId":id,"displayName":name});
        let group = |id: &str, topic: &str, members: Vec<Value>| json!({"id":id,"topic":topic,"chatType":"group","members":members,"lastUpdatedDateTime":"2026-11-01T10:00:00Z"});
        let mine = member(&me, "Yo");
        let directory = json!({"value":[
            group(CHAT, "Equipo pagos", vec![mine.clone(), member("ana-id", "Ana Pérez"), member("vic-id", "Victoria Gómez"), member("vic2-id", "Victoria Soto")]),
            group(OTHER_CHAT, "Releases", vec![mine.clone(), member("lu-id", "Luis Rojas")]),
            json!({"id":"19:one@unq.gbl.spaces","chatType":"oneOnOne","members":[mine.clone(), member("ana-id", "Ana Pérez")],"lastUpdatedDateTime":"2026-10-01T10:00:00Z"}),
        ]});
        Mock::given(method("GET"))
            .and(path("/me/chats"))
            .respond_with(ResponseTemplate::new(200).set_body_json(directory))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":me})))
            .mount(&server)
            .await;
        for (chat, members) in [
            (
                CHAT,
                vec![
                    mine.clone(),
                    member("ana-id", "Ana Pérez"),
                    member("vic-id", "Victoria Gómez"),
                    member("vic2-id", "Victoria Soto"),
                ],
            ),
            (
                OTHER_CHAT,
                vec![mine.clone(), member("lu-id", "Luis Rojas")],
            ),
            (
                "19:one@unq.gbl.spaces",
                vec![mine.clone(), member("ana-id", "Ana Pérez")],
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/chats/{chat}/members")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":members})))
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .respond_with(post(host.clone()))
            .mount(&server)
            .await;
        let graph = Arc::new(crate::adapters::graph::Graph {
            client: reqwest::Client::new(),
            token: Arc::new(SyntheticToken),
            base_url: server.uri(),
            config: Arc::new(config),
            store: Arc::new(crate::state::Store::open(&root.path().join("graph.db")).unwrap()),
            client_state: "synthetic-client-state-of-32-chars".into(),
        });
        *host.state.activity_graph.lock().await = Some(graph.clone());
        Self {
            run: run.id.clone(),
            root,
            host,
            server,
            graph,
        }
    }
    async fn send(&self, args: Value) -> control::Reply {
        self.send_as(Local, args).await
    }
    async fn send_as(&self, transport: authority::Transport, args: Value) -> control::Reply {
        let mut body = json!({"run":self.run});
        for (key, value) in args.as_object().unwrap() {
            body[key] = value.clone();
        }
        call_as(&self.host, transport, "workflow_summary_send", body).await
    }
    async fn posts(&self) -> Vec<MockRequest> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.method.as_str() == "POST")
            .collect()
    }
    fn sends(&self) -> Vec<Value> {
        stored(&self.host, &self.run).summary_sends
    }
    fn events(&self) -> Vec<String> {
        crate::state::Store::inspect(&self.root.path().join("graph.db"), true, 20, None, false)
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "teams_post")
            .map(|e| e["detail"].as_str().unwrap().to_owned())
            .collect()
    }
}
fn created() -> ResponseTemplate {
    ResponseTemplate::new(201).set_body_json(json!({"id":"message-1"}))
}

#[tokio::test]
async fn a_send_posts_once_with_resolved_mentions_and_records_it() {
    let t = Sending::new(created()).await;
    let reply = t
        .send(json!({"summary":"summary-1","chat":CHAT,"mentions":["ana","Victoria Gómez"],"confirmed":true}))
        .await;
    assert!(reply.ok, "{:?}", reply.message);
    assert_eq!(reply.data["sent"], true);

    // One POST: the stored text as the user's own message, the people mentioned in Graph's
    // way (a partial name resolves to the only member it names).
    let posts = t.posts().await;
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].url.path(), format!("/chats/{CHAT}/messages"));
    let body: Value = posts[0].body_json().unwrap();
    let html = body["body"]["content"].as_str().unwrap();
    assert!(
        html.contains("<at id=\"0\">Ana Pérez</at>")
            && html.contains("<at id=\"1\">Victoria Gómez</at>"),
        "{html}"
    );
    assert!(
        html.contains("Primer resumen") && html.contains("<b>va bien</b>"),
        "{html}"
    );
    assert_eq!(body["mentions"][0]["mentioned"]["user"]["id"], "ana-id");
    assert_eq!(body["mentions"][1]["mentioned"]["user"]["id"], "vic-id");

    // The run keeps the record, and Graph's own record is a `teams_post` `sent` event.
    let sends = t.sends();
    assert_eq!(sends.len(), 1);
    let record = &sends[0];
    assert_eq!(record["outcome"], "sent");
    assert_eq!(record["summary"], "summary-1");
    assert_eq!(record["chat"]["id"], CHAT);
    assert_eq!(record["chat"]["topic"], "Equipo pagos");
    assert_eq!(record["mentions"], json!(["Ana Pérez", "Victoria Gómez"]));
    assert_eq!(record["message_id"], "message-1");
    assert_eq!(record["edited"], false);
    // The text is kept only when it is an edit; the summary ID says what was sent otherwise.
    assert!(record.get("text").is_none());
    assert_eq!(
        record["chars"],
        "Primer resumen: **va bien**.".chars().count()
    );
    assert!(record["at"].as_i64().unwrap() > 0 && record["finished_at"].as_i64().unwrap() > 0);
    assert_eq!(reply.data["send"], *record);
    assert_eq!(t.events(), ["sent"]);
    // The stored summaries are untouched.
    assert_eq!(stored(&t.host, &t.run).summaries.len(), 2);
}

#[tokio::test]
async fn the_same_summary_to_a_second_chat_is_a_second_confirmed_send_with_its_own_record() {
    let t = Sending::new(created()).await;
    let first = t
        .send(json!({"summary":"summary-2","chat":CHAT,"confirmed":true}))
        .await;
    assert!(first.ok, "{:?}", first.message);
    // Unconfirmed, the second names exactly what it would do and posts nothing.
    let unconfirmed = t
        .send(json!({"summary":"summary-2","chat":OTHER_CHAT,"mentions":["Luis"]}))
        .await;
    assert_eq!(unconfirmed.code, "invalid_input");
    let question = unconfirmed.message.unwrap();
    assert!(
        question.contains(OTHER_CHAT)
            && question.contains("summary-2")
            && question.contains("Luis")
            && question.contains("Segundo resumen."),
        "{question}"
    );
    assert_eq!(t.posts().await.len(), 1);
    let second = t
        .send(json!({"summary":"summary-2","chat":OTHER_CHAT,"mentions":["Luis"],"confirmed":true}))
        .await;
    assert!(second.ok, "{:?}", second.message);
    let posts = t.posts().await;
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[1].url.path(), format!("/chats/{OTHER_CHAT}/messages"));
    // A person's one-to-one chat is a destination of the directory too.
    let person = t
        .send(json!({"summary":"summary-2","chat":"19:one@unq.gbl.spaces","confirmed":true}))
        .await;
    assert!(person.ok, "{:?}", person.message);
    let sends = t.sends();
    assert_eq!(sends.len(), 3);
    assert_eq!(
        sends
            .iter()
            .map(|s| s["chat"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [CHAT, OTHER_CHAT, "19:one@unq.gbl.spaces"]
    );
    let ids: std::collections::BTreeSet<&str> =
        sends.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 3);
    assert!(
        sends
            .iter()
            .all(|s| s["outcome"] == "sent" && s["summary"] == "summary-2")
    );
    assert_eq!(sends[1]["mentions"], json!(["Luis Rojas"]));
    assert_eq!(t.events(), ["sent", "sent", "sent"]);
}

#[tokio::test]
async fn an_edited_text_replaces_the_summary_for_that_send_and_is_kept_with_its_record() {
    let t = Sending::new(created()).await;
    let reply = t
        .send(json!({"summary":"summary-1","chat":CHAT,"text":"Hola @[Ana Pérez], el resumen corregido.","mentions":["Victoria Soto"],"confirmed":true}))
        .await;
    assert!(reply.ok, "{:?}", reply.message);
    let body: Value = t.posts().await[0].body_json().unwrap();
    let html = body["body"]["content"].as_str().unwrap();
    assert!(
        html.contains("el resumen corregido") && !html.contains("Primer resumen"),
        "{html}"
    );
    // The mention written in the text and the one given are both resolved, once each.
    assert_eq!(body["mentions"].as_array().unwrap().len(), 2);
    let record = &t.sends()[0];
    assert_eq!(record["edited"], true);
    assert_eq!(record["text"], "Hola @[Ana Pérez], el resumen corregido.");
    assert_eq!(record["mentions"], json!(["Victoria Soto", "Ana Pérez"]));
    // The stored summary is not changed by the edit.
    assert_eq!(
        stored(&t.host, &t.run).summaries[0]["text"],
        "Primer resumen: **va bien**."
    );
    // An empty or too long edit sends nothing.
    for text in ["   ".to_owned(), "x".repeat(MAX_EDITED_CHARS + 1)] {
        let refused = t
            .send(json!({"summary":"summary-1","chat":CHAT,"text":text,"confirmed":true}))
            .await;
        assert_eq!(refused.code, "invalid_input");
    }
    assert_eq!(t.posts().await.len(), 1);
}

#[tokio::test]
async fn nothing_is_sent_without_confirmation_a_chat_of_the_directory_and_known_mentions() {
    let t = Sending::new(created()).await;
    let cases = [
        // No confirmation.
        (json!({"summary":"summary-1","chat":CHAT}), "Confirm"),
        (
            json!({"summary":"summary-1","chat":CHAT,"confirmed":false}),
            "Confirm",
        ),
        // A chat that is not in the directory, however well formed, and the personal chat.
        (
            json!({"summary":"summary-1","chat":"19:stranger@thread.v2","confirmed":true}),
            "not in your chat directory",
        ),
        (
            json!({"summary":"summary-1","chat":"self","confirmed":true}),
            "directory",
        ),
        (
            json!({"summary":"summary-1","chat":"48:notes","confirmed":true}),
            "directory",
        ),
        (
            json!({"summary":"summary-1","chat":"  ","confirmed":true}),
            "Name the chat",
        ),
        // Mentions nobody in the chat answers to, or that fit several members.
        (
            json!({"summary":"summary-1","chat":CHAT,"mentions":["Luis"],"confirmed":true}),
            "names nobody",
        ),
        (
            json!({"summary":"summary-1","chat":CHAT,"mentions":["Victoria"],"confirmed":true}),
            "several members",
        ),
        (
            json!({"summary":"summary-1","chat":CHAT,"mentions":["Ana","Nadie"],"confirmed":true}),
            "«Nadie»",
        ),
        (
            json!({"summary":"summary-1","chat":CHAT,"text":"Hola @[Luis Rojas]","confirmed":true}),
            "names nobody",
        ),
        (
            json!({"summary":"summary-1","chat":CHAT,"mentions":["a]b"],"confirmed":true}),
            "display name",
        ),
        // A summary the run does not have.
        (
            json!({"summary":"summary-9","chat":CHAT,"confirmed":true}),
            "no summary summary-9",
        ),
    ];
    for (args, expected) in cases {
        let reply = t.send(args.clone()).await;
        assert!(!reply.ok, "{args}");
        let message = reply.message.unwrap();
        assert!(message.contains(expected), "{args}: {message}");
    }
    let unknown_run = call(&t.host, "workflow_summary_send", json!({"run":uuid::Uuid::new_v4().to_string(),"summary":"summary-1","chat":CHAT,"confirmed":true})).await;
    assert_eq!(unknown_run.code, "invalid_input");
    // Nothing reached Graph's send, and nothing is recorded: no send happened.
    assert!(t.posts().await.is_empty());
    assert!(t.sends().is_empty());
    assert!(t.events().is_empty());
}

#[tokio::test]
async fn an_uncertain_outcome_is_recorded_and_never_retried_and_a_refusal_is_recorded_too() {
    let t = Sending::new(ResponseTemplate::new(500)).await;
    let reply = t
        .send(json!({"summary":"summary-1","chat":CHAT,"confirmed":true}))
        .await;
    assert!(!reply.ok);
    let message = reply.message.unwrap();
    assert!(
        message.contains("never retried") && message.contains("uncertain"),
        "{message}"
    );
    // One attempt, however long we wait: the send is never retried.
    assert_eq!(t.posts().await.len(), 1);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(t.posts().await.len(), 1);
    let sends = t.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["outcome"], "uncertain");
    assert!(
        sends[0]["detail"]
            .as_str()
            .unwrap()
            .contains("never retried")
    );
    assert!(sends[0].get("message_id").is_none());
    assert_eq!(t.events(), ["uncertain"]);
    // Sending again is a deliberate, confirmed act, and the confirmation says to check Teams.
    let again = t.send(json!({"summary":"summary-1","chat":CHAT})).await;
    assert!(
        again
            .message
            .unwrap()
            .contains("uncertain: check Teams first")
    );
    // A client error is a refusal: nothing was sent, and the record says so.
    t.server.reset().await;
    let refusing = Sending::new(ResponseTemplate::new(400)).await;
    let refused = refusing
        .send(json!({"summary":"summary-1","chat":CHAT,"confirmed":true}))
        .await;
    assert!(!refused.ok);
    assert!(refused.message.unwrap().contains("refused"));
    assert_eq!(refusing.sends()[0]["outcome"], "refused");
    assert_eq!(refusing.posts().await.len(), 1);
    assert_eq!(refusing.events(), ["refused"]);
    let _ = &refusing.graph;
}

#[tokio::test]
async fn summaries_are_written_and_sent_only_where_the_runs_are() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let own = implementation::test_machine(&host, None).await;
    director(&host, &own);
    let run = record(&host, &own);
    let calls = [
        ("workflow_summary", json!({"run":run.id})),
        (
            "workflow_summary_send",
            json!({"run":run.id,"summary":"s","chat":CHAT,"confirmed":true}),
        ),
    ];
    for (method, args) in calls {
        // A machine credential never reaches them.
        let mut request = control::Request::new(method);
        request.args = args.clone();
        let reply = authority::within(
            authority::Invocation {
                scope: host.state.authority.clone(),
                transport: authority::Transport::Machine,
            },
            control::dispatch(&host, request),
        )
        .await;
        assert_eq!(reply.code, "authority_denied", "{method}");
        // Nor a caller that is not the account's owner.
        let mut request = control::Request::new(method);
        request.args = args;
        let mut scope = host.state.authority.clone();
        scope.owner = false;
        let reply = authority::within(
            authority::Invocation {
                scope,
                transport: Local,
            },
            control::dispatch(&host, request),
        )
        .await;
        assert!(!reply.ok, "{method}");
    }
    assert!(stored(&host, &run.id).summaries.is_empty());
}

#[tokio::test]
async fn an_agent_sends_a_stored_summary_as_written_and_the_person_may_edit_it() {
    use authority::Transport::{Agent, Browser};
    let t = Sending::new(created()).await;
    // The operation as each transport reaches it (the portal's allowlist check is the layer
    // before, covered by the portal and agent tests).
    let run = |transport: authority::Transport, args: Value| {
        let host = t.host.clone();
        let mut body = json!({"run":t.run});
        for (key, value) in args.as_object().unwrap() {
            body[key] = value.clone();
        }
        async move {
            authority::within(
                authority::Invocation {
                    scope: host.state.authority.clone(),
                    transport,
                },
                operation(&host, "workflow_summary_send", &body),
            )
            .await
        }
    };
    // The remote endpoint's agent: a free text is the capability `teams_post` is denied there for.
    let edited = json!({"summary":"summary-1","chat":CHAT,"text":"Otro texto","confirmed":true});
    let refused = run(Agent, edited.clone()).await.unwrap_err();
    assert!(refused.starts_with("[invalid_input]"));
    assert!(
        refused.contains("stored summary as it is written"),
        "{refused}"
    );
    assert!(t.posts().await.is_empty() && t.sends().is_empty());
    // The same agent sends the stored summary, unedited.
    let stored = run(
        Agent,
        json!({"summary":"summary-1","chat":CHAT,"confirmed":true}),
    )
    .await;
    assert!(stored.is_ok(), "{stored:?}");
    // The person in the portal's browser edits it, as at the local page.
    let person = run(Browser, edited).await;
    assert!(person.is_ok(), "{person:?}");
    let posts = t.posts().await;
    assert_eq!(posts.len(), 2);
    let html = |n: usize| {
        posts[n].body_json::<Value>().unwrap()["body"]["content"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert!(html(0).contains("Primer resumen") && html(1).contains("Otro texto"));
    // The remote endpoint offers the summary send and still denies `teams_post`.
    assert!(crate::app::agent::check_method("workflow_summary_send").is_ok());
    assert!(crate::app::agent::check_method("workflow_summary").is_ok());
    assert!(crate::app::agent::check_method("teams_post").is_err());
    assert!(matches!(
        crate::app::agent::tool("teams_post"),
        crate::app::agent::Remote::Denied(_)
    ));
}

#[tokio::test]
async fn the_confirmation_always_shows_the_exact_text_even_through_the_tool_layer() {
    let t = Sending::new(created()).await;
    // The tool layer reads the stored text from the run so the person sees what is posted.
    let host = t.host.clone();
    let dispatch: crate::app::tools::Dispatch = Arc::new(move |request| {
        let host = host.clone();
        Box::pin(async move {
            Ok(authority::within(
                authority::Invocation {
                    scope: host.state.authority.clone(),
                    transport: Local,
                },
                control::dispatch(&host, request),
            )
            .await)
        })
    });
    let args = json!({"run":t.run,"summary":"summary-1","chat":CHAT,"mentions":["Ana"]});
    let question =
        crate::app::tools::question_with(dispatch.clone(), "workflow_summary_send", &args)
            .await
            .unwrap()
            .unwrap();
    for shown in ["Primer resumen: **va bien**.", CHAT, "Ana", "summary-1"] {
        assert!(question.contains(shown), "{shown}: {question}");
    }
    // An edit is the text shown, not the stored one.
    let mut edited = args.clone();
    edited["text"] = json!("Texto editado");
    let question =
        crate::app::tools::question_with(dispatch.clone(), "workflow_summary_send", &edited)
            .await
            .unwrap()
            .unwrap();
    assert!(
        question.contains("Texto editado") && !question.contains("Primer resumen"),
        "{question}"
    );
    // A summary the run does not have cannot be confirmed: nothing is asked.
    let mut missing = args.clone();
    missing["summary"] = json!("summary-9");
    assert!(
        crate::app::tools::question_with(dispatch, "workflow_summary_send", &missing)
            .await
            .is_err()
    );
    // The host's own refusal shows the text too.
    let refusal = t
        .send(json!({"summary":"summary-1","chat":CHAT}))
        .await
        .message
        .unwrap();
    assert!(
        refusal.contains("Primer resumen: **va bien**."),
        "{refusal}"
    );
}

#[tokio::test]
async fn a_send_whose_record_cannot_be_saved_still_reports_its_real_outcome() {
    // Sent, then the run is lost before its record is updated: the caller still learns it was sent.
    let lost = Sending::with(|host| {
        move |_: &MockRequest| {
            for run in host.state.workflows.db.list(&[], None, -1).unwrap() {
                host.state.workflows.db.forget(&run.id);
            }
            created()
        }
    })
    .await;
    let reply = lost
        .send(json!({"summary":"summary-1","chat":CHAT,"confirmed":true}))
        .await;
    assert!(reply.ok, "{:?}", reply.message);
    assert_eq!(reply.data["sent"], true);
    assert_eq!(reply.data["record_saved"], false);
    let warning = reply.data["warning"].as_str().unwrap();
    assert!(
        warning.contains("was sent to Teams")
            && warning.contains("message-1")
            && warning.contains("could not be saved"),
        "{warning}"
    );
    assert_eq!(lost.posts().await.len(), 1);
    // Uncertain, then the record cannot be saved: still an error that says the result is unknown.
    let unknown = Sending::with(|host| {
        move |_: &MockRequest| {
            for run in host.state.workflows.db.list(&[], None, -1).unwrap() {
                host.state.workflows.db.forget(&run.id);
            }
            ResponseTemplate::new(500)
        }
    })
    .await;
    let reply = unknown
        .send(json!({"summary":"summary-1","chat":CHAT,"confirmed":true}))
        .await;
    assert!(!reply.ok);
    let message = reply.message.unwrap();
    assert!(
        message.contains("never retried") && message.contains("could not be saved"),
        "{message}"
    );
    assert_eq!(unknown.posts().await.len(), 1);
}

#[tokio::test]
async fn a_failure_before_the_request_is_a_refusal_not_an_uncertain_send() {
    let t = Sending::new(created()).await;
    // Graph cannot be reached for `me`, which `post_chat` reads before it posts.
    Mock::given(method("GET"))
        .and(path("/me"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(1)
        .mount(&t.server)
        .await;
    let reply = t
        .send(json!({"summary":"summary-1","chat":CHAT,"confirmed":true}))
        .await;
    assert!(!reply.ok);
    assert!(reply.message.unwrap().contains("nothing was sent"));
    assert!(t.posts().await.is_empty());
    let sends = t.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["outcome"], "refused", "{sends:?}");
    assert!(t.events().is_empty());
}

#[tokio::test]
async fn mentions_resolve_over_the_member_list_the_post_uses_so_a_confirmed_send_is_not_refused_later()
 {
    let t = Sending::new(created()).await;
    // «yo» names only the user in the post's own list (the directory leaves them out): the
    // preflight and the post agree, so it neither fails after confirmation nor passes unseen.
    let reply = t
        .send(json!({"summary":"summary-1","chat":CHAT,"mentions":["Yo"],"confirmed":true}))
        .await;
    assert!(reply.ok, "{:?}", reply.message);
    assert_eq!(t.posts().await.len(), 1);
    // Names matching several members of that list are refused before the post.
    let several = t
        .send(json!({"summary":"summary-1","chat":CHAT,"mentions":["o"],"confirmed":true}))
        .await;
    assert!(several.message.unwrap().contains("several members"));
    // The same person named three ways is mentioned once.
    let twice = t
        .send(json!({"summary":"summary-1","chat":CHAT,"mentions":["ana pérez","ANA PÉREZ","Ana"],"confirmed":true}))
        .await;
    assert!(twice.ok, "{:?}", twice.message);
    assert_eq!(t.sends().last().unwrap()["mentions"], json!(["Ana Pérez"]));
    let body: Value = t.posts().await[1].body_json().unwrap();
    assert_eq!(body["mentions"].as_array().unwrap().len(), 1);
}

#[test]
fn a_cut_falls_between_words_and_the_links_follow_the_text_that_is_kept() {
    use crate::config::Language;
    let words = "palabra ".repeat(900);
    let cut = clean(&words, 100);
    assert!(
        cut.ends_with("palabra…") && cut.chars().count() <= 100,
        "{cut}"
    );
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let run = record(&host, "machine");
    // A child named only in the part that is cut off is not linked.
    let long = format!(
        "{}\n\nAl final, la tarea #101.",
        "Relleno del resumen que ocupa sitio. ".repeat(130)
    );
    let composed = compose(&long, &run, Language::En);
    assert!(composed.chars().count() <= MAX_CHARS);
    assert!(
        composed.contains('…') && !composed.contains("#101."),
        "{composed}"
    );
    assert!(!composed.contains(WEB_URL), "{composed}");
    assert!(composed.contains(ITEM_URL));
}

/// The words after `pta workflows`, as the shell passes them.
fn words(args: &[&str]) -> Vec<String> {
    ["workflows", "summary"]
        .iter()
        .chain(args)
        .map(|a| a.to_string())
        .collect()
}

#[tokio::test]
async fn the_cli_send_path_runs_end_to_end_against_the_host() {
    use crate::app::cli::{summary_send, validate_args};
    let t = Sending::new(created()).await;
    let file = t.root.path().join("edited.md");
    std::fs::write(&file, "Texto corregido a mano.").unwrap();
    let file = file.display().to_string();
    // Every flag of the command is accepted by the argument check, and only for this command.
    let full = words(&[
        &t.run,
        "--send",
        CHAT,
        "--mention",
        "Ana",
        "--mention",
        "Luis",
        "--summary",
        "summary-1",
        "--text-file",
        &file,
        "--confirmed",
    ]);
    validate_args(&full).unwrap();
    validate_args(&words(&[&t.run])).unwrap();
    for flag in ["--send", "--mention", "--text-file", "--summary"] {
        assert!(
            validate_args(&words(&[&t.run, flag])).is_err(),
            "{flag} needs a value"
        );
        let elsewhere: Vec<String> = ["workflows", "status", &t.run, flag, "x"]
            .iter()
            .map(|a| a.to_string())
            .collect();
        assert!(
            validate_args(&elsewhere).is_err(),
            "{flag} is only for the summary"
        );
    }
    let status = call(&t.host, "workflow_run_status", json!({"run":t.run})).await;
    assert!(status.ok);
    let run = t.run.as_str();

    // Unconfirmed: nothing is sent; the reply names the latest summary, its ID and its exact
    // text, and says the confirmed run must repeat that ID.
    let shown = summary_send(
        &words(&[run, "--send", CHAT, "--mention", "Ana"]),
        run,
        &status.data,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(shown.code, "invalid_input");
    let question = shown.message.unwrap();
    assert!(
        question.contains("summary-2")
            && question.contains("Segundo resumen.")
            && question.contains("--summary summary-2 --confirmed"),
        "{question}"
    );
    // A confirmed send without the summary's ID is refused, so one written meanwhile is never
    // posted unseen.
    let unnamed = summary_send(
        &words(&[run, "--send", CHAT, "--confirmed"]),
        run,
        &status.data,
    )
    .unwrap()
    .unwrap_err();
    assert!(unnamed.message.unwrap().contains("--summary ID"));
    assert!(t.posts().await.is_empty());
    // The summary named is the one the host posts, not the latest.
    let body = summary_send(
        &words(&[
            run,
            "--send",
            CHAT,
            "--mention",
            "Ana",
            "--summary",
            "summary-1",
            "--confirmed",
        ]),
        run,
        &status.data,
    )
    .unwrap()
    .unwrap();
    assert_eq!(body["summary"], "summary-1");
    let sent = call(&t.host, "workflow_summary_send", body).await;
    assert!(sent.ok, "{:?}", sent.message);
    let posted: Value = t.posts().await[0].body_json().unwrap();
    let html = posted["body"]["content"].as_str().unwrap();
    assert!(
        html.contains("Primer resumen") && !html.contains("Segundo"),
        "{html}"
    );
    assert_eq!(posted["mentions"][0]["mentionText"], "Ana Pérez");
    // An edited file replaces the text for that send and is kept with its record.
    let body = summary_send(
        &words(&[
            run,
            "--send",
            OTHER_CHAT,
            "--text-file",
            &file,
            "--summary",
            "summary-2",
            "--confirmed",
        ]),
        run,
        &status.data,
    )
    .unwrap()
    .unwrap();
    assert_eq!(body["text"], "Texto corregido a mano.");
    let edited = call(&t.host, "workflow_summary_send", body).await;
    assert!(edited.ok, "{:?}", edited.message);
    let sends = t.sends();
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[1]["text"], "Texto corregido a mano.");
    assert_eq!(sends[1]["chat"]["id"], OTHER_CHAT);
    // A summary the run does not have is an error before anything is asked.
    assert!(
        summary_send(
            &words(&[run, "--send", CHAT, "--summary", "summary-9", "--confirmed"]),
            run,
            &status.data
        )
        .is_err()
    );
}
