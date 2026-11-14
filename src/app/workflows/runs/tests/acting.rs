//! The acting blocks end to end: Teams notifications posted through a mock Graph (chat members,
//! one POST that is never retried) and launchers run as fake programs, each at its stage of a
//! real run of the engine, with the synthetic Azure catalog and fake provider CLI of the
//! engine's own tests.
use super::super::super::acting::{self, Occurrence, Progress};
use super::*;
use crate::workflows::{Block, Chat, Kind, LauncherRef, Stage};
use authority::Transport;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const CHAT: &str = "19:synthetic@thread.v2";

struct Token;
#[async_trait::async_trait]
impl crate::adapters::oauth::AccessToken for Token {
    async fn access_token(&self) -> Result<String> {
        Ok("synthetic-graph-token".into())
    }
}

/// A synthetic Graph: the chat's members and a POST that answers `post_status`.
struct Teams {
    server: MockServer,
    dir: tempfile::TempDir,
    graph: Arc<crate::adapters::graph::Graph>,
}
impl Teams {
    async fn new(host: &Arc<Host>, members: &[&str], post_status: u16) -> Self {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let config = host.state.settings.config().unwrap();
        let me = config.graph.user_id.clone();
        let mut list = Vec::new();
        if !members.is_empty() {
            list.push(json!({"userId": me, "displayName": "Yo"}));
        }
        for (at, name) in members.iter().enumerate() {
            list.push(json!({"userId": format!("user-{at}"), "displayName": name}));
        }
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": me})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/chats/{CHAT}/members")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": list})))
            .mount(&server)
            .await;
        let created = if post_status < 300 {
            ResponseTemplate::new(post_status).set_body_json(json!({"id": "m1"}))
        } else {
            ResponseTemplate::new(post_status)
        };
        Mock::given(method("POST"))
            .and(path(format!("/chats/{CHAT}/messages")))
            .respond_with(created)
            .mount(&server)
            .await;
        let graph = Arc::new(crate::adapters::graph::Graph {
            client: reqwest::Client::new(),
            token: Arc::new(Token),
            base_url: server.uri(),
            config: Arc::new(config),
            store: Arc::new(crate::state::Store::open(&dir.path().join("graph.db")).unwrap()),
            client_state: "synthetic-client-state-of-32-chars".into(),
            config_path: None,
        });
        Self { server, dir, graph }
    }
    /// The session the running assistant would hold.
    async fn attach(&self, host: &Arc<Host>) {
        *host.state.activity_graph.lock().await = Some(self.graph.clone());
    }
    async fn posts(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .map(|r| String::from_utf8_lossy(&r.body).into_owned())
            .collect()
    }
    /// The details of the audit events of one kind, oldest first.
    fn events(&self, kind: &str) -> Vec<String> {
        let mut found: Vec<String> =
            crate::state::Store::inspect(&self.dir.path().join("graph.db"), true, 100, None, false)
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == kind)
                .map(|e| e["detail"].as_str().unwrap().to_owned())
                .collect();
        found.reverse();
        found
    }
}

fn notification(id: &str, stage: Stage, template: &str, mentions: &[&str]) -> Block {
    Block {
        id: id.into(),
        name: format!("Tell {id}"),
        kind: Kind::TeamsNotification,
        stage: Some(stage),
        chat: Some(Chat {
            id: CHAT.into(),
            name: "Releases".into(),
        }),
        mentions: mentions.iter().map(|m| (*m).to_owned()).collect(),
        template: template.into(),
        ..Default::default()
    }
}
fn launcher_block(id: &str, stage: Stage, launcher: &crate::consumers::Launcher) -> Block {
    Block {
        id: id.into(),
        name: format!("Run {id}"),
        kind: Kind::Launcher,
        stage: Some(stage),
        launcher: Some(LauncherRef {
            id: launcher.id.clone(),
            digest: launcher.digest.clone(),
        }),
        ..Default::default()
    }
}

