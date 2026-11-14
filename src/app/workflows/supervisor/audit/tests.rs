//! The audit's batch (a pure function of the runs), the merged branches code lists and what code
//! keeps of the supervisor's answer.
use super::*;
use crate::ado::workflow::PullRequest;
use crate::workflows::Repository;

const API: &str = "0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f";

fn run(id: u64, at: i64, state: Phase) -> Run {
    let mut run = Run::new(
        record::Item {
            provider: "azure".into(),
            id,
            project: "Payments".into(),
            organization: "https://dev.azure.com/example".into(),
            url: format!("https://dev.azure.com/example/Payments/_workitems/edit/{id}"),
            title: format!("Item {id}"),
            kind: "Task".into(),
            category: "task".into(),
            state: "Ready".into(),
            source: None,
        },
        at,
    );
    run.definition = Some(crate::workflows::Workflow {
        id: "payments".into(),
        repositories: vec![Repository {
            id: API.into(),
            name: "payments-api".into(),
        }],
        ..Default::default()
    });
    run.workflow = Some("payments".into());
    run.advance(Phase::Coding, at + 1, "assigned");
    match state {
        Phase::Done => run.advance(Phase::Done, at + 2, "done"),
        Phase::Coding => {}
        other => run.stop(other, at + 2, "Stopped.".into()),
    }
    run
}

#[test]
fn a_batch_ends_when_every_run_since_the_last_audit_has_left_the_engines_stages() {
    let now = 1_800_000_000;
    let mut runs = vec![
        run(1, now - 500, Phase::Done),
        run(2, now - 400, Phase::NeedsAttention),
        run(3, now - 300, Phase::Coding),
    ];
    // One still codes: no batch.
    assert_eq!(batch(&runs, None, now), None);
    runs[2] = run(3, now - 300, Phase::Done);
    let ended = batch(&runs, None, now).unwrap();
    assert_eq!(ended.runs.len(), 3);
    assert_eq!(ended.until, now - 300);
    // The same runs make the same key, so the batch is audited once.
    assert_eq!(batch(&runs, None, now + 30).unwrap().key, ended.key);
    // After the audit, only runs created later make the next batch.
    assert_eq!(batch(&runs, Some(ended.until), now), None);
    runs.push(run(4, now - 100, Phase::Done));
    let next = batch(&runs, Some(ended.until), now).unwrap();
    assert_eq!(next.runs, [runs[3].id.clone()]);
    // A batch where nothing reached coding is not audited.
    let mut unassigned = Run::new(runs[0].item.clone(), now - 50);
    unassigned.advance(Phase::Unassigned, now - 49, "none");
    assert_eq!(batch(&[unassigned], Some(now - 60), now), None);
}

#[test]
fn only_completed_pull_requests_of_reported_branches_name_a_merged_branch() {
    let mut done = run(5, 1_800_000_000, Phase::Done);
    let repository = Repository {
        id: API.into(),
        name: "payments-api".into(),
    };
    let pull = |id: u64, status: &str| PullRequest {
        id,
        repository: API.into(),
        status: status.into(),
        ..Default::default()
    };
    done.children = vec![
        record::Child {
            id: 5,
            repository: Some(repository.clone()),
            branch: Some("feature/5".into()),
            pull_requests: vec![pull(7, "completed")],
            ..Default::default()
        },
        record::Child {
            id: 6,
            repository: Some(repository.clone()),
            branch: Some("feature/6".into()),
            pull_requests: vec![pull(8, "active")],
            ..Default::default()
        },
        record::Child {
            id: 7,
            repository: Some(repository),
            branch: Some("main".into()),
            pull_requests: vec![pull(9, "completed")],
            ..Default::default()
        },
    ];
    let found = completed_branches(&done);
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].1.as_str(), found[0].2), ("feature/5", 7));
}

#[test]
fn the_audit_keeps_only_cleaned_text_about_the_runs_of_its_batch() {
    let runs = vec![run(1, 1_800_000_000, Phase::Done)];
    let mut audit = Audit {
        items: runs.iter().map(item_of).collect(),
        ..Default::default()
    };
    let answer = json!({
        "overview": "Went well. See https://evil.example/x and @[[Mallory]]",
        "items": [{"run": runs[0].id, "result": "Merged."}, {"run": "another", "result": "x"}],
        "findings": [
            {"run": runs[0].id, "text": "The export rounds too <<b>a href=x>here</a>", "from": "comment", "needs_item": true},
            {"run": "another", "text": "x", "from": "comment", "needs_item": false},
            {"run": runs[0].id, "text": "y", "from": "email", "needs_item": false}
        ],
        "proposals": ["Delete merged branches."],
        "work_items": [
            {"run": runs[0].id, "type": "bug", "title": "Round the export", "description": "In the API.", "repository": "payments-api", "reason": "A comment."},
            {"run": runs[0].id, "type": "bug", "title": "Elsewhere", "description": "x", "repository": "billing", "reason": "x"}
        ]
    });
    judge(answer.as_object().unwrap(), &runs, &mut audit);
    assert!(
        !audit.overview.contains("evil.example") && !audit.overview.contains("@["),
        "{}",
        audit.overview
    );
    assert_eq!(audit.items[0].result, "Merged.");
    assert_eq!(audit.findings.len(), 1);
    assert!(audit.findings[0].needs_item);
    assert!(
        !audit.findings[0].text.contains('<'),
        "{}",
        audit.findings[0].text
    );
    assert_eq!(audit.work_items.len(), 1);
    assert_eq!(
        audit.work_items[0].draft.repository.as_ref().unwrap().name,
        "payments-api"
    );
    assert_eq!(audit.dropped, 4);
}
