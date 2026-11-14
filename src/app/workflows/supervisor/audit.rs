//! The supervisor's audit of a batch of workflow runs: when every run created since the last
//! audit has ended (`batch`), or on request, code lists what it can state itself (each run's
//! item, state, children and pull requests, the checks of its reports, the branches of
//! completed pull requests that still exist, the looks that failed) and the supervisor adds,
//! from the comments code read, the findings left only in comments and its proposals, in the
//! same read-only, tool-less session as a look (`DirectTask::Audit`). Audits are kept in
//! `workflow_audits`; nothing is posted or written to a provider.
use super::*;

/// At most this many audits are kept; the oldest is dropped.
pub(in crate::app) const MAX_AUDITS: usize = 50;
/// At most this many runs are in one audit (the most recent).
pub(in crate::app) const MAX_BATCH: usize = 50;
/// Without an earlier audit, a batch is the runs of the last week.
const FIRST_BATCH_SECONDS: i64 = 7 * DAY;
/// The most characters of an audit's input.
pub(in crate::app) const AUDIT_INPUT: usize = 120_000;
/// Comments of one item an audit reads, each at most this many characters.
const AUDIT_COMMENTS: usize = 10;
const AUDIT_COMMENT_CHARS: usize = 1000;
const MAX_FINDINGS: usize = 30;
const MAX_PROPOSALS: usize = 10;
const MAX_AUDIT_DRAFTS: usize = 5;
/// The `run` of the rows that ask for an audit.
pub(in crate::app) const AUDIT_ROW: &str = "audit";
/// Branches never listed as merged but not deleted: a repository's usual default branches.
const DEFAULT_BRANCHES: [&str; 2] = ["main", "master"];

/// One run as the audit lists it, from its record (and the supervisor's line about it).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Item {
    pub run: String,
    pub item: u64,
    pub title: String,
    /// The work item's verified link.
    pub url: String,
    /// Its project and organization, for a drafted work item's Create request.
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    pub state: String,
    pub children: usize,
    /// Each pull request of its children, with its status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_requests: Vec<Value>,
    /// Checks of its latest look's report claims that were not verified.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub not_verified: usize,
    /// The supervisor's line about it, cleaned.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub result: String,
}
fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

/// A branch whose pull request was completed and that still exists in its repository.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct MergedBranch {
    pub run: String,
    pub repository: String,
    pub branch: String,
    pub pull_request: u64,
}

/// A finding the supervisor read only in comments, reports or sessions.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Finding {
    pub run: String,
    pub text: String,
    /// `comment`, `report` or `session`.
    pub from: String,
    pub needs_item: bool,
}

/// A drafted work item of the audit, for the run it is about.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct AuditDraft {
    pub run: String,
    pub draft: Draft,
}

/// One audit, as stored in `workflow_audits`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Audit {
    /// Its session's record in Agent history.
    pub id: String,
    /// `batch_ended` or `requested`.
    pub event: String,
    pub created_at: i64,
    /// The runs it covers were created after `since` and up to `until` (Unix seconds).
    pub since: i64,
    pub until: i64,
    pub supervised_by: Director,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// `done` or `failed`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncated: Vec<String>,
    pub items: Vec<Item>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_branches: Vec<MergedBranch>,
    /// Runs whose latest look failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_looks: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub overview: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work_items: Vec<AuditDraft>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u32,
}
impl Audit {
    /// The audit in a list: its identity, when, what it covers and how it went.
    pub(in crate::app) fn summary(&self) -> Value {
        json!({
            "id": self.id, "event": self.event, "created_at": self.created_at,
            "since": self.since, "until": self.until, "outcome": self.outcome,
            "runs": self.items.len(), "findings": self.findings.len(),
            "merged_branches": self.merged_branches.len(),
        })
    }
}

/// The stages a run still goes through by itself: a batch has ended when none of its runs is in
/// one (a run waiting for a slot is in one too).
fn active(state: Phase) -> bool {
    matches!(
        state,
        Phase::Classifying
            | Phase::Planning
            | Phase::CreatingChildren
            | Phase::Coding
            | Phase::PullRequests
            | Phase::Blocks
    )
}

/// A batch of runs to audit: its key, the window it covers and its runs.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct Batch {
    pub key: String,
    pub since: i64,
    pub until: i64,
    pub runs: Vec<String>,
}

/// The runs created after `since` (the last audit's `until`, or a week ago), at most
/// `MAX_BATCH`, the most recent.
fn window(runs: &[Run], since: i64) -> Vec<&Run> {
    let mut after: Vec<&Run> = runs.iter().filter(|r| r.created_at > since).collect();
    after.sort_by_key(|r| r.created_at);
    let skip = after.len().saturating_sub(MAX_BATCH);
    after.split_off(skip)
}

