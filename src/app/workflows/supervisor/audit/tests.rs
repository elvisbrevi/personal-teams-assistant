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
    // After an audit that answered, which carried the run that had not finished, nothing new
    // makes no batch; a new run makes the next one, with the carried run in it.
    let last = Last {
        until: ended.until,
        carried: vec![runs[1].id.clone()],
    };
    assert_eq!(batch(&runs, Some(&last), now), None);
    runs.push(run(4, now - 100, Phase::Done));
    let next = batch(&runs, Some(&last), now).unwrap();
    assert_eq!(next.runs, [runs[1].id.clone(), runs[3].id.clone()]);
    // A carried run that is coding again holds the batch until it stops.
    runs[1] = {
        let mut resumed = runs[1].clone();
        resumed.state = Phase::Coding;
        resumed
    };
    assert_eq!(batch(&runs, Some(&last), now), None);
    // A batch where nothing reached coding is not audited.
    let mut unassigned = Run::new(runs[0].item.clone(), now - 50);
    unassigned.advance(Phase::Unassigned, now - 49, "none");
    let quiet = Last {
        until: now - 60,
        carried: Vec::new(),
    };
    assert_eq!(batch(&[unassigned], Some(&quiet), now), None);
}

#[test]
fn an_audit_that_failed_consumes_no_batch_and_a_cut_input_says_so() {
    let root = tempfile::tempdir().unwrap();
    let looks = Looks::open(&root.path().join("workflows.db")).unwrap();
    let audit = |id: &str, outcome: &str, until: i64, at: i64| Audit {
        id: id.into(),
        outcome: outcome.into(),
        until,
        created_at: at,
        carried: vec!["carried-run".into()],
        ..Default::default()
    };
    looks.insert_audit(&audit("a1", "done", 100, 1)).unwrap();
    looks.insert_audit(&audit("a2", "failed", 200, 2)).unwrap();
    let last = last(&looks).unwrap().unwrap();
    assert_eq!(
        last.until, 100,
        "the failed audit's window is audited again"
    );
    assert_eq!(last.carried, ["carried-run"]);
    // An input past its bound even without comments is cut, and says so.
    let runs = vec![run(1, 1_800_000_000, Phase::Done)];
    let read = AuditRead {
        merged: (0..2000)
            .map(|n| MergedBranch {
                run: runs[0].id.clone(),
                repository: "payments-api".into(),
                branch: format!("feature/{n}-{}", "b".repeat(200)),
                pull_request: n,
            })
            .collect(),
        ..Default::default()
    };
    let (text, truncated) = input(&runs, &read, crate::config::Language::En, 1_800_000_100);
    assert!(text.chars().count() <= AUDIT_INPUT);
    assert!(
        truncated.iter().any(|t| t == "the end of the input"),
        "{truncated:?}"
    );
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
