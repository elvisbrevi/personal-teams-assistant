//! The watcher over the engine's synthetic Azure catalog and fake provider CLI: items entering
//! and leaving a ready state, an item seen twice, the cases that start nothing, the bounds and
//! where it does not run. Every read is recorded by the catalog, which has no way to write.
use super::super::record::State as Phase;
use super::super::runs::tests::{Bench, HU};
use super::*;
use crate::store::settings::SettingsStore;
use crate::workflows::director::ReadyStates;
use std::fs;

fn set_ready(bench: &Bench, states: &[&str]) {
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflow_settings.ready_states = if states.is_empty() {
        Vec::new()
    } else {
        vec![ReadyStates {
            organization: super::super::synthetic::ORGANIZATION.into(),
            project: "Payments".into(),
            states: states.iter().map(|s| (*s).to_owned()).collect(),
        }]
    };
    bench.host.state.settings.save_map(&map).unwrap();
}
fn edit_workflow(bench: &Bench, change: impl FnOnce(&mut crate::workflows::Workflow)) {
    let mut map = bench.host.state.settings.map().unwrap();
    change(&mut map.workflows[0]);
    bench.host.state.settings.save_map(&map).unwrap();
}
/// Only the HU is in the ready state.
fn only_the_hu(bench: &Bench) {
    for id in [101, 102, 200] {
        bench.catalog.set_state(id, "New");
    }
}
async fn poll(bench: &Bench) -> Value {
    super::poll(&bench.host, &bench.azure, bench.now()).await
}
fn runs(bench: &Bench) -> Vec<super::super::record::Run> {
    bench.host.state.workflows.db.list(&[], None, -1).unwrap()
}
fn url(id: u64) -> String {
    format!("https://dev.azure.com/example/Payments/_workitems/edit/{id}")
}
fn answer(bench: &Bench, workflow: &str) {
    let answer = json!({"workflow":workflow,"reason":"Habla de pagos."});
    let line = json!({"type":"text","part":{"text":answer.to_string()}});
    fs::write(bench.log.join("director.jsonl"), format!("{line}\n")).unwrap();
}

#[tokio::test]
async fn an_item_entering_a_ready_state_starts_one_run_once_and_again_after_it_leaves_and_returns()
{
    let bench = Bench::new().await;
    bench.director();
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    let watcher = &bench.host.state.workflows.watcher;

    // Due now: one run, started by the watcher (its director chooses among the auto workflows).
    super::tick(&bench.host, &bench.azure, 1_000).await;
    let started = runs(&bench);
    assert_eq!(started.len(), 1);
    assert!(started[0].auto_start && started[0].item.id == HU);
    assert_eq!(started[0].state, Phase::Classifying);
    assert_eq!(watcher.rows(&url(HU)).len(), 1);
    assert_eq!(bench.reads("list Payments Ready limit 100"), 1);
    let last = watcher.last().unwrap();
    assert_eq!(last["started"], 1);
    assert_eq!(last["projects"][0]["complete"], true);

    // Not due within the interval (five minutes), due after it.
    super::tick(&bench.host, &bench.azure, 1_000 + 299).await;
    assert_eq!(bench.reads("list Payments"), 1);
    super::tick(&bench.host, &bench.azure, 1_000 + 300).await;
    assert_eq!(bench.reads("list Payments"), 2);
    // The same item on the next poll starts nothing: not while its run is active, and not
    // after the run was cancelled either, while the item stays in the state.
    assert_eq!(runs(&bench).len(), 1);
    let mut run = runs(&bench).remove(0);
    run.cancel(2_000);
    bench.save(&run);
    let again = poll(&bench).await;
    assert_eq!(again["started"], 0);
    assert_eq!(again["projects"][0]["already_seen"], 1);
    assert_eq!(runs(&bench).len(), 1);

    // It leaves the state: forgotten. It comes back: seen again, a second run.
    bench.catalog.set_state(HU, "Active");
    let left = poll(&bench).await;
    assert_eq!(left["started"], 0);
    assert_eq!(watcher.seen(), 0);
    bench.catalog.set_state(HU, "Ready");
    let back = poll(&bench).await;
    assert_eq!(back["started"], 1);
    assert_eq!(runs(&bench).len(), 2);
    assert!(bench.catalog.only_reads());
}

#[tokio::test]
async fn the_ledger_records_the_item_its_state_and_its_changed_date() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    poll(&bench).await;
    let rows = bench.host.state.workflows.watcher.rows(&url(HU));
    assert_eq!(rows, vec![("Ready".to_owned(), None)]);
}