/// The batch that has ended now, if any (a pure function of the runs): every run created since
/// the last audit has left the stages the engine drives, and one of them reached coding.
pub(in crate::app) fn batch(runs: &[Run], last_until: Option<i64>, now: i64) -> Option<Batch> {
    let since = last_until.unwrap_or(now - FIRST_BATCH_SECONDS);
    let runs = window(runs, since);
    let coded = runs
        .iter()
        .any(|r| r.timeline.iter().any(|s| s.stage == Phase::Coding));
    if runs.is_empty() || !coded || runs.iter().any(|r| active(r.state)) {
        return None;
    }
    let newest = runs.last()?;
    Some(Batch {
        key: format!(
            "{AUDIT_ROW}:batch_ended:{}:{}",
            newest.created_at, newest.id
        ),
        since,
        until: newest.created_at,
        runs: runs.iter().map(|r| r.id.clone()).collect(),
    })
}

/// When the last audit's window ends, from the stored audits.
pub(in crate::app) fn last_until(looks: &Looks) -> Result<Option<i64>> {
    Ok(looks.audits(1)?.first().map(|a| a.until))
}

/// The runs a requested audit covers: those created after `since`, or after the last audit.
pub(in crate::app) fn requested_runs(runs: &[Run], since: i64) -> Batch {
    let runs = window(runs, since);
    Batch {
        key: String::new(),
        since,
        until: runs.last().map_or(since, |r| r.created_at),
        runs: runs.iter().map(|r| r.id.clone()).collect(),
    }
}

/// What code states about one run from its record.
fn item_of(run: &Run) -> Item {
    let latest = run.supervision.last();
    Item {
        run: run.id.clone(),
        item: run.item.id,
        title: cut(&run.item.title, TITLE_CHARS),
        url: run.item.url.clone(),
        project: run.item.project.clone(),
        organization: run.item.organization.clone(),
        workflow: run.workflow.clone(),
        state: run.state.word().into(),
        children: run.children.len(),
        pull_requests: run
            .children
            .iter()
            .flat_map(|c| &c.pull_requests)
            .map(|p| json!({"id": p.id, "status": p.status, "url": p.url}))
            .collect(),
        not_verified: latest.map_or(0, |l| {
            l.checks
                .iter()
                .filter(|c| c.result == "not_verified")
                .count()
        }),
        result: String::new(),
    }
}

/// What an audit reads beyond the records: the merged branches that still exist and each
/// item's comments since its run started.
#[derive(Default)]
pub(in crate::app) struct AuditRead {
    pub merged: Vec<MergedBranch>,
    /// Comments by run.
    pub comments: Vec<(String, Vec<Value>)>,
    pub unread: Vec<String>,
}

/// The branches whose pull request was completed, by run: a child reports one branch per
/// repository, so a completed pull request of that repository is that branch's.
pub(in crate::app) fn completed_branches(
    run: &Run,
) -> Vec<(crate::workflows::Repository, String, u64)> {
    let mut found = Vec::new();
    for child in &run.children {
        for (repository, branch) in child.delivered() {
            if DEFAULT_BRANCHES.contains(&branch.as_str()) {
                continue;
            }
            if let Some(pull) = child.pull_requests.iter().find(|p| {
                p.status == "completed" && p.repository.eq_ignore_ascii_case(&repository.id)
            }) {
                found.push((repository, branch, pull.id));
            }
        }
    }
    found
}

