//! A workflow's size limits end to end: a definition at the limits reaches its coding run's
//! prompt whole, with the longest handover note, and a definition whose context would not fit
//! (one an older host saved) stops its run before anything starts, never cut.
use super::*;
use crate::workflows::{Block, MAX_CONTEXT, MAX_INSTRUCTIONS, MAX_WORKFLOW_BLOCKS};

/// `count` characters that start with `start` and end with `end`, plain ASCII in between.
fn text(start: &str, end: &str, count: usize) -> String {
    let filler = count - start.chars().count() - end.chars().count();
    let middle: String = "abcdefghij".chars().cycle().take(filler).collect();
    format!("{start}{middle}{end}")
}

/// The workflow at its limits: 20,000 characters of instructions and 20 instruction blocks of
/// `each` characters, stored as the operations would store them.
fn at_the_limits(bench: &Bench, each: usize) -> (Workflow, Vec<Block>) {
    let mut map = bench.host.state.settings.map().unwrap();
    let blocks: Vec<Block> = (0..MAX_WORKFLOW_BLOCKS)
        .map(|n| Block {
            id: format!("rule-{n}"),
            name: format!("Rule {n}"),
            text: text(
                &format!("Rule {n} starts. "),
                &format!(" Rule {n} ends."),
                each,
            ),
            ..Default::default()
        })
        .collect();
    let mut workflow = map.workflows[0].clone();
    workflow.instructions = text(
        "Instructions start. ",
        " Instructions end.",
        MAX_INSTRUCTIONS,
    );
    workflow.blocks = blocks.iter().map(|b| b.id.clone()).collect();
    map.workflow_blocks = blocks.clone();
    map.workflows = vec![workflow.clone()];
    bench.host.state.settings.save_map(&map).unwrap();
    (workflow, blocks)
}

#[tokio::test]
async fn a_workflow_at_its_limits_reaches_the_coding_prompt_whole_with_the_longest_handover() {
    let bench = Bench::new().await;
    let (workflow, blocks) = at_the_limits(&bench, 3000);
    // A definition a save accepts: its largest context fits.
    let largest = engine::largest_context(&workflow, &blocks);
    assert!(largest <= MAX_CONTEXT, "{largest}");

    bench.mode("mode", "fail");
    let old = bench.start(TASK).await;
    bench.state(&old, Phase::NeedsAttention).await;
    let note = text("Handover starts. ", " Handover ends.", record::MAX_HANDOVER);
    bench.mode("mode", "done");
    let reply = bench
        .op(
            "workflow_run_restart",
            json!({"run": old, "note": note, "confirmed": true}),
        )
        .await
        .unwrap();
    let new = reply["run"]["id"].as_str().unwrap().to_owned();
    bench.state(&new, Phase::PullRequests).await;

    let prompt = fs::read_to_string(bench.log.join("coding.2")).unwrap();
    assert!(prompt.contains(&workflow.instructions), "instructions cut");
    for block in &blocks {
        assert!(
            prompt.contains(&format!("Instruction block «{}»", block.name))
                && prompt.contains(&block.text),
            "block {} cut",
            block.id
        );
    }
    assert!(prompt.contains(&note), "handover note cut");
}

#[tokio::test]
async fn a_definition_whose_context_does_not_fit_stops_its_run_and_is_never_cut() {
    let bench = Bench::new().await;
    // Only a hand edit or an older host could store it: a save refuses it.
    let (workflow, blocks) = at_the_limits(&bench, crate::workflows::MAX_BLOCK_TEXT);
    let largest = engine::largest_context(&workflow, &blocks);
    assert!(largest > MAX_CONTEXT, "{largest}");
    let id = bench.start(TASK).await;
    let run = bench.state(&id, Phase::Failed).await;
    let error = run.error.clone().unwrap_or_default();
    assert!(
        error.contains("never cut") && error.contains(&MAX_CONTEXT.to_string()),
        "{error}"
    );
    assert_eq!(bench.count("starts"), 0);
}
