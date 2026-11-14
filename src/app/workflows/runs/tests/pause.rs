//! Pausing the runs and restarting one with the user's handover note, end to end with the
//! engine's synthetic catalog and fake provider CLI: the note reaches the new coding run's
//! prompt and nothing else, the two runs name each other, a pause starts nothing and keeps the
//! line's order, and its choice for the live sessions is kept.
use super::*;

const NOTE: &str = "Branch feature/child-1 is pushed; do not push it again.";
const LEFT: &str = "Left to do: open its pull request.";

async fn pause(bench: &Bench, args: Value) -> Result<Value, String> {
    let mut args = args;
    args["confirmed"] = json!(true);
    bench.op("workflow_runs_pause", args).await
}
async fn unpause(bench: &Bench) -> Value {
    bench
        .op("workflow_runs_unpause", json!({"confirmed": true}))
        .await
        .unwrap()
}
/// Tick until the run's coding run started, then return its ID.
async fn coding_started(bench: &Bench, id: &str) -> String {
    let run = bench
        .until(id, |r| {
            r.children
                .first()
                .and_then(|c| c.coding.as_ref())
                .is_some_and(|c| c.started)
        })
        .await;
    run.children[0].coding.clone().unwrap().id
}

#[tokio::test]
async fn a_restart_cancels_the_run_and_its_new_coding_run_gets_the_handover_note_only() {
    let bench = Bench::new().await;
    bench.mode("mode", "fail");
    let old = bench.start(TASK).await;
    let stopped = bench.state(&old, Phase::NeedsAttention).await;
    let note = format!("{NOTE}\n{LEFT}");

    // Unconfirmed, refused with what it does; too long or not plain text, refused; nothing moves.
    let asked = bench
        .op("workflow_run_restart", json!({"run": old, "note": note}))
        .await
        .unwrap_err();
    assert!(asked.contains("Confirm"), "{asked}");
    assert!(
        asked.contains(&format!("({} characters)", note.chars().count())),
        "{asked}"
    );
    assert!(
        asked.contains(engine::HANDOVER) && asked.contains("posted nowhere"),
        "{asked}"
    );
    let long = "x".repeat(record::MAX_HANDOVER + 1);
    for refused in [long.as_str(), "Done.\u{7}"] {
        let error = bench
            .op(
                "workflow_run_restart",
                json!({"run": old, "note": refused, "confirmed": true}),
            )
            .await
            .unwrap_err();
        assert!(error.starts_with("[invalid_input]"), "{error}");
    }
    assert_eq!(bench.run(&old), stopped);

    bench.mode("mode", "done");
    let reply = bench
        .op(
            "workflow_run_restart",
            json!({"run": old, "note": note, "confirmed": true}),
        )
        .await
        .unwrap();
    let new = reply["run"]["id"].as_str().unwrap().to_owned();
    assert_eq!(reply["previous"], old.as_str());
    assert_eq!(reply["paused"], false);

    // The old run is cancelled as restarted, the new one starts from the first stage with the note.
    let previous = bench.run(&old);
    assert_eq!(previous.state, Phase::Cancelled);
    assert_eq!(previous.restarted_as.as_deref(), Some(new.as_str()));
    // It had stopped: its last stage keeps the outcome it stopped with.
    assert_eq!(
        previous.timeline.last().unwrap().outcome.as_deref(),
        Some("needs_attention")
    );
    let next = bench.run(&new);
    assert_eq!(next.continues.as_deref(), Some(old.as_str()));
    assert_eq!(next.handover.as_deref(), Some(note.as_str()));
    assert_eq!(next.state, Phase::Coding);
    assert_eq!(next.workflow.as_deref(), Some("payments"));
    assert!(next.children[0].coding.is_none());

    // The new coding run's prompt carries the note under its heading, before the workflow's own
    // instructions; the first coding run had none.
    bench.state(&new, Phase::PullRequests).await;
    let first = fs::read_to_string(bench.log.join("coding.1")).unwrap();
    assert!(!first.contains(engine::HANDOVER));
    let prompt = fs::read_to_string(bench.log.join("coding.2")).unwrap();
    let heading = prompt.find(engine::HANDOVER).expect("the heading");
    let (at_note, at_left) = (prompt.find(NOTE).unwrap(), prompt.find(LEFT).unwrap());
    assert!(heading < at_note && at_note < at_left, "{prompt}");
    assert!(
        at_left < prompt.find("Write tests first.").unwrap(),
        "{prompt}"
    );

    // The two runs name each other in their status and in the list.
    let status = |id: &str| bench.op("workflow_run_status", json!({"run": id}));
    assert_eq!(
        status(&old).await.unwrap()["run"]["restarted_as"],
        new.as_str()
    );
    assert_eq!(
        status(&new).await.unwrap()["run"]["continues"],
        old.as_str()
    );
    let listed = bench.op("workflow_runs", json!({})).await.unwrap();
    let row = |id: &str| {
        listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(row(&old)["restarted_as"], new.as_str());
    assert_eq!(row(&new)["continues"], old.as_str());

    // The director's summary, which a person may send to Teams, is written without the note.
    let summarized = super::super::super::summary::input(
        &bench.run(&new),
        crate::config::Language::default(),
        bench.now(),
    );
    assert!(!summarized.to_string().contains(NOTE));
    // The engine wrote nowhere: Azure DevOps was only read, and only the coding run got the note.
    assert!(bench.catalog.only_reads());
    let notes = fs::read_dir(&bench.log)
        .unwrap()
        .filter_map(|e| fs::read_to_string(e.ok()?.path()).ok())
        .filter(|t| t.contains(NOTE))
        .count();
    // The CLI's own copy of its prompt (`prompt.PID`) and the one it keeps (`coding.2`).
    assert_eq!(notes, 2);

    // A finished run, and a run without a workflow, are not restarted.
    let error = bench
        .op(
            "workflow_run_restart",
            json!({"run": old, "confirmed": true}),
        )
        .await
        .unwrap_err();
    assert!(error.starts_with("[conflict]"), "{error}");
    let mut unassigned = Run::new(previous.item.clone(), bench.now());
    unassigned.state = Phase::Unassigned;
    unassigned.item.url.push_str("?unassigned");
    bench.host.state.workflows.db.insert(&unassigned).unwrap();
    let error = bench
        .op(
            "workflow_run_restart",
            json!({"run": unassigned.id, "confirmed": true}),
        )
        .await
        .unwrap_err();
    assert!(error.contains("no workflow yet"), "{error}");
}

#[tokio::test]
async fn while_paused_nothing_starts_and_after_unpause_the_queued_runs_start_in_their_order() {
    let bench = Bench::new().await;
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflow_settings.concurrency = 1;
    map.workflow_settings.ready_states = vec![crate::workflows::director::ReadyStates {
        organization: synthetic::ORGANIZATION.into(),
        project: "Payments".into(),
        states: vec!["Ready".into()],
    }];
    map.workflows[0].auto_start = true;
    bench.host.state.settings.save_map(&map).unwrap();
    // Only the items started by hand below are in the ready state.
    for id in [HU, 101, 102, TASK] {
        bench.catalog.set_state(id, "New");
    }
    for item in 201..=203 {
        bench.catalog.add(
            synthetic::detail(item, "Task", "task", &format!("Task {item}"), "Anywhere."),
            true,
        );
    }

    // Unconfirmed, a pause records nothing; without the choice for the sessions it is refused.
    let asked = bench
        .op("workflow_runs_pause", json!({"sessions": "finish"}))
        .await
        .unwrap_err();
    assert!(
        asked.contains("Confirm") && asked.contains("left to finish"),
        "{asked}"
    );
    assert!(
        bench
            .op("workflow_runs_pause", json!({"confirmed": true}))
            .await
            .is_err()
    );
    assert!(bench.host.state.workflows.db.pauses().unwrap().is_empty());
    let paused = pause(&bench, json!({"sessions": "finish"})).await.unwrap();
    assert_eq!(paused["paused"][0]["sessions"], "finish");
    assert!(paused["paused"][0]["project"].is_null());

    // Started while paused, the runs are recorded and wait: no slot, no coding run.
    let mut ids = Vec::new();
    for item in 201..=203 {
        ids.push(bench.start(item).await);
    }
    for _ in 0..4 {
        bench.tick().await;
    }
    assert_eq!(bench.count("starts"), 0);
    for id in &ids {
        let run = bench.run(id);
        assert_eq!(run.state, Phase::Coding);
        assert!(!run.admitted && run.children[0].coding.is_none());
    }
    // The pause is shown: in the list, on each run it holds, in the settings and the watcher.
    let listed = bench.op("workflow_runs", json!({})).await.unwrap();
    assert_eq!(listed["paused"][0]["sessions"], "finish");
    for id in &ids {
        let row = listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id.as_str())
            .unwrap();
        assert_eq!(row["paused"], true);
        assert_eq!(row["waiting_for_slot"], false);
    }
    let settings =
        super::super::super::director::operation(&bench.host, "workflow_settings", &json!({}))
            .await
            .unwrap();
    assert_eq!(settings["paused"][0]["sessions"], "finish");
    assert_eq!(settings["watcher"]["state"], "paused");

    // A poll while paused starts nothing and records nothing.
    bench.catalog.set_state(HU, "Ready");
    let poll = super::super::super::watcher::poll(&bench.host, &bench.azure, bench.now()).await;
    assert_eq!(poll["started"], 0);
    assert_eq!(poll["projects"][0]["paused"], true);
    assert_eq!(bench.host.state.workflows.watcher.seen(), 0);
    assert!(
        bench
            .host
            .state
            .workflows
            .db
            .active_for(&format!(
                "https://dev.azure.com/example/Payments/_workitems/edit/{HU}"
            ))
            .unwrap()
            .is_none()
    );

    // Unpaused, the queued runs start in the order they were created, one at a time.
    let unpaused = unpause(&bench).await;
    assert_eq!(unpaused["paused"], json!([]));
    assert!(
        bench
            .op("workflow_runs_unpause", json!({"confirmed": true}))
            .await
            .unwrap_err()
            .contains("No workflow run is paused")
    );
    for id in &ids {
        bench.state(id, Phase::PullRequests).await;
    }
    assert_eq!(coded(&bench), vec![201, 202, 203]);
}

