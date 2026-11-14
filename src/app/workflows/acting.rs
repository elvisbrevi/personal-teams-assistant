//! The acting blocks of a workflow run: the Teams notifications and the coordinator's launchers
//! the engine runs at their stage.
//!
//! **Stages.** An acting block names the stage at which it acts, and a stage occurs in a run
//! when its record says so (`occurrences`): `before_planning` before the planning run starts,
//! `plan_ready` when the run waits in `plan_review`, `children_created` once the creation ledger
//! confirmed the children, `coding_started` for each coding run its machine accepted (a task's
//! own, which has no planning stage, or each child's), `child_done` for each child whose coding
//! run ended done, then
//! `pull_requests_completed` and `done` in the `blocks` stage, and `failed` when the run fails.
//! The blocks of one occurrence run in the workflow's order.
//!
//! **Once.** A block runs at most once for each occurrence of its stage. The engine records the
//! attempt (`attempting`, durably) before it acts and the result after, so a restart never posts
//! or runs again: an attempt without a result is `uncertain` from then on, and nothing is ever
//! retried. A refused notification does not stop the run; a launcher that failed or timed out
//! stops it in `needs_attention` (a launcher that did not start, because it was removed,
//! unapproved or approved again with another digest, only records why).
//!
//! **Authority.** The engine reaches Teams and the launchers only here and only with
//! `Transport::Workflow` (`authority`), which no token, session or credential maps to; each path
//! checks it again (`require_workflow`), so the control channel, the browser, a machine
//! credential, MCP and `pta tools call` cannot reach them this way. A notification posts through
//! the `post_chat` path of `teams_post` (the service's session, else the profile's, a chat the
//! user belongs to, `@[Name]` mentions resolved against that chat's members only, never
//! retried); a launcher runs through the launcher path (`consumers::run_for_block`) only while
//! it is still approved with the digest stored in the block. A launcher's output is never
//! captured, and no outcome holds a value of a variable.
use super::record::{BlockOutcome, Run, State as Phase};
use super::*;
use crate::workflows::{Block, Kind, Stage};

/// One occurrence of a stage in a run: the stage and the key that tells it from the others of
/// the same stage (each child, each planning run, each failure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct Occurrence {
    pub stage: Stage,
    pub key: String,
}

/// The result of an acting block before it is recorded.
pub(in crate::app) struct Attempt {
    pub outcome: &'static str,
    pub reason: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub message_id: Option<String>,
}
impl Attempt {
    fn of(outcome: &'static str, reason: impl Into<String>) -> Self {
        Self {
            outcome,
            reason: Some(reason.into()),
            exit_code: None,
            duration_ms: None,
            message_id: None,
        }
    }
}

/// What `execute` did.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::app) enum Progress {
    /// Nothing was due.
    Idle,
    /// At least one block was attempted and its result recorded.
    Changed,
    /// A launcher is running for another run: nothing was recorded, ask again later.
    Waiting,
}