async fn gather(host: &Arc<Host>, azure: &Azure, runs: &[Run]) -> AuditRead {
    let state = &host.state;
    let mut read = AuditRead::default();
    let work = async {
        for run in runs {
            for (repository, branch, pull) in completed_branches(run) {
                match azure
                    .branch_exists(state, run, &repository.id, &branch)
                    .await
                {
                    Ok(true) => read.merged.push(MergedBranch {
                        run: run.id.clone(),
                        repository: repository.name.clone(),
                        branch,
                        pull_request: pull,
                    }),
                    Ok(false) => {}
                    Err(_) => read
                        .unread
                        .push(format!("branch {branch} of run {}", run.id)),
                }
            }
            match azure
                .item(
                    state,
                    run.item.source.as_deref(),
                    Some(&run.item.project),
                    run.item.id,
                )
                .await
            {
                Ok(detail) => {
                    let since = run.created_at;
                    let comments: Vec<Value> = detail
                        .comments
                        .iter()
                        .filter(|c| {
                            c.created_at
                                .as_deref()
                                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                                .is_none_or(|t| t.timestamp() >= since)
                        })
                        .rev()
                        .take(AUDIT_COMMENTS)
                        .map(|c| json!({"author": cut(&c.author, 100), "at": c.created_at, "text": cut(&c.text, AUDIT_COMMENT_CHARS)}))
                        .collect();
                    read.comments.push((run.id.clone(), comments));
                }
                Err(_) => read.unread.push(format!("the work item of run {}", run.id)),
            }
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(READ_SECONDS), work)
        .await
        .is_err()
    {
        read.unread
            .push("the rest of the reads did not answer in time".into());
    }
    read
}

/// The audit's input: what code states under `facts`, the titles and comments under
/// `untrusted`, at most `AUDIT_INPUT` characters (the oldest comments cut first).
pub(in crate::app) fn input(
    runs: &[Run],
    read: &AuditRead,
    language: crate::config::Language,
    now: i64,
) -> (String, Vec<String>) {
    let mut comments: Vec<(String, Vec<Value>)> = read.comments.clone();
    let mut truncated = Vec::new();
    loop {
        let value = json!({
            "language": language,
            "generated_at": time(now),
            "facts": {
                "runs": runs.iter().map(|r| {
                    let item = item_of(r);
                    json!({
                        "run": r.id, "item": r.item.id, "workflow": r.workflow, "state": item.state,
                        "children": item.children, "pull_requests": item.pull_requests,
                        "not_verified_claims": item.not_verified,
                        "latest_look": r.supervision.last().map(|l| l.outcome.clone()),
                        "created_at": time(r.created_at), "finished_at": r.finished_at.and_then(time),
                    })
                }).collect::<Vec<_>>(),
                "merged_branches_not_deleted": read.merged,
            },
            "untrusted": {
                "titles": runs.iter().map(|r| json!({"run": r.id, "title": cut(&r.item.title, TITLE_CHARS)})).collect::<Vec<_>>(),
                "comments": comments.iter().map(|(run, c)| json!({"run": run, "comments": c})).collect::<Vec<_>>(),
            },
            "unread": read.unread,
            "truncated": truncated,
        });
        let text = value.to_string();
        if text.chars().count() <= AUDIT_INPUT {
            return (text, truncated);
        }
        if let Some((_, list)) = comments.iter_mut().find(|(_, c)| !c.is_empty()) {
            list.pop();
            if !truncated.iter().any(|t| t == "the oldest comments") {
                truncated.push("the oldest comments".into());
            }
        } else {
            return (cut(&text, AUDIT_INPUT), truncated);
        }
    }
}

/// The audit's answer, exactly the closed schema (five properties, nothing else).
fn shaped(answer: Option<&Value>) -> Option<&serde_json::Map<String, Value>> {
    let object = answer?.as_object()?;
    let keys = ["overview", "items", "findings", "proposals", "work_items"];
    (object.len() == keys.len() && keys.iter().all(|k| object.contains_key(*k))).then_some(object)
}

/// What code keeps of the answer: cleaned, cut, and only about the runs of the batch.
pub(in crate::app) fn judge(
    answer: &serde_json::Map<String, Value>,
    runs: &[Run],
    audit: &mut Audit,
) {
    let known = |id: &str| runs.iter().find(|r| r.id == id.trim());
    audit.overview = clean(&answer["overview"], PART_CHARS);
    for entry in answer["items"].as_array().into_iter().flatten() {
        let run = entry["run"].as_str().unwrap_or_default().trim();
        match audit.items.iter_mut().find(|i| i.run == run) {
            Some(item) if item.result.is_empty() => {
                item.result = one_line(&entry["result"], LINE_CHARS);
            }
            _ => audit.dropped += 1,
        }
    }
    for entry in answer["findings"].as_array().into_iter().flatten() {
        let run = entry["run"].as_str().unwrap_or_default();
        let from = entry["from"].as_str().unwrap_or_default();
        let text = clean(&entry["text"], 600);
        if audit.findings.len() >= MAX_FINDINGS
            || known(run).is_none()
            || !matches!(from, "comment" | "report" | "session")
            || text.is_empty()
        {
            audit.dropped += 1;
            continue;
        }
        audit.findings.push(Finding {
            run: run.trim().into(),
            text,
            from: from.into(),
            needs_item: entry["needs_item"] == true,
        });
    }
    audit.proposals = answer["proposals"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_PROPOSALS)
        .map(|p| one_line(p, LINE_CHARS))
        .filter(|p| !p.is_empty())
        .collect();
    for entry in answer["work_items"].as_array().into_iter().flatten() {
        let Some(run) = known(entry["run"].as_str().unwrap_or_default()) else {
            audit.dropped += 1;
            continue;
        };
        let kind = entry["type"].as_str().unwrap_or_default();
        let named = entry["repository"].as_str().unwrap_or_default().trim();
        let repositories = run
            .definition
            .as_ref()
            .map(|w| w.repositories.clone())
            .unwrap_or_default();
        let repository = repositories
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(named))
            .cloned();
        let title = one_line(&entry["title"], TITLE_CHARS);
        if audit.work_items.len() >= MAX_AUDIT_DRAFTS
            || !matches!(kind, "requirement" | "bug")
            || title.is_empty()
            || (!named.is_empty() && repository.is_none())
        {
            audit.dropped += 1;
            continue;
        }
        audit.work_items.push(AuditDraft {
            run: run.id.clone(),
            draft: Draft {
                kind: kind.into(),
                title,
                description: clean(&entry["description"], PART_CHARS),
                repository,
                reason: one_line(&entry["reason"], LINE_CHARS),
            },
        });
    }
}