impl Bench {
    /// The workflow's blocks, in order, after its instruction block.
    fn acting(&self, blocks: Vec<Block>) {
        let mut map = self.host.state.settings.map().unwrap();
        map.workflows[0].blocks = std::iter::once("base".to_owned())
            .chain(blocks.iter().map(|b| b.id.clone()))
            .collect();
        map.workflow_blocks.extend(blocks);
        self.host.state.settings.save_map(&map).unwrap();
    }
    /// Pull requests that are completed already, for the task's branch.
    fn merged(&self) {
        self.pulls(WEB, "payments-web", "feature/child-1", 21, "completed");
    }
    /// A launcher of this computer that runs `body` in the allowed checkout; `marker` gets a
    /// line each time it runs.
    #[cfg(unix)]
    fn launcher(&self, id: &str, body: &str, timeout: u64) -> crate::consumers::Launcher {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let program = self.root.path().join(format!("{id}.sh"));
        fs::write(
            &program,
            format!(
                "#!/bin/sh\necho run >> '{}'\n{body}\n",
                self.marker(id).display()
            ),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let program = program.canonicalize().unwrap();
        let digest = Sha256::digest(fs::read(&program).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        crate::consumers::Launcher {
            id: id.into(),
            description: "Synthetic release".into(),
            program,
            digest,
            args: Vec::new(),
            directory: self.api.clone(),
            env: BTreeMap::new(),
            timeout_seconds: timeout,
            created_at: 0,
            approved_at: Some(1),
        }
    }
    #[cfg(unix)]
    fn marker(&self, id: &str) -> PathBuf {
        self.root.path().join(format!("{id}.ran"))
    }
    #[cfg(unix)]
    fn ran(&self, id: &str) -> usize {
        fs::read_to_string(self.marker(id))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }
    #[cfg(unix)]
    fn define(&self, launcher: &crate::consumers::Launcher) {
        consumers::add_test_launcher(&self.host.state, launcher.clone());
    }
}

/// The text of every request a run's record or the engine's operations answered with.
async fn everything(bench: &Bench, id: &str) -> String {
    let status = bench
        .op("workflow_run_status", json!({"run":id}))
        .await
        .unwrap();
    let listed = bench.op("workflow_runs", json!({})).await.unwrap();
    format!("{status}{listed}{:?}", bench.run(id))
}

#[tokio::test]
async fn a_done_notification_posts_once_with_rendered_placeholders_and_the_resolved_mention() {
    let bench = Bench::new().await;
    bench.merged();
    bench.acting(vec![notification(
        "notify",
        Stage::Done,
        "{item} is {state}: {link}\nPull requests:\n{pull_requests}\nSummary: {summary}\nAs a link: [{id} - {title}]({link})",
        &["Victoria"],
    )]);
    let teams = Teams::new(&bench.host, &["Victoria Gómez", "Ana Pérez"], 201).await;
    let id = bench.start(TASK).await;
    // A summary the director wrote can name nobody: only the block's own mentions mention.
    let mut run = bench.run(&id);
    run.summaries
        .push(json!({"text":"Shipped @[Ana Pérez] <b>fast</b>","created_at":1,"model":"m"}));
    bench.save(&run);
    teams.attach(&bench.host).await;
    let run = bench.state(&id, Phase::Done).await;
    // More ticks, and a restart, post nothing more.
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(teams.posts().await.len(), 1);
    let posts = teams.posts().await;
    let body: Value = serde_json::from_str(&posts[0]).unwrap();
    let html = body["body"]["content"].as_str().unwrap();
    assert!(html.contains("<at id=\"0\">Victoria Gómez</at>"), "{html}");
    assert!(html.contains("Fix rounding (#200) is done"), "{html}");
    // `{id}` and `{title}` are the item's number and title, for a link of one's own.
    assert!(html.contains("200 - Fix rounding"), "{html}");
    assert!(
        html.contains("https://dev.azure.com/example/Payments/_workitems/edit/200"),
        "{html}"
    );
    assert!(
        html.contains("pullrequest/21") && html.contains("PR 21: PR 21"),
        "{html}"
    );
    assert!(html.contains("Shipped"), "{html}");
    assert_eq!(body["mentions"].as_array().unwrap().len(), 1);
    assert!(!html.contains("Ana Pérez</at>"));
    // The audit and the run's record each hold the one outcome.
    assert_eq!(teams.events("workflow_notification"), ["sent"]);
    assert_eq!(teams.events("teams_post"), ["sent"]);
    let [outcome] = run.block_outcomes.as_slice() else {
        panic!("{:?}", run.block_outcomes)
    };
    assert_eq!(
        (outcome.block.as_str(), outcome.occurrence.as_str()),
        ("notify", "done")
    );
    assert_eq!(outcome.outcome, "sent");
    assert_eq!(outcome.message_id.as_deref(), Some("m1"));
    assert_eq!(outcome.stage, Some(Stage::Done));
    // It shows in the run's status.
    let shown = bench
        .op("workflow_run_status", json!({"run":id}))
        .await
        .unwrap();
    assert_eq!(shown["run"]["block_outcomes"][0]["outcome"], "sent");
    // Another host on the same profile never posts it again.
    let mut bench = bench;
    bench.restart().await;
    teams.attach(&bench.host).await;
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(teams.posts().await.len(), 1);
    assert!(bench.catalog.only_reads());
}

#[tokio::test]
async fn a_refused_notification_records_why_and_the_run_continues() {
    // The chat's members, the mention, and whether the session exists.
    for (members, mentions, session, reason) in [
        (&[][..], &["Victoria"][..], true, "not a member"),
        (&["Ana Pérez"][..], &["Victoria"][..], true, "names nobody"),
        (
            &["Victoria Gómez", "Victoria Díaz"][..],
            &["Victoria"][..],
            true,
            "several members",
        ),
        (
            &["Victoria Gómez"][..],
            &["Victoria"][..],
            false,
            "no Microsoft session",
        ),
    ] {
        let bench = Bench::new().await;
        bench.merged();
        bench.acting(vec![
            notification("first", Stage::Done, "{item} is done", mentions),
            notification("second", Stage::Done, "again {item}", &[]),
        ]);
        let teams = Teams::new(&bench.host, members, 201).await;
        let id = bench.start(TASK).await;
        if session {
            teams.attach(&bench.host).await;
        }
        let run = bench.state(&id, Phase::Done).await;
        let first = &run.block_outcomes[0];
        assert_eq!(first.outcome, "refused", "{reason}");
        assert!(
            first.reason.as_deref().unwrap().contains(reason),
            "{reason}: {:?}",
            first.reason
        );
        // The run went on: the next block of the stage ran in its turn.
        assert_eq!(run.block_outcomes.len(), 2);
        assert_eq!(run.state, Phase::Done);
        if session && members.is_empty() {
            // Not a member: even the second block's post is refused, and nothing was sent.
            assert_eq!(run.block_outcomes[1].outcome, "refused");
        }
        if session {
            let sent = run
                .block_outcomes
                .iter()
                .filter(|o| o.outcome == "sent")
                .count();
            assert_eq!(teams.posts().await.len(), sent, "{reason}");
        }
        if session {
            assert_eq!(teams.events("workflow_notification")[0], "refused");
        } else {
            assert!(teams.posts().await.is_empty());
        }
    }
}

#[tokio::test]
async fn a_failure_after_the_request_is_uncertain_and_nothing_is_retried() {
    let bench = Bench::new().await;
    bench.merged();
    bench.acting(vec![
        notification("first", Stage::Done, "{item} is done", &[]),
        notification("second", Stage::Done, "again", &[]),
    ]);
    let teams = Teams::new(&bench.host, &["Victoria Gómez"], 500).await;
    let id = bench.start(TASK).await;
    teams.attach(&bench.host).await;
    let run = bench.state(&id, Phase::Done).await;
    for _ in 0..3 {
        bench.tick().await;
    }
    // One request for each block, and neither was repeated.
    assert_eq!(teams.posts().await.len(), 2);
    assert!(run.block_outcomes.iter().all(
        |o| o.outcome == "uncertain" && o.reason.as_deref().unwrap().contains("never retried")
    ));
    assert_eq!(
        teams.events("workflow_notification"),
        ["uncertain", "uncertain"]
    );
    assert_eq!(run.state, Phase::Done);
}

#[tokio::test]
async fn an_attempt_without_a_result_is_uncertain_after_a_restart_and_never_made_again() {
    let mut bench = Bench::new().await;
    bench.acting(vec![notification("notify", Stage::Done, "{item}", &[])]);
    let teams = Teams::new(&bench.host, &["Ana Pérez"], 201).await;
    let id = bench.start(TASK).await;
    bench.state(&id, Phase::PullRequests).await;
    // The host stopped after it recorded the attempt and before it recorded the result.
    let mut run = bench.run(&id);
    run.block_outcomes
        .push(crate::app::workflows::record::BlockOutcome {
            block: "notify".into(),
            kind: Kind::TeamsNotification,
            stage: Some(Stage::Done),
            outcome: "attempting".into(),
            at: 1,
            occurrence: "done".into(),
            reason: None,
            exit_code: None,
            duration_ms: None,
            message_id: None,
        });
    bench.save(&run);
    bench.restart().await;
    teams.attach(&bench.host).await;
    bench.merged();
    let run = bench.state(&id, Phase::Done).await;
    assert_eq!(run.block_outcomes.len(), 1);
    assert_eq!(run.block_outcomes[0].outcome, "uncertain");
    assert!(teams.posts().await.is_empty());
}

#[tokio::test]
async fn every_stage_of_an_hu_run_acts_in_its_turn_and_a_child_is_told_once() {
    let bench = Bench::new().await;
    bench.pulls(WEB, "payments-web", "feature/child-1", 11, "completed");
    bench.pulls(API, "payments-api", "feature/child-2", 12, "completed");
    let stages = [
        Stage::BeforePlanning,
        Stage::PlanReady,
        Stage::ChildrenCreated,
        Stage::ChildDone,
        Stage::PullRequestsCompleted,
        Stage::Done,
    ];
    // In the workflow's order: for one stage, the blocks run in the order the workflow lists.
    let mut blocks: Vec<Block> = stages
        .iter()
        .map(|s| {
            let word = format!("{s:?}");
            notification(
                &format!("n-{}", word.to_lowercase()),
                *s,
                &format!("{word}: {{item}} {{state}}"),
                &[],
            )
        })
        .collect();
    blocks.push(notification(
        "second",
        Stage::ChildDone,
        "Second {link}",
        &[],
    ));
    bench.acting(blocks);
    let teams = Teams::new(&bench.host, &["Ana Pérez"], 201).await;
    let id = bench.start(HU).await;
    teams.attach(&bench.host).await;
    // Before planning, then the plan waits: its note is posted and nothing more.
    let run = bench.state(&id, Phase::PlanReview).await;
    let posts = teams.posts().await;
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert!(posts[0].contains("BeforePlanning: Pay by card (#100)"));
    assert!(posts[1].contains("PlanReady: Pay by card (#100) plan_review"));
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(teams.posts().await.len(), 2);
    bench.confirm(&run, "created", &[101, 102]).await;
    let run = bench.state(&id, Phase::Done).await;
    let posts = teams.posts().await;
    let order: Vec<&str> = posts
        .iter()
        .map(|p| {
            [
                "BeforePlanning",
                "PlanReady",
                "ChildrenCreated",
                "ChildDone",
                "Second",
                "PullRequestsCompleted",
                "Done",
            ]
            .into_iter()
            .find(|w| p.contains(&format!("{w}:")) || (*w == "Second" && p.contains("Second ")))
            .unwrap()
        })
        .collect();
    assert_eq!(
        order,
        [
            "BeforePlanning",
            "PlanReady",
            "ChildrenCreated",
            "ChildDone",
            "Second",
            "ChildDone",
            "Second",
            "PullRequestsCompleted",
            "Done"
        ],
        "{posts:?}"
    );
    // Each child is told once, by its own title and link.
    assert!(posts[3].contains("Child 101 (#101)") && posts[5].contains("Child 102 (#102)"));
    assert!(posts[4].contains("_workitems/edit/101"));
    assert!(posts[8].contains("Pay by card (#100) done"));
    assert_eq!(run.block_outcomes.len(), 9);
    assert!(run.block_outcomes.iter().all(|o| o.outcome == "sent"));
    let blocks = run
        .timeline
        .iter()
        .find(|s| s.stage == Phase::Blocks)
        .unwrap();
    assert_eq!(blocks.outcome.as_deref(), Some("2 acting block outcomes"));
    assert_eq!(teams.events("workflow_notification").len(), 9);
}

#[tokio::test]
async fn a_task_run_says_once_that_its_coding_started_while_it_codes() {
    let bench = Bench::new().await;
    bench.merged();
    bench.mode("mode", "block");
    bench.acting(vec![notification(
        "started",
        Stage::CodingStarted,
        "Started [{id} - {title}]({link}): {state}",
        &[],
    )]);
    let teams = Teams::new(&bench.host, &["Ana Pérez"], 201).await;
    let id = bench.start(TASK).await;
    teams.attach(&bench.host).await;
    // Once the machine accepted the coding run, while it still codes.
    bench
        .until(&id, |r| {
            r.block_outcomes
                .iter()
                .any(|o| o.occurrence == format!("coding_started:{TASK}"))
        })
        .await;
    assert_eq!(bench.run(&id).state, Phase::Coding);
    let posts = teams.posts().await;
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(
        posts[0].contains("200 - Fix rounding") && posts[0].contains(": coding"),
        "{}",
        posts[0]
    );
    bench.release();
    let run = bench.state(&id, Phase::Done).await;
    assert_eq!(teams.posts().await.len(), 1);
    assert_eq!(run.block_outcomes.len(), 1);
    assert_eq!(run.block_outcomes[0].stage, Some(Stage::CodingStarted));
}

#[tokio::test]
async fn a_failed_run_posts_its_failed_blocks_once_for_each_failure() {
    let bench = Bench::new().await;
    bench.acting(vec![notification(
        "failed",
        Stage::Failed,
        "{item} is {state}",
        &[],
    )]);
    let teams = Teams::new(&bench.host, &["Ana Pérez"], 201).await;
    let id = bench.start(TASK).await;
    teams.attach(&bench.host).await;
    // The checkout the workflow maps is gone: coding cannot start, which fails the run.
    let fail = |bench: &Bench| {
        let mut run = bench.run(&id);
        run.definition.as_mut().unwrap().checkouts.clear();
        bench.save(&run);
    };
    fail(&bench);
    let run = bench.state(&id, Phase::Failed).await;
    assert_eq!(run.block_outcomes.len(), 1);
    assert_eq!(teams.posts().await.len(), 1);
    assert!(teams.posts().await[0].contains("Fix rounding (#200) is failed"));
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(teams.posts().await.len(), 1);
    // After a resume the same failure is another occurrence of the stage.
    bench
        .op("workflow_run_resume", json!({"run":id,"confirmed":true}))
        .await
        .unwrap();
    bench.state(&id, Phase::Failed).await;
    assert_eq!(teams.posts().await.len(), 2);
    assert_eq!(bench.run(&id).block_outcomes.len(), 2);
}

#[tokio::test]
async fn a_run_that_failed_while_the_host_was_down_posts_its_failed_blocks_after_the_start() {
    let mut bench = Bench::new().await;
    bench.acting(vec![notification("failed", Stage::Failed, "{item}", &[])]);
    let teams = Teams::new(&bench.host, &["Ana Pérez"], 201).await;
    let id = bench.start(TASK).await;
    // The host stopped right after the run was recorded as failed, before its blocks ran.
    let mut run = bench.run(&id);
    run.stop(
        Phase::Failed,
        bench.now(),
        "The coding run could not be launched.".into(),
    );
    bench.save(&run);
    bench.restart().await;
    teams.attach(&bench.host).await;
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(teams.posts().await.len(), 1);
    assert_eq!(bench.run(&id).block_outcomes[0].outcome, "sent");
    bench.restart().await;
    teams.attach(&bench.host).await;
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(teams.posts().await.len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn a_launcher_block_runs_with_its_stored_digest_and_records_the_outcome_only() {
    let bench = Bench::new().await;
    bench.merged();
    let release = bench.launcher(
        "release",
        "echo SECRET-OUTPUT\necho SECRET-ERROR >&2\nexit 0",
        30,
    );
    bench.define(&release);
    bench.acting(vec![launcher_block(
        "desa",
        Stage::PullRequestsCompleted,
        &release,
    )]);
    let id = bench.start(TASK).await;
    let run = bench.state(&id, Phase::Done).await;
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(bench.ran("release"), 1);
    let [outcome] = run.block_outcomes.as_slice() else {
        panic!("{:?}", run.block_outcomes)
    };
    assert_eq!(outcome.outcome, "succeeded");
    assert_eq!(outcome.exit_code, Some(0));
    assert!(outcome.duration_ms.is_some());
    assert_eq!(outcome.occurrence, "pull_requests_completed");
    // Nothing the program printed is anywhere the engine reaches.
    let seen = everything(&bench, &id).await;
    assert!(!seen.contains("SECRET"), "{seen}");
    // It ran after the pull requests were completed and before the run was done.
    assert!(run.timeline.iter().any(|s| s.stage == Phase::Blocks));
}

#[cfg(unix)]
#[tokio::test]
async fn a_launcher_that_is_not_approved_with_the_blocks_digest_runs_nothing() {
    for case in ["reapproved", "unapproved", "removed"] {
        let bench = Bench::new().await;
        bench.merged();
        let mut release = bench.launcher("release", "exit 0", 30);
        let block = launcher_block("desa", Stage::PullRequestsCompleted, &release);
        bench.acting(vec![block]);
        match case {
            // Approved again after a change: another digest than the block stored.
            "reapproved" => release.digest = "b".repeat(64),
            "unapproved" => release.approved_at = None,
            _ => {}
        }
        if case != "removed" {
            bench.define(&release);
        }
        let id = bench.start(TASK).await;
        let run = bench.state(&id, Phase::Done).await;
        assert_eq!(bench.ran("release"), 0, "{case}");
        assert_eq!(run.block_outcomes[0].outcome, "not_started", "{case}");
        assert!(run.block_outcomes[0].reason.is_some(), "{case}");
        assert_eq!(run.block_outcomes[0].exit_code, None);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_or_timed_out_launcher_ends_the_run_in_needs_attention() {
    for (body, timeout, outcome, code) in [
        ("echo SECRET-OUTPUT\nexit 3", 30, "failed", Some(3)),
        ("sleep 20", 1, "timed_out", None),
    ] {
        let bench = Bench::new().await;
        bench.merged();
        let release = bench.launcher("release", body, timeout);
        let after = bench.launcher("after", "exit 0", 30);
        bench.define(&release);
        bench.define(&after);
        bench.acting(vec![
            launcher_block("desa", Stage::PullRequestsCompleted, &release),
            launcher_block("later", Stage::PullRequestsCompleted, &after),
        ]);
        let id = bench.start(TASK).await;
        let run = bench.state(&id, Phase::NeedsAttention).await;
        assert_eq!(run.stopped_in, Some(Phase::Blocks));
        assert_eq!(run.block_outcomes.len(), 1);
        assert_eq!(run.block_outcomes[0].outcome, outcome);
        assert_eq!(run.block_outcomes[0].exit_code, code);
        assert!(run.attention.as_deref().unwrap().contains("desa"));
        // The stage stopped there, and nothing runs again until the user resumes.
        for _ in 0..3 {
            bench.tick().await;
        }
        assert_eq!((bench.ran("release"), bench.ran("after")), (1, 0));
        let seen = everything(&bench, &id).await;
        assert!(!seen.contains("SECRET"), "{seen}");
        // Resuming goes on with what has not run: the failed block is never run again.
        bench
            .op("workflow_run_resume", json!({"run":id,"confirmed":true}))
            .await
            .unwrap();
        let run = bench.state(&id, Phase::Done).await;
        assert_eq!((bench.ran("release"), bench.ran("after")), (1, 1));
        assert_eq!(run.block_outcomes.len(), 2);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_launcher_runs_once_at_a_time_and_a_run_waits_for_it_or_for_a_free_slot() {
    let bench = Bench::new().await;
    bench.merged();
    let release = bench.launcher("release", "exit 0", 30);
    bench.define(&release);
    bench.acting(vec![launcher_block(
        "desa",
        Stage::PullRequestsCompleted,
        &release,
    )]);
    // Another launcher running leaves room for it.
    let mut others = vec![consumers::busy_guard(&bench.host.state, "other-1")];
    // The same launcher is running: this run waits, with nothing recorded, then goes on.
    let busy = consumers::busy_guard(&bench.host.state, "release");
    let id = bench.start(TASK).await;
    bench.state(&id, Phase::Blocks).await;
    assert_eq!(
        acting::execute(&bench.host, &id, bench.now())
            .await
            .unwrap(),
        Progress::Waiting
    );
    assert!(bench.run(&id).block_outcomes.is_empty());
    // As many other launchers as the bound allows are running: it still waits.
    drop(busy);
    while others.len() < crate::consumers::MAX_RUNNING {
        let name = format!("other-{}", others.len() + 1);
        others.push(consumers::busy_guard(&bench.host.state, &name));
    }
    assert_eq!(
        acting::execute(&bench.host, &id, bench.now())
            .await
            .unwrap(),
        Progress::Waiting
    );
    assert!(bench.run(&id).block_outcomes.is_empty());
    others.pop();
    bench.state(&id, Phase::Done).await;
    assert_eq!(bench.ran("release"), 1);
    drop(others);
}

#[cfg(unix)]
#[tokio::test]
async fn the_engines_authority_reaches_only_its_two_paths_and_no_other_caller_holds_it() {
    let bench = Bench::new().await;
    bench.merged();
    let release = bench.launcher("release", "exit 0", 30);
    bench.define(&release);
    let teams = Teams::new(&bench.host, &["Victoria Gómez"], 201).await;
    teams.attach(&bench.host).await;
    let scope = bench.host.state.authority.clone();
    let as_ = |transport| authority::Invocation {
        scope: scope.clone(),
        transport,
    };
    // Every transport a client can come in on is refused, with nothing sent and nothing run.
    for transport in [
        Transport::Local,
        Transport::Window,
        Transport::Browser,
        Transport::Machine,
        Transport::Agent,
    ] {
        let (posted, _) =
            authority::within(as_(transport), acting::post(&bench.host, CHAT, "x")).await;
        assert_eq!(posted.outcome, "refused", "{transport:?}");
        let launched = authority::within(
            as_(transport),
            consumers::run_for_block(
                &bench.host,
                "release",
                &release.digest,
                consumers::busy_guard(&bench.host.state, "release"),
            ),
        )
        .await;
        assert!(launched.is_err(), "{transport:?}");
        // Arguments cannot name it either.
        let refused = call_as(
            &bench,
            transport,
            "workflow_runs",
            json!({"transport":"workflow","authority":"workflow"}),
        )
        .await;
        assert!(!refused.ok, "{transport:?}");
    }
    // Code that is not under an invocation at all (the default is the local channel) is refused.
    let (posted, _) = acting::post(&bench.host, CHAT, "x").await;
    assert_eq!(posted.outcome, "refused");
    assert!(
        consumers::run_for_block(
            &bench.host,
            "release",
            &release.digest,
            consumers::busy_guard(&bench.host.state, "release")
        )
        .await
        .is_err()
    );
    assert!(teams.posts().await.is_empty());
    assert_eq!(bench.ran("release"), 0);
    // The engine's authority is not a client: no operation answers it.
    for method in [
        "status",
        "teams_post",
        "variable_launcher_run",
        "workflow_runs",
    ] {
        let refused = call_as(&bench, Transport::Workflow, method, json!({})).await;
        assert!(!refused.ok, "{method}");
    }
    let mut request = control::Request::new("workflow_runs");
    request.args = json!({});
    let invocation = authority::workflow(&bench.host.state).unwrap();
    assert!(
        authority::within(invocation, async {
            authority::authorize(&bench.host.state, &request)
        })
        .await
        .is_err()
    );
    // With it, the two paths work.
    let invocation = authority::workflow(&bench.host.state).unwrap();
    let (posted, _) = authority::within(
        invocation.clone(),
        acting::post(&bench.host, CHAT, "@[Victoria] hi"),
    )
    .await;
    assert_eq!(posted.outcome, "sent");
    let launched = authority::within(
        invocation,
        consumers::run_for_block(
            &bench.host,
            "release",
            &release.digest,
            consumers::busy_guard(&bench.host.state, "release"),
        ),
    )
    .await;
    assert!(matches!(launched, Ok(consumers::BlockLaunch::Ran(_))));
}

/// No code that reaches the host for a client names the engine's authority: the control token,
/// the browser and machine credentials, the remote MCP endpoint, `pta mcp` and `pta tools call`
/// all map to the other transports.
#[test]
fn no_client_path_names_the_workflow_authority() {
    let clients = [
        ("control", include_str!("../../../control.rs")),
        ("web", include_str!("../../../web.rs")),
        ("agent", include_str!("../../../agent.rs")),
        ("web/agents", include_str!("../../../web/agents.rs")),
        ("cli", include_str!("../../../cli.rs")),
        ("mcp", include_str!("../../../mcp.rs")),
        ("page", include_str!("../../../page.rs")),
        ("tools", include_str!("../../../tools/mod.rs")),
        (
            "machines",
            include_str!("../../../implementation/management.rs"),
        ),
        ("headless", include_str!("../../../headless.rs")),
    ];
    for (name, source) in clients {
        assert!(
            !source.contains("Transport::Workflow") && !source.contains("authority::workflow("),
            "{name}"
        );
    }
}

async fn call_as(bench: &Bench, transport: Transport, method: &str, args: Value) -> control::Reply {
    let mut request = control::Request::new(method);
    request.args = args;
    authority::within(
        authority::Invocation {
            scope: bench.host.state.authority.clone(),
            transport,
        },
        control::dispatch(&bench.host, request),
    )
    .await
}

#[test]
fn the_occurrences_follow_the_record_and_a_template_is_rendered_by_code() {
    let item = crate::app::workflows::record::Item {
        provider: "azure".into(),
        id: 7,
        project: "Payments".into(),
        organization: synthetic::ORGANIZATION.into(),
        url: "https://dev.azure.com/example/Payments/_workitems/edit/7".into(),
        title: "Pay @[Boss] {link}".into(),
        kind: "User Story".into(),
        category: "requirement".into(),
        state: "Ready".into(),
        source: None,
    };
    let mut run = Run::new(item, 1);
    assert!(acting::occurrences(&run).is_empty());
    run.state = Phase::Done;
    assert!(acting::occurrences(&run).is_empty());
    run.state = Phase::Blocks;
    let keys: Vec<String> = acting::occurrences(&run)
        .into_iter()
        .map(|o| o.key)
        .collect();
    assert_eq!(keys, ["done"]);
    let block = notification(
        "n",
        Stage::Done,
        "{item}|{link}|{state}|{summary}|{pull_requests}|{missing}|{",
        &[],
    );
    let occurrence = Occurrence {
        stage: Stage::Done,
        key: "done".into(),
    };
    // The title cannot mention anybody, nor name another placeholder; no summary is empty.
    assert_eq!(
        acting::render(&run, &block, &occurrence),
        "Pay @\u{200b}[Boss] {link} (#7)|https://dev.azure.com/example/Payments/_workitems/edit/7|blocks|||{missing}|{"
            .replace("blocks", "done")
    );
    run.summaries.push(json!({"text":"first"}));
    run.summaries.push(json!({"text":"latest"}));
    assert!(acting::render(&run, &block, &occurrence).contains("|latest|"));
}

/// The record as 0.12.3 wrote it: its acting blocks were noted as not run, and nothing marks
/// when the engine began to act.
fn legacy(bench: &Bench, id: &str) {
    let mut run = bench.run(id);
    run.acting_since = None;
    run.block_outcomes = run
        .blocks
        .iter()
        .filter(|b| b.kind != Kind::Instruction)
        .map(|b| crate::app::workflows::record::BlockOutcome {
            block: b.id.clone(),
            kind: b.kind,
            stage: b.stage,
            outcome: "not_run: this version records acting blocks without running them".into(),
            at: 1,
            occurrence: String::new(),
            reason: None,
            exit_code: None,
            duration_ms: None,
            message_id: None,
        })
        .collect();
    let mut value = serde_json::to_value(&run).unwrap();
    assert!(value.get("acting_since").is_none());
    for outcome in value["block_outcomes"].as_array_mut().unwrap() {
        assert!(outcome.get("occurrence").is_none());
    }
    // Read back as the older record is read.
    let run: Run = serde_json::from_value(value).unwrap();
    bench.save(&run);
}

fn skipped(run: &Run) -> Vec<String> {
    run.block_outcomes
        .iter()
        .filter(|o| o.outcome == "skipped")
        .map(|o| format!("{}:{}", o.block, o.occurrence))
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_recorded_by_an_older_version_never_acts_on_what_already_happened() {
    // Failed long ago: its failed blocks, a notification and a launcher, never run.
    let bench = Bench::new().await;
    let release = bench.launcher("release", "exit 0", 30);
    bench.define(&release);
    bench.acting(vec![
        notification("failed", Stage::Failed, "{item} {state}", &[]),
        launcher_block("again", Stage::Failed, &release),
    ]);
    let teams = Teams::new(&bench.host, &["Ana Pérez"], 201).await;
    let id = bench.start(TASK).await;
    let mut run = bench.run(&id);
    run.stop(Phase::Failed, bench.now(), "Failed under 0.12.3.".into());
    bench.save(&run);
    legacy(&bench, &id);
    teams.attach(&bench.host).await;
    for _ in 0..3 {
        bench.tick().await;
    }
    assert!(teams.posts().await.is_empty());
    assert_eq!(bench.ran("release"), 0);
    let run = bench.run(&id);
    assert!(run.acting_since.is_some());
    assert_eq!(
        skipped(&run),
        ["failed:failed:2", "again:failed:2"].map(String::from)
    );
    assert_eq!(run.state, Phase::Failed);

    // Coding, after the plan and the children: those stages passed, what comes later acts.
    let bench = Bench::new().await;
    bench.acting(vec![
        notification("plan", Stage::PlanReady, "plan {item}", &[]),
        notification("created", Stage::ChildrenCreated, "created {item}", &[]),
        notification("child", Stage::ChildDone, "child {item}", &[]),
        notification("finished", Stage::Done, "finished {item}", &[]),
    ]);
    bench.pulls(WEB, "payments-web", "feature/child-1", 11, "completed");
    bench.pulls(API, "payments-api", "feature/child-2", 12, "completed");
    let teams = Teams::new(&bench.host, &["Ana Pérez"], 201).await;
    teams.attach(&bench.host).await;
    bench.mode("mode", "block");
    let id = bench.start(HU).await;
    let run = bench.state(&id, Phase::PlanReview).await;
    bench.confirm(&run, "created", &[101, 102]).await;
    bench
        .until(&id, |r| {
            r.children
                .first()
                .and_then(|c| c.coding.as_ref())
                .is_some_and(|c| c.started)
        })
        .await;
    let before = teams.posts().await.len();
    assert_eq!(before, 2);
    legacy(&bench, &id);
    bench.release();
    bench.state(&id, Phase::Done).await;
    let posts = teams.posts().await;
    assert_eq!(posts.len(), before + 3, "{posts:?}");
    assert!(posts[before].contains("child ") && posts[before + 1].contains("child "));
    assert!(posts[before + 2].contains("finished "));
    let run = bench.run(&id);
    assert_eq!(
        skipped(&run).len(),
        2,
        "{:?}",
        run.block_outcomes
            .iter()
            .map(|o| (&o.block, &o.occurrence, &o.outcome))
            .collect::<Vec<_>>()
    );

    // Waiting for pull requests: the children done are past; the completion acts.
    let bench = Bench::new().await;
    bench.acting(vec![
        notification("child", Stage::ChildDone, "child {item}", &[]),
        notification("finished", Stage::Done, "finished {item}", &[]),
    ]);
    let teams = Teams::new(&bench.host, &["Ana Pérez"], 201).await;
    let id = bench.start(TASK).await;
    teams.attach(&bench.host).await;
    bench.state(&id, Phase::PullRequests).await;
    assert_eq!(teams.posts().await.len(), 1);
    legacy(&bench, &id);
    bench.merged();
    bench.state(&id, Phase::Done).await;
    let posts = teams.posts().await;
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert!(posts[1].contains("finished "));
    assert_eq!(
        skipped(&bench.run(&id)),
        ["child:child_done:200".to_owned()]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_runs_launcher_waits_while_it_is_running_elsewhere_and_then_runs() {
    let bench = Bench::new().await;
    let release = bench.launcher("release", "exit 0", 30);
    bench.define(&release);
    bench.acting(vec![launcher_block("again", Stage::Failed, &release)]);
    let id = bench.start(TASK).await;
    let mut run = bench.run(&id);
    run.definition.as_mut().unwrap().checkouts.clear();
    bench.save(&run);
    let busy = consumers::busy_guard(&bench.host.state, "release");
    bench.state(&id, Phase::Failed).await;
    for _ in 0..3 {
        bench.tick().await;
    }
    // Nothing recorded, nothing run: it is still due, and the sweep keeps asking.
    assert!(bench.run(&id).block_outcomes.is_empty());
    assert_eq!(bench.ran("release"), 0);
    drop(busy);
    bench.tick().await;
    assert_eq!(bench.run(&id).block_outcomes[0].outcome, "succeeded");
    assert_eq!(bench.ran("release"), 1);
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(bench.ran("release"), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn a_launcher_whose_outcome_is_unknown_stops_a_running_run_and_every_attempt_is_settled_at_start()
 {
    let mut bench = Bench::new().await;
    let release = bench.launcher("release", "exit 0", 30);
    bench.define(&release);
    bench.acting(vec![
        launcher_block("desa", Stage::PullRequestsCompleted, &release),
        notification("note", Stage::Done, "{item}", &[]),
    ]);
    let waiting = bench.start(TASK).await;
    bench.state(&waiting, Phase::PullRequests).await;
    let attempt =
        |block: &str, kind, stage, occurrence: &str| crate::app::workflows::record::BlockOutcome {
            block: block.into(),
            kind,
            stage: Some(stage),
            outcome: "attempting".into(),
            at: 1,
            occurrence: occurrence.into(),
            reason: None,
            exit_code: None,
            duration_ms: None,
            message_id: None,
        };
    // A run the host stopped in while its launcher ran.
    let mut run = bench.run(&waiting);
    run.block_outcomes.push(attempt(
        "desa",
        Kind::Launcher,
        Stage::PullRequestsCompleted,
        "pull_requests_completed",
    ));
    bench.save(&run);
    // And a run that was already stopped, with a notification left in flight.
    let stopped = bench.start(HU).await;
    let mut run = bench.run(&stopped);
    run.block_outcomes.push(attempt(
        "note",
        Kind::TeamsNotification,
        Stage::Done,
        "done",
    ));
    run.stop(Phase::NeedsAttention, bench.now(), "Look at it.".into());
    bench.save(&run);
    bench.restart().await;
    bench.tick().await;
    let run = bench.run(&waiting);
    assert_eq!(run.state, Phase::NeedsAttention);
    assert_eq!(run.stopped_in, Some(Phase::PullRequests));
    assert_eq!(run.block_outcomes[0].outcome, "uncertain");
    assert!(run.attention.as_deref().unwrap().contains("desa"));
    let run = bench.run(&stopped);
    assert_eq!(run.state, Phase::NeedsAttention);
    assert_eq!(run.block_outcomes[0].outcome, "uncertain");
    assert_eq!(bench.ran("release"), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn a_launcher_result_that_returns_after_the_run_was_resumed_does_not_stop_it() {
    let bench = Bench::new().await;
    let release = bench.launcher("release", "sleep 1\nexit 1", 30);
    bench.define(&release);
    bench.acting(vec![launcher_block("again", Stage::Failed, &release)]);
    let id = bench.start(TASK).await;
    let mut run = bench.run(&id);
    let checkouts = run.definition.as_ref().unwrap().checkouts.clone();
    run.definition.as_mut().unwrap().checkouts.clear();
    bench.save(&run);
    let driver = async {
        while bench.ran("release") == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // The user fixed the checkout and resumed while the launcher was still running.
        let mut run = bench.run(&id);
        run.definition.as_mut().unwrap().checkouts = checkouts.clone();
        bench.save(&run);
        bench
            .op("workflow_run_resume", json!({"run":id,"confirmed":true}))
            .await
            .unwrap();
    };
    tokio::join!(bench.tick(), driver);
    let run = bench.run(&id);
    assert_eq!(run.block_outcomes[0].outcome, "failed");
    assert_ne!(run.state, Phase::NeedsAttention, "{:?}", run.attention);
    assert_eq!(bench.ran("release"), 1);
}

#[tokio::test]
async fn a_failure_to_log_after_the_request_went_out_stays_uncertain() {
    let bench = Bench::new().await;
    bench.merged();
    bench.acting(vec![notification("notify", Stage::Done, "{item}", &[])]);
    let teams = Teams::new(&bench.host, &["Ana Pérez"], 500).await;
    // The event log cannot be written.
    rusqlite::Connection::open(teams.dir.path().join("graph.db"))
        .unwrap()
        .execute("DROP TABLE events", [])
        .unwrap();
    let id = bench.start(TASK).await;
    teams.attach(&bench.host).await;
    let run = bench.state(&id, Phase::Done).await;
    assert_eq!(teams.posts().await.len(), 1);
    assert_eq!(run.block_outcomes[0].outcome, "uncertain");
}

#[test]
fn mention_syntax_in_any_rendered_text_is_inert_except_the_blocks_own_mentions() {
    let item = crate::app::workflows::record::Item {
        provider: "azure".into(),
        id: 7,
        project: "Payments".into(),
        organization: synthetic::ORGANIZATION.into(),
        url: "https://dev.azure.com/example/Payments/_workitems/edit/7".into(),
        title: "[Boss] fixes".into(),
        kind: "Task".into(),
        category: "task".into(),
        state: "Ready".into(),
        source: None,
    };
    let mut run = Run::new(item, 1);
    run.state = Phase::Blocks;
    // A template's own @ before a title that starts with [ must not become a mention.
    let block = notification("n", Stage::Done, "@{item} and @[Template] ", &["Victoria"]);
    let occurrence = Occurrence {
        stage: Stage::Done,
        key: "done".into(),
    };
    let rendered = acting::render(&run, &block, &occurrence);
    assert!(!rendered.contains("@["), "{rendered}");
    // Only the block's own mention is a mention in what is posted.
    let message = acting::compose(&run, &block, &occurrence).unwrap();
    assert_eq!(message.matches("@[").count(), 1, "{message}");
    assert!(message.starts_with("@[Victoria] "));
    // A mention that a Teams post could not read is refused, as saving refuses it.
    for name in ["a]b".to_owned(), "x".repeat(101)] {
        let mut bad = block.clone();
        bad.mentions = vec![name];
        assert!(acting::compose(&run, &bad, &occurrence).is_err());
        assert!(crate::workflows::validate_block(&bad).is_err());
    }
    let mut longest = block.clone();
    longest.mentions = vec!["x".repeat(100)];
    assert!(acting::compose(&run, &longest, &occurrence).is_ok());
    assert!(crate::workflows::validate_block(&longest).is_ok());
}