#[tokio::test]
async fn a_pause_of_one_project_holds_only_its_runs() {
    let bench = Bench::new().await;
    bench.catalog.add(
        synthetic::detail(201, "Task", "task", "Task 201", "Anywhere."),
        true,
    );
    pause(&bench, json!({"project": "Billing", "sessions": "finish"}))
        .await
        .unwrap();
    let id = bench.start(TASK).await;
    bench.state(&id, Phase::PullRequests).await;
    let error = bench
        .op(
            "workflow_runs_unpause",
            json!({"project": "Payments", "confirmed": true}),
        )
        .await
        .unwrap_err();
    assert!(error.contains("no pause of its own"), "{error}");
    // Paused by name in any case, the project's next run waits.
    pause(&bench, json!({"project": "payments", "sessions": "finish"}))
        .await
        .unwrap();
    bench.pulls(WEB, "payments-web", "feature/child-1", 21, "completed");
    let held = bench.start(201).await;
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(bench.count("starts"), 1);
    assert!(bench.run(&held).children[0].coding.is_none());
    bench
        .op(
            "workflow_runs_unpause",
            json!({"project": "Payments", "confirmed": true}),
        )
        .await
        .unwrap();
    bench.state(&held, Phase::PullRequests).await;
}

#[tokio::test]
async fn pausing_with_stop_ends_the_live_sessions_and_with_finish_it_does_not() {
    // Let them finish: the session keeps running, and its end is recorded while paused.
    let bench = Bench::new().await;
    bench.mode("mode", "block");
    let id = bench.start(TASK).await;
    let coding = coding_started(&bench, &id).await;
    let reply = pause(&bench, json!({"sessions": "finish"})).await.unwrap();
    assert_eq!(reply["sessions_stopped"], json!([]));
    assert_eq!(reply["interrupted"], json!([]));
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(bench.implementation(&coding).await, "running");
    assert_eq!(bench.run(&id).state, Phase::Coding);
    bench.release();
    let run = bench.state(&id, Phase::PullRequests).await;
    assert_eq!(run.children[0].outcome.as_deref(), Some("done"));
    // Its pull requests are still read, but the stage of the blocks waits for the pause to end.
    bench.pulls(WEB, "payments-web", "feature/child-1", 21, "completed");
    bench.state(&id, Phase::Blocks).await;
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(bench.run(&id).state, Phase::Blocks);
    unpause(&bench).await;
    bench.state(&id, Phase::Done).await;
    drop(bench);

    // Stop now: the live session ends and its run is interrupted, to restart or resume.
    let bench = Bench::new().await;
    bench.mode("mode", "block");
    let id = bench.start(TASK).await;
    let coding = coding_started(&bench, &id).await;
    let asked = bench
        .op("workflow_runs_pause", json!({"sessions": "stop"}))
        .await
        .unwrap_err();
    assert!(asked.contains("stopped now"), "{asked}");
    assert_eq!(bench.implementation(&coding).await, "running");
    let reply = pause(&bench, json!({"sessions": "stop"})).await.unwrap();
    assert_eq!(reply["sessions_stopped"], json!([coding]));
    assert_eq!(reply["interrupted"], json!([id]));
    assert_eq!(bench.implementation(&coding).await, "cancelled");
    let run = bench.run(&id);
    assert_eq!(run.state, Phase::Interrupted);
    assert_eq!(run.stopped_in, Some(Phase::Coding));
    assert!(
        run.attention
            .as_deref()
            .unwrap()
            .contains("workflow_run_restart")
    );
    // A restart while paused records the new run, which waits for the pause to end. (The
    // stopped session may have been ended before the fake CLI counted its start.)
    let before = bench.count("starts");
    let reply = bench
        .op(
            "workflow_run_restart",
            json!({"run": id, "note": NOTE, "confirmed": true}),
        )
        .await
        .unwrap();
    assert_eq!(reply["paused"], true);
    let new = reply["run"]["id"].as_str().unwrap().to_owned();
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(bench.count("starts"), before);
    assert!(bench.run(&new).children[0].coding.is_none());
    unpause(&bench).await;
    bench.mode("mode", "done");
    bench.state(&new, Phase::PullRequests).await;
    let last = bench.count("starts");
    assert_eq!(last, before + 1);
    assert!(
        fs::read_to_string(bench.log.join(format!("coding.{last}")))
            .unwrap()
            .contains(NOTE)
    );
}