/// The occurrences of acting stages the run's record establishes now, in the order they
/// happen. Pure: the same record gives the same list, so a restart finds the same ones.
pub(in crate::app) fn occurrences(run: &Run) -> Vec<Occurrence> {
    let at = |stage, key: String| Occurrence { stage, key };
    let mut found = Vec::new();
    if run.state == Phase::Failed {
        // One for each failure: a failure after a resume is another occurrence.
        found.push(at(Stage::Failed, format!("failed:{}", run.timeline.len())));
        return found;
    }
    if !run.state.driven() {
        return found;
    }
    let planned = !run.task();
    if planned && run.state == Phase::Planning && run.planning.as_ref().is_none_or(|p| !p.started) {
        found.push(at(Stage::BeforePlanning, "before_planning".into()));
    }
    if planned
        && let Some(plan) = &run.planning
        && (run.state == Phase::PlanReview || run.ledger.as_deref() == Some(plan.id.as_str()))
    {
        found.push(at(Stage::PlanReady, format!("plan_ready:{}", plan.id)));
    }
    let coding = matches!(
        run.state,
        Phase::Coding | Phase::PullRequests | Phase::Blocks
    );
    if planned
        && coding
        && let Some(ledger) = &run.ledger
        && !run.children.is_empty()
    {
        found.push(at(
            Stage::ChildrenCreated,
            format!("children_created:{ledger}"),
        ));
    }
    if coding {
        for child in &run.children {
            if child.coding.as_ref().is_some_and(|c| c.started) {
                found.push(at(
                    Stage::CodingStarted,
                    format!("coding_started:{}", child.id),
                ));
            }
            if child.outcome.as_deref() == Some("done") {
                found.push(at(Stage::ChildDone, format!("child_done:{}", child.id)));
            }
        }
    }
    if run.state == Phase::Blocks {
        if run.children.iter().any(|c| !c.pull_requests.is_empty()) {
            found.push(at(
                Stage::PullRequestsCompleted,
                "pull_requests_completed".into(),
            ));
        }
        found.push(at(Stage::Done, "done".into()));
    }
    found
}

fn recorded(run: &Run, block: &Block, occurrence: &Occurrence) -> bool {
    run.block_outcomes
        .iter()
        .any(|o| o.block == block.id && o.occurrence == occurrence.key)
}

/// The next block to run: the first block, in the workflow's order, of the earliest occurrence
/// that has one not recorded yet (an attempt counts as recorded).
fn next_due(run: &Run) -> Option<(Block, Occurrence)> {
    for occurrence in occurrences(run) {
        let due = run.blocks.iter().find(|b| {
            b.kind != Kind::Instruction
                && b.stage == Some(occurrence.stage)
                && !recorded(run, b, &occurrence)
        });
        if let Some(block) = due {
            return Some((block.clone(), occurrence));
        }
    }
    None
}

/// Whether the engine has acting work for the run: a block due in its current stage, an
/// attempt left without a result that must be settled, or a run of an older version whose
/// passed occurrences must be marked.
pub(in crate::app) fn has_due(run: &Run) -> bool {
    run.acting_since.is_none()
        || run.block_outcomes.iter().any(|o| o.outcome == ATTEMPTING)
        || next_due(run).is_some()
}

const ATTEMPTING: &str = "attempting";
const SKIPPED: &str = "skipped";

/// A run recorded before acting blocks existed has no `acting_since`: the occurrences of its
/// stages that already happened (a failure long past, a plan that waited, children coded) are
/// marked `skipped` without acting, and only occurrences that happen from now on act.
fn mark_passed(run: &mut Run, now: i64) -> bool {
    if run.acting_since.is_some() {
        return false;
    }
    let mut skipped = Vec::new();
    for occurrence in occurrences(run) {
        for block in run.blocks.iter().filter(|b| {
            b.kind != Kind::Instruction
                && b.stage == Some(occurrence.stage)
                && !recorded(run, b, &occurrence)
        }) {
            skipped.push(BlockOutcome {
                block: block.id.clone(),
                kind: block.kind,
                stage: block.stage,
                outcome: SKIPPED.into(),
                at: now,
                occurrence: occurrence.key.clone(),
                reason: Some("before acting blocks existed".into()),
                exit_code: None,
                duration_ms: None,
                message_id: None,
            });
        }
    }
    run.block_outcomes.extend(skipped);
    run.acting_since = Some(now);
    run.updated_at = now;
    true
}