/// Audit the runs of `row` (a batch that ended, or a request) and store it.
pub(in crate::app) async fn audit_at(
    host: &Arc<Host>,
    azure: &Azure,
    agent: &Director,
    settings: &Supervisor,
    row: &Row,
) -> Result<Audit> {
    let state = &host.state;
    let now = now();
    let all = {
        let mut all = state.workflows.db.list(&[], None, RUNS_LISTED)?;
        all.reverse();
        all
    };
    let wanted: Vec<String> = row.payload["runs"]
        .as_array()
        .map(|r| {
            r.iter()
                .filter_map(|i| i.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let since = row.payload["since"]
        .as_i64()
        .unwrap_or(now - FIRST_BATCH_SECONDS);
    let selected = if wanted.is_empty() {
        requested_runs(&all, since)
    } else {
        Batch {
            key: row.key.clone(),
            since,
            until: row.payload["until"].as_i64().unwrap_or(now),
            runs: wanted,
        }
    };
    let runs: Vec<Run> = all
        .into_iter()
        .filter(|r| selected.runs.contains(&r.id))
        .collect();
    let read = gather(host, azure, &runs).await;
    let language = state.settings.config()?.llm.language;
    let (input, truncated) = input(&runs, &read, language, now);
    let map = state.settings.map()?;
    let instructions =
        crate::agent_prompts::resolve(&map.agent_prompts, crate::agent_prompts::SUPERVISE);
    let mut audit = Audit {
        event: if row.requested {
            "requested"
        } else {
            "batch_ended"
        }
        .into(),
        created_at: now,
        since: selected.since,
        until: selected.until,
        supervised_by: agent.clone(),
        outcome: "failed".into(),
        unread: read.unread.clone(),
        truncated,
        items: runs.iter().map(item_of).collect(),
        merged_branches: read.merged.clone(),
        failed_looks: runs
            .iter()
            .filter(|r| r.supervision.last().is_some_and(|l| l.outcome == "failed"))
            .map(|r| r.id.clone())
            .collect(),
        ..Default::default()
    };
    // The record an audit's session leaves in Agent history names no one work item.
    let item = crate::tools::todo::Item {
        id: 0,
        title: format!("Audit of {} workflow runs", runs.len()),
        url: String::new(),
        project: String::new(),
        state: String::new(),
        kind: String::new(),
        category: String::new(),
        created_at: None,
        changed_at: None,
        dates: Vec::new(),
    };
    let answer = Box::pin(implementation::director::ask(
        host,
        implementation::director::Ask {
            machine: &agent.machine,
            task: DirectTask::Audit,
            director: agent,
            instructions: &instructions,
            input,
            item: &item,
            seconds: Some(u64::from(settings.session_seconds)),
        },
    ))
    .await;
    match answer {
        Ok(answer) => {
            audit.id = answer.run.clone();
            audit.session_id = answer.session_id.clone();
            match shaped(answer.direction.answer.as_ref()) {
                Some(object) => {
                    judge(object, &runs, &mut audit);
                    audit.outcome = "done".into();
                }
                None => {
                    let failure = answer
                        .direction
                        .failure
                        .unwrap_or(Failure::of(Cause::InvalidAnswer));
                    audit.failure = Some(failure);
                    let _ = implementation::director::annotate(
                        host,
                        &answer.run,
                        "state",
                        json!("failed"),
                    );
                    let _ = implementation::director::annotate(
                        host,
                        &answer.run,
                        "failure",
                        json!(failure),
                    );
                }
            }
        }
        Err(error) => {
            audit.failure = Some(crate::llm::implementation::of_error(&error, Cause::Unknown));
            audit.id = uuid::Uuid::new_v4().to_string();
        }
    }
    state.workflows.supervision.looks.insert_audit(&audit)?;
    Ok(audit)
}

#[cfg(test)]
mod tests;
