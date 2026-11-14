//! The watcher over the engine's synthetic Azure catalog and fake provider CLI: items entering
//! and leaving a ready state, an item seen twice, the cases that start nothing, the bounds and
//! where it does not run. Every read is recorded by the catalog, which has no way to write.
use super::super::record::State as Phase;
use super::super::runs::tests::{Bench, HU, TASK};
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
/// A second enabled workflow of the project that starts by itself, and one that does not.
fn second_and_manual(bench: &Bench) {
    let mut map = bench.host.state.settings.map().unwrap();
    let mut second = map.workflows[0].clone();
    second.id = "second".into();
    second.name = "Second".into();
    second.auto_start = true;
    let mut manual = map.workflows[0].clone();
    manual.id = "manual".into();
    manual.name = "Manual".into();
    manual.auto_start = false;
    map.workflows[0].auto_start = true;
    map.workflows.push(second);
    map.workflows.push(manual);
    bench.host.state.settings.save_map(&map).unwrap();
}
fn answer(bench: &Bench, workflow: &str) {
    let answer = json!({"workflow":workflow,"reason":"Habla de pagos."});
    let line = json!({"type":"text","part":{"text":answer.to_string()}});
    fs::write(bench.log.join("director.jsonl"), format!("{line}\n")).unwrap();
}

#[tokio::test]
async fn an_item_entering_a_ready_state_starts_one_run_once_and_again_after_it_leaves_and_returns()
{
    // No director configured: the one workflow that starts by itself lends its machine and
    // agent, and the director chooses it.
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    answer(&bench, "payments");
    let watcher = &bench.host.state.workflows.watcher;

    // Due now: one run, started by the watcher, classified by the director.
    super::tick(&bench.host, &bench.azure, 1_000).await;
    let started = runs(&bench);
    assert_eq!(started.len(), 1);
    assert!(started[0].auto_start && started[0].item.id == HU);
    assert_eq!(started[0].state, Phase::Classifying);
    let planned = bench.state(&started[0].id, Phase::Planning).await;
    assert_eq!(planned.workflow.as_deref(), Some("payments"));
    assert_eq!(bench.count("directions"), 1);
    // The ledger row names the run it asked for, and that run exists.
    let rows = watcher.rows(&url(HU));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2, started[0].id);
    assert_eq!(bench.reads("list Payments Ready limit 100"), 1);
    let last = watcher.last().unwrap();
    assert_eq!(last["started"], 1);
    assert_eq!(last["projects"][0]["complete"], true);

    // Not due within the interval (five minutes), due after it.
    // Each poll makes the state's two reads: the default list and the task kind.
    super::tick(&bench.host, &bench.azure, 1_000 + 299).await;
    assert_eq!(bench.reads("list Payments"), 2);
    super::tick(&bench.host, &bench.azure, 1_000 + 300).await;
    assert_eq!(bench.reads("list Payments"), 4);
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
async fn the_director_classifies_even_one_workflow_and_an_item_it_does_not_fit_waits_unassigned() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    answer(&bench, "");
    assert_eq!(poll(&bench).await["started"], 1);
    let id = runs(&bench)[0].id.clone();
    let run = bench.state(&id, Phase::Unassigned).await;
    assert!(run.workflow.is_none() && run.attention.is_some());
    assert_eq!(bench.count("directions"), 1);
    assert_eq!(bench.count("starts") + bench.count("plans"), 0);
}

#[tokio::test]
async fn without_a_director_that_can_classify_the_one_workflow_takes_the_item_directly() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    // The project's own director is on a machine this account does not have.
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflow_settings.project_directors = vec![crate::workflows::director::ProjectDirector {
        organization: super::super::synthetic::ORGANIZATION.into(),
        project: "Payments".into(),
        director: crate::workflows::director::Director {
            machine: uuid::Uuid::new_v4().to_string(),
            provider: "opencode".into(),
            model: "provider/model".into(),
            effort: "default".into(),
        },
    }];
    bench.host.state.settings.save_map(&map).unwrap();
    assert_eq!(poll(&bench).await["started"], 1);
    let run = runs(&bench).remove(0);
    assert_eq!(run.state, Phase::Planning);
    assert_eq!(run.workflow.as_deref(), Some("payments"));
    assert_eq!(bench.count("directions"), 0);
}