/// An attempt without a result belongs to an engine that stopped before it recorded one: what
/// it did is unknown, so it is `uncertain` and never made again. A launcher whose outcome is
/// unknown stops a run that is advancing, for a person to check its side effect; a notification
/// does not.
fn settle(run: &mut Run, now: i64) -> bool {
    let mut changed = false;
    let mut launcher = None;
    for outcome in run
        .block_outcomes
        .iter_mut()
        .filter(|o| o.outcome == ATTEMPTING)
    {
        outcome.outcome = "uncertain".into();
        outcome.reason = Some(
            "The host stopped after this block's attempt was recorded and before its result; whether it acted is unknown, and it is never attempted again.".into(),
        );
        outcome.at = now;
        if outcome.kind == Kind::Launcher {
            launcher.get_or_insert(outcome.block.clone());
        }
        changed = true;
    }
    if changed {
        run.updated_at = now;
        if let Some(block) = launcher
            && run.state.driven()
        {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "The host stopped while launcher block {block} was running, so whether it ran is unknown (its output is not kept); check what it was meant to do, then resume to continue without running it again, or cancel the run."
                ),
            );
        }
    }
    changed
}

/// After a start: every attempt of every run that was left without a result is settled, so a
/// stopped run never shows `attempting` for good.
pub(in crate::app) async fn settle_all(host: &Arc<Host>, now: i64) -> Result<()> {
    let state = &host.state.workflows;
    for run in state.db.list(&[], None, -1)? {
        if !run.block_outcomes.iter().any(|o| o.outcome == ATTEMPTING) {
            continue;
        }
        let lock = state.lock(&run.id);
        let _guard = lock.lock().await;
        let Some(mut run) = state.db.get(&run.id)? else {
            continue;
        };
        if settle(&mut run, now) {
            state.db.save(&run)?;
        }
    }
    Ok(())
}

/// Run every acting block that is due in the run, one after another, each recorded before and
/// after it acts, until none is due, a launcher stops the run, the run changed stage meanwhile
/// or a launcher cannot start yet. The run's lock is held only while recording, never while a
/// block acts, so a cancel is not kept waiting.
pub(in crate::app) async fn execute(host: &Arc<Host>, id: &str, now: i64) -> Result<Progress> {
    let state = &host.state.workflows;
    let lock = state.lock(id);
    let mut progress = Progress::Idle;
    loop {
        let (block, occurrence, run, slot) = {
            let _guard = lock.lock().await;
            let Some(mut run) = state.db.get(id)? else {
                return Ok(progress);
            };
            let mut dirty = settle(&mut run, now);
            dirty |= mark_passed(&mut run, now);
            let due = if run.state.driven() || run.state == Phase::Failed {
                next_due(&run)
            } else {
                None
            };
            let Some((block, occurrence)) = due else {
                if dirty {
                    state.db.save(&run)?;
                }
                return Ok(progress);
            };
            // A launcher never runs twice at once, and only a few run together. The slot is
            // taken before the attempt is recorded, so a launcher that is busy is waited for
            // with nothing recorded, never lost as an outcome.
            let slot = match (block.kind, &block.launcher) {
                (Kind::Launcher, Some(launcher)) => {
                    match consumers::try_launcher_slot(&host.state, &launcher.id) {
                        Some(slot) => Some(slot),
                        None => {
                            if dirty {
                                state.db.save(&run)?;
                            }
                            return Ok(Progress::Waiting);
                        }
                    }
                }
                _ => None,
            };
            run.block_outcomes.push(BlockOutcome {
                block: block.id.clone(),
                kind: block.kind,
                stage: block.stage,
                outcome: ATTEMPTING.into(),
                at: now,
                occurrence: occurrence.key.clone(),
                reason: None,
                exit_code: None,
                duration_ms: None,
                message_id: None,
            });
            run.updated_at = now;
            state.db.save(&run)?;
            (block, occurrence, run, slot)
        };
        // Where the run was when the block began: a result that comes back after the run moved
        // on (a resume, a cancel) belongs to a stage that is over and decides nothing now.
        let (began_in, began_at) = (run.state, run.timeline.len());
        // The engine's own authority, for the two paths and nothing else.
        let result = match authority::workflow(&host.state) {
            Ok(invocation) => {
                authority::within(invocation, act(host, &run, &block, &occurrence, slot)).await
            }
            Err(error) => Attempt::of("refused", error.to_string()),
        };
        let (stop, moved) = {
            let _guard = lock.lock().await;
            let Some(mut run) = state.db.get(id)? else {
                return Ok(progress);
            };
            let now = chrono::Utc::now().timestamp().max(now);
            if let Some(entry) = run
                .block_outcomes
                .iter_mut()
                .find(|o| o.block == block.id && o.occurrence == occurrence.key)
            {
                entry.outcome = result.outcome.into();
                entry.reason = result.reason.clone();
                entry.exit_code = result.exit_code;
                entry.duration_ms = result.duration_ms;
                entry.message_id = result.message_id.clone();
                entry.at = now;
            }
            let moved = run.state != began_in || run.timeline.len() != began_at;
            // A launcher that failed, timed out, was stopped or whose outcome is unknown stops
            // the run that is still in the stage the block belongs to; one that already failed,
            // was resumed or was cancelled is not stopped again.
            let stops = block.kind == Kind::Launcher
                && matches!(
                    result.outcome,
                    "failed" | "timed_out" | "stopped" | "uncertain"
                )
                && !moved
                && run.state.driven();
            if stops {
                let name = block.stage.map_or("a", stage_word);
                run.stop(
                    Phase::NeedsAttention,
                    now,
                    format!(
                        "Launcher block {} ({}) {} at the {name} stage (exit code {}); its output is not kept. Resume to continue without running it again, or cancel the run.",
                        block.name,
                        block.launcher.as_ref().map_or("", |l| l.id.as_str()),
                        match result.outcome {
                            "timed_out" => "timed out",
                            "stopped" => "was stopped",
                            _ => "ended unknown",
                        },
                        result.exit_code.map_or("none".into(), |c| c.to_string()),
                    ),
                );
            } else {
                run.updated_at = now;
            }
            state.db.save(&run)?;
            (stops, moved)
        };
        progress = Progress::Changed;
        if stop || moved {
            return Ok(progress);
        }
    }
}