#[tokio::test]
async fn no_ready_state_no_auto_start_and_a_disabled_workflow_start_no_run_and_read_nothing() {
    let bench = Bench::new().await;
    bench.director();
    // Idle until a ready state is configured: no read, and the settings say so.
    super::tick(&bench.host, &bench.azure, 1_000).await;
    assert_eq!(bench.reads("list "), 0);
    assert_eq!(view(&bench.host.state)["state"], "idle");

    // A workflow that does not start by itself.
    set_ready(&bench, &["Ready"]);
    assert_eq!(view(&bench.host.state)["state"], "watching");
    let shown = poll(&bench).await;
    assert_eq!(shown["started"], 0);
    assert!(shown["projects"][0]["skipped"].is_string());
    // A disabled one, even with auto_start.
    edit_workflow(&bench, |w| {
        w.auto_start = true;
        w.enabled = false;
    });
    assert_eq!(poll(&bench).await["started"], 0);
    assert_eq!(bench.reads("list "), 0);
    assert!(runs(&bench).is_empty());
    assert_eq!(bench.count("starts") + bench.count("plans"), 0);
    // A project with no ready state is not read either.
    set_ready(&bench, &[]);
    edit_workflow(&bench, |w| w.enabled = true);
    assert_eq!(poll(&bench).await["projects"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_null_director_answer_and_a_workflow_that_does_not_start_by_itself_leave_an_unassigned_run()
 {
    let bench = Bench::new().await;
    bench.director();
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    // A second workflow of the project that is only started by hand.
    let mut map = bench.host.state.settings.map().unwrap();
    let mut manual = map.workflows[0].clone();
    manual.id = "manual".into();
    manual.name = "Manual".into();
    manual.auto_start = false;
    map.workflows[0].auto_start = true;
    map.workflows.push(manual);
    bench.host.state.settings.save_map(&map).unwrap();

    // The director says none: unassigned, no coding.
    answer(&bench, "");
    assert_eq!(poll(&bench).await["started"], 1);
    let id = runs(&bench)[0].id.clone();
    let run = bench.state(&id, Phase::Unassigned).await;
    assert!(run.attention.is_some());
    assert_eq!(bench.count("starts") + bench.count("plans"), 0);

    // It names the workflow that does not start by itself: not offered, so unassigned too.
    let mut run = bench.run(&id);
    run.cancel(5);
    bench.save(&run);
    bench.catalog.set_state(HU, "Active");
    poll(&bench).await;
    bench.catalog.set_state(HU, "Ready");
    answer(&bench, "manual");
    poll(&bench).await;
    let second = runs(&bench).into_iter().find(|r| r.id != id).unwrap();
    let run = bench.state(&second.id, Phase::Unassigned).await;
    assert!(run.workflow.is_none());
    assert_eq!(bench.count("starts") + bench.count("plans"), 0);
}

#[tokio::test]
async fn the_watcher_reads_only_bounds_each_poll_and_reports_partial_coverage() {
    let bench = Bench::new().await;
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    // The read's own bound: at most 100 items per project, over all its ready states.
    poll(&bench).await;
    assert_eq!(bench.reads("limit 100"), 1);
    assert!(bench.catalog.only_reads());

    // A catalog larger than the read returns: partial, and nothing is forgotten from it.
    let watcher = &bench.host.state.workflows.watcher;
    let before = watcher.seen();
    *bench.catalog.list_cap.lock().unwrap() = Some(1);
    let partial = poll(&bench).await;
    assert_eq!(partial["partial"], true);
    assert_eq!(partial["projects"][0]["complete"], false);
    assert_eq!(watcher.seen(), before);

    // A failed read is reported, starts nothing and forgets nothing.
    *bench.catalog.list_cap.lock().unwrap() = None;
    bench
        .catalog
        .failing_lists
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let failed = poll(&bench).await;
    assert_eq!(failed["partial"], true);
    assert_eq!(failed["started"], 0);
    assert!(
        failed["projects"][0]["errors"][0]
            .as_str()
            .unwrap()
            .contains("Ready")
    );
    assert_eq!(watcher.seen(), before);
}

#[tokio::test]
async fn one_poll_starts_a_bounded_number_of_runs_and_the_next_takes_the_rest() {
    let bench = Bench::new().await;
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    for id in 300..330 {
        bench.catalog.add(
            super::super::synthetic::detail(id, "Task", "task", "Extra", "Text."),
            true,
        );
    }
    // 34 items in the state: the first poll starts MAX_STARTS, the rest wait unrecorded.
    let first = poll(&bench).await;
    assert_eq!(first["started"], MAX_STARTS);
    assert_eq!(first["projects"][0]["deferred"], 34 - MAX_STARTS);
    assert_eq!(first["projects"][0]["complete"], false);
    let second = poll(&bench).await;
    assert_eq!(second["started"], 34 - MAX_STARTS);
    assert_eq!(runs(&bench).len(), 34);
}

#[tokio::test]
async fn an_item_that_could_not_be_read_for_its_start_is_tried_again_at_the_next_poll() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    bench.catalog.failing_items.lock().unwrap().insert(HU);
    let failed = poll(&bench).await;
    assert_eq!(failed["started"], 0);
    assert_eq!(bench.host.state.workflows.watcher.seen(), 0);
    assert!(runs(&bench).is_empty());
    bench.catalog.failing_items.lock().unwrap().clear();
    assert_eq!(poll(&bench).await["started"], 1);
}

#[tokio::test]
async fn it_does_not_run_on_a_computer_signed_in_to_a_coordinator() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    fs::write(
        bench
            .root
            .path()
            .join("profile/implementation/connection.json"),
        json!({"url":"https://coordinator.example","account":1,"id":bench.machine}).to_string(),
    )
    .unwrap();
    super::tick(&bench.host, &bench.azure, 1_000).await;
    assert_eq!(bench.reads("list "), 0);
    assert!(runs(&bench).is_empty());
    assert_eq!(view(&bench.host.state)["state"], "unavailable");
}

#[tokio::test]
async fn workflow_settings_show_the_watcher_and_its_last_poll() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    poll(&bench).await;
    let shown = super::super::director::operation(&bench.host, "workflow_settings", &json!({}))
        .await
        .unwrap();
    assert_eq!(shown["watcher"]["state"], "watching");
    assert_eq!(shown["watcher"]["interval_minutes"], 5);
    assert_eq!(shown["watcher"]["last_poll"]["started"], 1);
    assert!(shown["watcher"]["next_poll_at"].as_i64().unwrap() > 0);
}