#[tokio::test]
async fn the_ledger_records_the_item_its_state_and_its_changed_date() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    poll(&bench).await;
    let rows = bench.host.state.workflows.watcher.rows(&url(HU));
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].0.as_str(), rows[0].1.as_ref()), ("Ready", None));
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
    // Two workflows start by themselves (so the director chooses), one only by hand.
    second_and_manual(&bench);

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
async fn several_workflows_that_start_by_themselves_wait_for_a_director() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    second_and_manual(&bench);
    // They differ in agent, so they lend no shared one.
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflows[1].agent.effort = "high".into();
    bench.host.state.settings.save_map(&map).unwrap();
    // No director: nothing is read or started, and the view says what it waits for.
    let waiting = poll(&bench).await;
    assert_eq!(waiting["started"], 0);
    assert_eq!(waiting["projects"][0]["waiting_for_director"], true);
    assert_eq!(bench.reads("list "), 0);
    assert!(runs(&bench).is_empty());
    let shown = view(&bench.host.state);
    assert_eq!(shown["state"], "waiting_for_director");
    assert_eq!(shown["waiting_projects"], json!(["Payments"]));
    // Once a director is configured it starts, and the director chooses.
    bench.director();
    assert_eq!(view(&bench.host.state)["state"], "watching");
    assert_eq!(poll(&bench).await["started"], 1);
    assert_eq!(runs(&bench).len(), 1);
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

#[tokio::test]
async fn items_past_the_start_budget_stay_in_the_ledger_and_never_get_a_second_run() {
    let bench = Bench::new().await;
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    let extra = |range: std::ops::Range<u64>| {
        for id in range {
            bench.catalog.add(
                super::super::synthetic::detail(id, "Task", "task", "Extra", "Text."),
                true,
            );
        }
    };
    extra(300..330);
    // 34 items: the highest IDs get the 25 runs, the others wait.
    assert_eq!(poll(&bench).await["started"], MAX_STARTS);
    let watcher = &bench.host.state.workflows.watcher;
    assert_eq!(watcher.seen(), MAX_STARTS);
    // Newer items come first in the list: they use the whole budget before the recorded ones
    // are reached, which must still count as present.
    extra(400..430);
    let second = poll(&bench).await;
    assert_eq!(second["started"], MAX_STARTS);
    assert_eq!(watcher.seen(), 2 * MAX_STARTS);
    // The rest follow, and no item has two runs.
    poll(&bench).await;
    poll(&bench).await;
    let all = runs(&bench);
    assert_eq!(all.len(), 64);
    let items: std::collections::BTreeSet<u64> = all.iter().map(|r| r.item.id).collect();
    assert_eq!(items.len(), 64);
}

#[tokio::test]
async fn a_mark_without_its_run_is_asked_for_again_after_one_interval_under_the_same_run_id() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    let watcher = &bench.host.state.workflows.watcher;
    // An abort between the mark and the start: a row, no run.
    let item = bench
        .catalog
        .ready_items(None, "Payments", "Ready", None, 10)
        .unwrap()
        .items
        .remove(0);
    let planned = uuid::Uuid::new_v4().to_string();
    watcher
        .mark(&item, "example", &planned, bench.now())
        .unwrap();
    // Within the interval it may still be in flight: nothing starts.
    assert_eq!(poll(&bench).await["started"], 0);
    assert!(runs(&bench).is_empty());
    // After the interval it is not seen: the run is asked for under the recorded ID.
    watcher.age(&url(HU), 301);
    assert_eq!(poll(&bench).await["started"], 1);
    let all = runs(&bench);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, planned);
    assert_eq!(watcher.rows(&url(HU))[0].2, planned);
    // The run exists now: the next poll starts nothing.
    assert_eq!(poll(&bench).await["started"], 0);
    assert_eq!(runs(&bench).len(), 1);
}

#[tokio::test]
async fn a_run_started_by_hand_is_the_items_run_in_the_ledger() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    let by_hand = bench.start(HU).await;
    let shown = poll(&bench).await;
    assert_eq!(shown["started"], 0);
    assert_eq!(runs(&bench).len(), 1);
    let rows = bench.host.state.workflows.watcher.rows(&url(HU));
    assert_eq!(rows[0].2, by_hand);
}

