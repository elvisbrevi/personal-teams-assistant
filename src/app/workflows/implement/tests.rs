//! The Implement form's workflow choice over the engine's synthetic catalog and fake provider
//! CLI: a workflow ID starts a workflow run, `director` classifies and starts nothing, and
//! `none` is the plain implementation run, unchanged.
use super::super::record::State as Phase;
use super::super::runs::tests::{Bench, HU};
use super::*;
use std::fs;

fn request(workflow: &str) -> Value {
    json!({"item":{"provider":"azure","id":HU,"project":"Payments"},"workflow":workflow})
}
fn runs(bench: &Bench) -> usize {
    bench
        .host
        .state
        .workflows
        .db
        .list(&[], None, -1)
        .unwrap()
        .len()
}

#[tokio::test]
async fn a_workflow_id_starts_a_workflow_run_with_the_workflows_own_machine_checkouts_and_agent() {
    let bench = Bench::new().await;
    let reply = start(&bench.host, &request("payments"), &bench.azure)
        .await
        .unwrap();
    assert_eq!(reply["started"], true);
    assert_eq!(reply["workflow"], "payments");
    let id = reply["run"]["id"].as_str().unwrap().to_owned();
    let run = bench.run(&id);
    assert_eq!(run.state, Phase::Planning);
    assert!(!run.auto_start);
    let definition = run.definition.as_ref().unwrap();
    assert_eq!(definition.machine, bench.machine);
    assert_eq!(definition.agent.provider, "opencode");
    // The engine plans it on the workflow's machine and agent, with its checkouts.
    bench.state(&id, Phase::PlanReview).await;
    assert_eq!(bench.count("plans"), 1);
    assert_eq!(bench.count("starts"), 0);
    // One run per item.
    let again = start(&bench.host, &request("payments"), &bench.azure)
        .await
        .unwrap_err();
    assert!(again.starts_with("[conflict]"), "{again}");
    assert_eq!(runs(&bench), 1);
}

#[tokio::test]
async fn a_workflow_run_takes_its_place_and_agent_from_the_workflow_not_from_the_request() {
    let bench = Bench::new().await;
    for (field, value) in [
        ("machine", json!(bench.machine)),
        ("directories", json!(["/tmp"])),
        ("provider", json!("codex")),
        ("model", json!("m")),
        ("effort", json!("high")),
        ("context", json!("Do it differently.")),
    ] {
        let mut args = request("payments");
        args[field] = value;
        let refused = start(&bench.host, &args, &bench.azure).await.unwrap_err();
        assert!(refused.contains("takes its machine"), "{field}: {refused}");
    }
    // Empty values are as good as none.
    let mut args = request("payments");
    args["context"] = json!("");
    args["directories"] = json!([]);
    assert!(start(&bench.host, &args, &bench.azure).await.is_ok());
    // A GitHub item is not a workflow item.
    let mut github = request("payments");
    github["item"]["provider"] = json!("github");
    assert!(
        start(&bench.host, &github, &bench.azure)
            .await
            .unwrap_err()
            .contains("Azure DevOps")
    );
}

#[tokio::test]
async fn the_director_choice_is_returned_for_confirmation_and_nothing_starts() {
    let bench = Bench::new().await;
    // No director: nothing to ask.
    let missing = start(&bench.host, &request("director"), &bench.azure)
        .await
        .unwrap_err();
    assert!(missing.starts_with("[not_ready]"), "{missing}");
    bench.director();
    let choice = start(&bench.host, &request("director"), &bench.azure)
        .await
        .unwrap();
    assert_eq!(choice["started"], false);
    assert_eq!(choice["workflow"], "payments");
    assert_eq!(choice["outcome"], "chosen");
    assert_eq!(choice["reason"], "Habla de pagos.");
    assert_eq!(choice["item"]["id"], HU);
    assert!(
        choice["next"]
            .as_str()
            .unwrap()
            .contains("Nothing was started")
    );
    assert_eq!(runs(&bench), 0);
    assert_eq!(bench.count("directions"), 1);
    assert_eq!(bench.count("plans") + bench.count("starts"), 0);

    // The director chooses none: the choice says so, and still nothing starts.
    let line = json!({"type":"text","part":{"text":json!({"workflow":"","reason":"Ninguno encaja."}).to_string()}});
    fs::write(bench.log.join("director.jsonl"), format!("{line}\n")).unwrap();
    let none = start(&bench.host, &request("director"), &bench.azure)
        .await
        .unwrap();
    assert!(none["workflow"].is_null());
    assert_eq!(none["reason"], "Ninguno encaja.");
    assert_eq!(runs(&bench), 0);

    // Confirming is a second call naming the workflow.
    let started = start(&bench.host, &request("payments"), &bench.azure)
        .await
        .unwrap();
    assert_eq!(started["started"], true);
    assert_eq!(runs(&bench), 1);
}

#[tokio::test]
async fn none_is_the_plain_implementation_run_unchanged() {
    let bench = Bench::new().await;
    assert_eq!(choice(&request("none")), None);
    assert_eq!(choice(&json!({"item":{}})), None);
    assert_eq!(choice(&request("director")), Some("director"));
    assert_eq!(choice(&request("payments")), Some("payments"));
    // `workflow: none` is dropped before the plain operation reads its request, which refuses
    // unknown fields: the answer is the one the request without it gets.
    let plain_args = json!({"machine":"not-an-id","item":{"provider":"azure","id":HU,"project":"Payments"},
        "context":"","directories":[],"provider":"codex","model":"m","effort":"e"});
    let mut with_none = plain_args.clone();
    with_none["workflow"] = json!("none");
    assert_eq!(super::plain(&with_none), plain_args);
    let without = implementation::operate(&bench.host, "implementation_start", &plain_args).await;
    let with = implementation::operate(&bench.host, "implementation_start", &with_none).await;
    assert!(without.is_err());
    assert_eq!(with, without);
    assert_eq!(runs(&bench), 0);
}