fn stage_word(stage: Stage) -> &'static str {
    match stage {
        Stage::BeforePlanning => "before_planning",
        Stage::PlanReady => "plan_ready",
        Stage::ChildrenCreated => "children_created",
        Stage::CodingStarted => "coding_started",
        Stage::ChildDone => "child_done",
        Stage::PullRequestsCompleted => "pull_requests_completed",
        Stage::Done => "done",
        Stage::Failed => "failed",
    }
}

async fn act(
    host: &Arc<Host>,
    run: &Run,
    block: &Block,
    occurrence: &Occurrence,
    slot: Option<consumers::LauncherSlot>,
) -> Attempt {
    match block.kind {
        Kind::TeamsNotification => notify(host, run, block, occurrence).await,
        Kind::Launcher => match slot {
            Some(slot) => launch(host, block, slot).await,
            None => Attempt::of("not_started", "No launcher slot was taken."),
        },
        Kind::Instruction => Attempt::of("refused", "An instruction block does not act."),
    }
}

// ---- Launchers ----

async fn launch(host: &Arc<Host>, block: &Block, slot: consumers::LauncherSlot) -> Attempt {
    let Some(launcher) = &block.launcher else {
        return Attempt::of("not_started", "The block names no launcher.");
    };
    match consumers::run_for_block(host, &launcher.id, &launcher.digest, slot).await {
        Ok(consumers::BlockLaunch::Ran(ran)) => Attempt {
            outcome: ran.status,
            reason: None,
            exit_code: ran.exit_code,
            duration_ms: Some(ran.duration_ms),
            message_id: None,
        },
        Ok(consumers::BlockLaunch::NotStarted(why)) => Attempt::of("not_started", why),
        Err(error) => Attempt::of("not_started", error),
    }
}

// ---- Notifications ----

/// Longest value one placeholder puts in a message.
const VALUE: usize = 3000;