#[tokio::test]
async fn a_failed_read_of_one_ready_state_does_not_skip_the_others_or_forget_its_rows() {
    let bench = Bench::new().await;
    only_the_hu(&bench);
    set_ready(&bench, &["Ready", "Approved"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    let watcher = &bench.host.state.workflows.watcher;
    assert_eq!(poll(&bench).await["started"], 1);
    // The ready state fails; the other still starts its item and the failed one is reported.
    bench.catalog.set_state(200, "Approved");
    *bench.catalog.failing_state.lock().unwrap() = Some("Ready".into());
    let partial = poll(&bench).await;
    assert_eq!(partial["started"], 1);
    assert_eq!(partial["partial"], true);
    assert_eq!(partial["projects"][0]["complete"], false);
    assert!(
        partial["projects"][0]["errors"][0]
            .as_str()
            .unwrap()
            .starts_with("Ready")
    );
    assert_eq!(watcher.rows(&url(HU)).len(), 1);
    assert_eq!(watcher.rows(&url(200)).len(), 1);
    assert_eq!(runs(&bench).len(), 2);
}

#[tokio::test]
async fn each_ready_state_entry_is_read_through_the_source_that_names_its_organization() {
    use crate::knowledge::{Access, Resource};
    use crate::tools::{CatalogSource, ToolSpec};
    let bench = Bench::new().await;
    only_the_hu(&bench);
    let source = |id: &str, organization: &str| Resource {
        id: id.into(),
        description: "Azure DevOps".into(),
        topics: Vec::new(),
        enabled: true,
        external_processing: false,
        allowed_conversations: Vec::new(),
        allowed_senders: Vec::new(),
        access: Access::Tool {
            tool: ToolSpec::AzureDevopsStatus {
                repository: String::new(),
                path: PathBuf::new(),
                catalog: vec![CatalogSource {
                    organization: organization.into(),
                    projects: vec!["Payments".into()],
                    author_email: "ana@example.com".into(),
                }],
                secret_ref: "secret://AZURE_READ".into(),
                link_secret_ref: None,
            },
        },
    };
    let mut map = bench.host.state.settings.map().unwrap();
    map.resources
        .push(source("azure-other", "https://dev.azure.com/other"));
    map.resources.push(source(
        "azure-example",
        super::super::synthetic::ORGANIZATION,
    ));
    bench.host.state.settings.save_map(&map).unwrap();
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    assert_eq!(poll(&bench).await["started"], 1);
    assert_eq!(
        bench.reads("list Payments Ready limit 100 source azure-example"),
        1
    );
    assert_eq!(
        runs(&bench)[0].item.source.as_deref(),
        Some("azure-example")
    );
    // An organization no enabled source names is reported, and nothing is read for it.
    let state = &bench.host.state;
    assert_eq!(
        tools::todo::source_for(state, "https://dev.azure.com/other").unwrap(),
        Some("azure-other".to_owned())
    );
    let unknown = tools::todo::source_for(state, "https://dev.azure.com/nobody").unwrap_err();
    assert!(unknown.starts_with("[invalid_input]"), "{unknown}");
    // The same organization in two enabled sources is ambiguous.
    let mut map = state.settings.map().unwrap();
    map.resources
        .push(source("azure-twin", "https://dev.azure.com/other"));
    state.settings.save_map(&map).unwrap();
    let twice = tools::todo::source_for(state, "https://dev.azure.com/other").unwrap_err();
    assert!(twice.contains("Several"), "{twice}");
    // With one source enabled, no source is passed.
    let mut map = state.settings.map().unwrap();
    map.resources
        .retain(|r| r.id != "azure-other" && r.id != "azure-twin");
    state.settings.save_map(&map).unwrap();
    assert_eq!(
        tools::todo::source_for(state, "https://dev.azure.com/other").unwrap(),
        None
    );
}

#[tokio::test]
async fn one_poll_starts_a_run_for_every_ready_item_whatever_its_type_and_each_only_once() {
    let bench = Bench::new().await;
    set_ready(&bench, &["Ready"]);
    edit_workflow(&bench, |w| w.auto_start = true);
    answer(&bench, "payments");
    // An HU, a bug, a development bug listed with the bugs by its guide and a plain Task.
    for id in [101, 102] {
        bench.catalog.set_state(id, "New");
    }
    bench.catalog.add(
        super::super::synthetic::detail(500, "Bug", "bug", "Rounding", "Text."),
        true,
    );
    bench.catalog.add(
        super::super::synthetic::detail(501, "Development Bug", "task", "Totals", "Text."),
        true,
    );
    bench
        .catalog
        .listed_with_bugs
        .lock()
        .unwrap()
        .insert("Development Bug".into());
    let first = poll(&bench).await;
    assert_eq!(first["started"], 4, "{first}");
    let project = &first["projects"][0];
    assert_eq!(project["seen"], 4);
    assert_eq!(
        project["seen_by_category"],
        json!({"requirement": 1, "bug": 1, "task": 2})
    );
    assert_eq!(
        first["seen_by_category"],
        json!({"requirement": 1, "bug": 1, "task": 2})
    );
    assert_eq!(project["complete"], true);
    let items: std::collections::BTreeSet<u64> = runs(&bench).iter().map(|r| r.item.id).collect();
    assert_eq!(items, [HU, TASK, 500, 501].into());
    // Both reads of the state are the To-Do read's own, within the project's bound.
    assert_eq!(bench.reads("list Payments Ready limit 100"), 1);
    assert_eq!(bench.reads("list Payments Ready kind task limit 97"), 1);

    // Every type is seen once: the next poll starts nothing.
    let again = poll(&bench).await;
    assert_eq!(again["started"], 0);
    assert_eq!(again["projects"][0]["already_seen"], 4);
    assert_eq!(runs(&bench).len(), 4);
    assert_eq!(bench.host.state.workflows.watcher.seen(), 4);

    // `workflows settings` shows the last poll's counts by category.
    let shown = super::super::director::operation(&bench.host, "workflow_settings", &json!({}))
        .await
        .unwrap();
    assert_eq!(
        shown["watcher"]["last_poll"]["seen_by_category"],
        json!({"requirement": 1, "bug": 1, "task": 2})
    );
    assert!(bench.catalog.only_reads());
}