#[test]
fn a_paused_run_is_out_of_the_line_and_back_in_its_place_after() {
    let item = |id: u64, project: &str| Item {
        id,
        project: project.into(),
        url: format!("https://dev.azure.com/example/{project}/_workitems/edit/{id}"),
        ..Default::default()
    };
    let mut runs: Vec<Run> = [(1, "Payments"), (2, "Billing"), (3, "Payments")]
        .into_iter()
        .map(|(id, project)| Run::new(item(id, project), 100))
        .collect();
    for run in &mut runs {
        run.state = Phase::Coding;
    }
    let ids: Vec<String> = runs.iter().map(|r| r.id.clone()).collect();
    let payments = [record::Pause {
        project: Some("PAYMENTS".into()),
        sessions: record::Sessions::Finish,
        paused_at: 100,
    }];
    assert!(record::may_take_slot(&runs, &ids[1], 1, 100, &payments));
    assert!(!record::may_take_slot(&runs, &ids[0], 1, 100, &payments));
    let places = record::positions(&runs, 100, &payments);
    assert_eq!((places.get(&ids[0]), places[&ids[1]]), (None, 1));
    let places = record::positions(&runs, 100, &[]);
    assert_eq!(
        (places[&ids[0]], places[&ids[1]], places[&ids[2]]),
        (1, 2, 3)
    );
}