/// Make a value safe to put in a message: one a person or a model wrote cannot mention anybody
/// (only the block's own mentions do), and it stays within its bound.
fn plain(value: &str, max: usize) -> String {
    let flat: String = value
        .chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect();
    let mut text = flat.trim().replace("@[", "@\u{200b}[");
    if text.chars().count() > max {
        text = text.chars().take(max).collect::<String>() + "…";
    }
    text
}

/// What `{state}` says at a stage.
fn state_word(run: &Run, stage: Stage) -> &'static str {
    match stage {
        Stage::Done => "done",
        Stage::Failed => "failed",
        Stage::PlanReady => "plan_review",
        Stage::BeforePlanning => "planning",
        Stage::CodingStarted => "coding",
        _ => run.state.word(),
    }
}

/// The links of pull requests, one markdown list item each.
fn pull_request_links<'a>(
    pulls: impl Iterator<Item = &'a crate::ado::workflow::PullRequest>,
) -> String {
    pulls
        .map(|p| {
            let title = plain(&p.title, 120).replace(['[', ']', '(', ')'], "");
            format!("- [PR {}: {}]({})", p.id, title, p.url)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The latest stored summary of the run (written by the director on request), or nothing.
fn latest_summary(run: &Run) -> String {
    let Some(last) = run.summaries.last() else {
        return String::new();
    };
    last["text"]
        .as_str()
        .or_else(|| last.as_str())
        .map(|t| plain(t, VALUE))
        .unwrap_or_default()
}

/// The template with its placeholders replaced, by code and in one pass: what replaces one is
/// never read as another placeholder or as a mention. `{item}` is `Title (#ID)`; `{id}` and
/// `{title}` are its parts, the title without square brackets. At `coding_started` and
/// `child_done` the item is the child being coded (its title, number and link, and its pull
/// requests); at every other stage it
/// is the run's work item with the pull requests of all its children.
pub(in crate::app) fn render(run: &Run, block: &Block, occurrence: &Occurrence) -> String {
    let prefix = match occurrence.stage {
        Stage::ChildDone => Some("child_done"),
        Stage::CodingStarted => Some("coding_started"),
        _ => None,
    };
    let child = prefix.and_then(|prefix| {
        run.children
            .iter()
            .find(|c| occurrence.key == format!("{prefix}:{}", c.id))
    });
    let (title, id, link) = match child {
        Some(c) => (c.title.as_str(), c.id, c.url.as_str()),
        None => (run.item.title.as_str(), run.item.id, run.item.url.as_str()),
    };
    let pulls = match child {
        Some(c) => pull_request_links(c.pull_requests.iter()),
        None => pull_request_links(run.children.iter().flat_map(|c| c.pull_requests.iter())),
    };
    let values: [(&str, String); 7] = [
        ("item", format!("{} (#{})", plain(title, 300), id)),
        ("id", id.to_string()),
        // Without brackets, so `[{id} - {title}]({link})` stays one Markdown link.
        ("title", plain(title, 300).replace(['[', ']'], "")),
        ("link", link.to_owned()),
        ("pull_requests", pulls),
        ("summary", latest_summary(run)),
        ("state", state_word(run, occurrence.stage).to_owned()),
    ];
    let template = &block.template;
    let mut text = String::new();
    let mut rest = template.as_str();
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        let tail = &rest[open..];
        let hit = values.iter().find(|(name, _)| {
            tail.get(1..=name.len()) == Some(*name) && tail[1 + name.len()..].starts_with('}')
        });
        match hit {
            Some((name, value)) => {
                text.push_str(value);
                rest = &tail[name.len() + 2..];
            }
            None => {
                text.push('{');
                rest = &tail[1..];
            }
        }
    }
    text.push_str(rest);
    // Mention syntax anywhere in the rendered text is made inert, wherever it came from (a
    // value that starts with `[` after the template's `@` included): only the block's own
    // mentions, which `compose` writes after this, mention anybody.
    text.replace("@[", "@\u{200b}[")
}

/// The message a notification posts: the members the block mentions, then its rendered text.
pub(in crate::app) fn compose(
    run: &Run,
    block: &Block,
    occurrence: &Occurrence,
) -> std::result::Result<String, String> {
    let mut text = String::new();
    for name in &block.mentions {
        if name.contains(']') || name.chars().count() > crate::workflows::MAX_MENTION {
            return Err(format!(
                "The mention {name} cannot be written as a Teams mention."
            ));
        }
        text.push_str(&format!("@[{name}] "));
    }
    text.push_str(&render(run, block, occurrence));
    let text = text.trim().to_owned();
    if text.is_empty() || text.chars().count() > 20_000 {
        return Err("The rendered message is empty or too long.".into());
    }
    Ok(text)
}

fn redacted(text: &str) -> String {
    let redactor = security::Redactor::new(&[], Vec::new()).expect("built-in redaction patterns");
    redactor.redact(text).chars().take(400).collect()
}

async fn notify(host: &Arc<Host>, run: &Run, block: &Block, occurrence: &Occurrence) -> Attempt {
    let Some(chat) = &block.chat else {
        return Attempt::of("refused", "The block names no chat.");
    };
    let (attempt, store) = match compose(run, block, occurrence) {
        Ok(text) => post(host, &chat.id, &text).await,
        Err(reason) => (Attempt::of("refused", reason), None),
    };
    audit(host, store.as_deref(), attempt.outcome);
    attempt
}

/// Post once in the chat: no retry, whatever happens. Returns the result and the event log of
/// the session that posted, when there was one.
pub(in crate::app) async fn post(
    host: &Arc<Host>,
    chat: &str,
    text: &str,
) -> (Attempt, Option<Arc<crate::state::Store>>) {
    use crate::adapters::graph::PostRefusal;
    if let Err(error) = authority::require_workflow(&host.state) {
        return (Attempt::of("refused", error.to_string()), None);
    }
    // No Microsoft session (or one the running assistant holds for another use): refused.
    let graph = match teams_graph(&host.state).await {
        Ok(graph) => graph,
        Err(error) => {
            let reason = format!(
                "There is no Microsoft session to post with: {}",
                redacted(&error.to_string())
            );
            return (Attempt::of("refused", reason), None);
        }
    };
    let store = Some(graph.store.clone());
    let attempt = match graph.post_chat(chat, text, &[]).await {
        Ok(sent) => Attempt {
            outcome: "sent",
            reason: None,
            exit_code: None,
            duration_ms: None,
            message_id: sent["message_id"].as_str().map(str::to_owned),
        },
        Err(error) => match error.downcast_ref::<PostRefusal>() {
            Some(PostRefusal::Uncertain) => {
                Attempt::of("uncertain", PostRefusal::Uncertain.to_string())
            }
            Some(refusal) => Attempt::of("refused", refusal.to_string()),
            // Everything else fails before the request is made (the session, the chat's
            // members), so nothing was sent.
            None => Attempt::of(
                "refused",
                format!("Nothing was sent: {}", redacted(&error.to_string())),
            ),
        },
    };
    (attempt, store)
}

/// Record the notification in the audit: a `workflow_notification` event (`sent`, `refused` or
/// `uncertain`) in the assistant's event log, beside the `teams_post` event of the send itself.
/// A log that cannot be written never changes the outcome.
fn audit(host: &Arc<Host>, store: Option<&crate::state::Store>, outcome: &str) {
    const KIND: &str = "workflow_notification";
    if let Some(store) = store {
        let _ = store.event(KIND, outcome);
        return;
    }
    // Refused before a session was reached: the profile's own log, when it exists.
    if let Ok(config) = host.state.settings.config() {
        let path = config.server.data_dir.join("assistant.db");
        if path.exists()
            && let Ok(store) = crate::state::Store::attach(&path)
        {
            let _ = store.event(KIND, outcome);
        }
    }
}
