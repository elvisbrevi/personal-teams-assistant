//! The workflow director's session. It runs on the director's machine in the same tool-less,
//! read-only, closed-JSON session as planning (`llm::implementation::direct`), never retained
//! after its answer, in a private temporary directory under `implementation/directors/` removed
//! when the session ends, with the restricted child environment;
//! on another machine it is the additive one-shot call `workflow_direct` (`answer`), which a
//! worker runs beside its poll and advertises with `workflow_direct_support`. The coordinator
//! records each session as an agent session in `request_runs` (purpose `classification`: the
//! machine, provider, model, effort, native session ID, timestamps and the classified item's
//! verified link), so Agent history lists it; startup marks one still generating
//! `interrupted`. Nothing here starts a run or writes to a provider.
use super::*;
use crate::llm::implementation::{Cause, Direction, Failed, Failure};

/// How long listing the machine's installed CLIs may take before a session, when the last
/// probe (`llm::implementation::PROBE_REUSE`) does not offer the agent.
pub(in crate::app) const PROBE_SECONDS: u64 = 15;
/// How long one director session may take on its machine.
pub(in crate::app) const SESSION_SECONDS: u64 = 60;
/// A worker polls every two seconds: one poll to claim the call, one to return its answer,
/// with room for a slow round-trip.
pub(in crate::app) const POLL_SECONDS: u64 = 10;
/// How long the coordinator waits for another machine's answer: the probe, the session and the
/// polls, with ten seconds of margin (95). With the item read before it, a classification
/// answers within the control channel's client deadline (`control::CLIENT_DEADLINE_SECONDS`).
pub(in crate::app) const WAIT_SECONDS: u64 = PROBE_SECONDS + SESSION_SECONDS + POLL_SECONDS + 10;
const INSTRUCTIONS: usize = 12_000;
const INPUT: usize = 200_000;

/// What the coordinator asks the director.
pub(in crate::app) struct Ask<'a> {
    pub machine: &'a str,
    /// Choosing a workflow for an item, or summarizing a run (the record's `purpose`).
    pub task: crate::llm::implementation::DirectTask,
    pub director: &'a crate::workflows::director::Director,
    /// The resolved `direct` prompt.
    pub instructions: &'a str,
    /// The item and the candidate workflows, as JSON (untrusted input to the director).
    pub input: String,
    /// The classified item, read fresh: its verified link goes in the record.
    pub item: &'a crate::tools::todo::Item,
    /// The session's own bound, when it is not the director's (`SESSION_SECONDS`): the
    /// supervisor's `session_seconds`.
    pub seconds: Option<u64>,
}
/// The recorded session and what it returned.
pub(in crate::app) struct Answer {
    pub run: String,
    pub session_id: Option<String>,
    pub direction: Direction,
}

/// The call a director's machine answers (`workflow_direct`).
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Call {
    /// Left out for choosing a workflow, so an older worker, which does not know the field,
    /// still answers it.
    #[serde(
        default,
        skip_serializing_if = "crate::llm::implementation::DirectTask::is_classify"
    )]
    task: crate::llm::implementation::DirectTask,
    id: String,
    provider: String,
    model: String,
    effort: String,
    instructions: String,
    input: String,
    /// The item's ID, for the session's name; sent only to a worker that advertises
    /// `session_names_support` (an older one refuses unknown fields) and absent from an older
    /// coordinator's call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item: Option<u64>,
    /// The session's bound when it is not `SESSION_SECONDS` (a supervisor's look); left out
    /// otherwise, and sent only to a worker that advertises `workflow_supervise_support`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seconds: Option<u64>,
}

/// The longest session a call may ask for (the supervisor's `session_seconds` bound).
const MAX_SESSION_SECONDS: u64 = 600;
/// How long the coordinator waits for another machine's answer to a session bounded to
/// `seconds`: the probe, the session and the polls, with ten seconds of margin.
pub(in crate::app) fn wait_seconds(seconds: u64) -> u64 {
    PROBE_SECONDS + seconds + POLL_SECONDS + 10
}

fn valid_session(session: &str) -> Result<()> {
    ensure!(
        (1..=200).contains(&session.len())
            && session
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
        "Invalid session ID."
    );
    Ok(())
}
fn now_text() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Run the director's session on this machine.
async fn session(
    host: &Arc<Host>,
    call: &Call,
    observer: crate::llm::implementation::SessionObserver<'_>,
    remote: crate::llm::implementation::mobile::Observer,
) -> Result<Direction> {
    uuid(&call.id)?;
    ensure!(
        call.instructions.chars().count() <= INSTRUCTIONS && call.input.len() <= INPUT,
        "The workflow director's input is too long."
    );
    let seconds = call.seconds.unwrap_or(SESSION_SECONDS);
    ensure!(
        (1..=MAX_SESSION_SECONDS).contains(&seconds),
        "Invalid session bound."
    );
    crate::llm::implementation::validate_recent(
        &call.provider,
        &call.model,
        &call.effort,
        Duration::from_secs(PROBE_SECONDS),
    )
    .await?;
    // A private directory for this session only, removed when it ends (`directory` drops).
    let parent = host.state.implementation.root.join("directors");
    security::private_dir(&parent)?;
    let directory = tempfile::Builder::new()
        .prefix("session-")
        .tempdir_in(&parent)?;
    security::private_dir(directory.path())?;
    match tokio::time::timeout(
        Duration::from_secs(seconds),
        crate::llm::implementation::direct(
            call.task,
            (&call.provider, &call.model, &call.effort),
            directory.path().to_path_buf(),
            &call.instructions,
            &call.input,
            &call.task.session_name(call.item),
            observer,
            remote,
        ),
    )
    .await
    {
        Ok(direction) => direction,
        // The session's process tree ends with it; it was read-only and wrote nothing.
        Err(_) => Ok(Direction {
            answer: None,
            problem: Some("The workflow director did not answer in time."),
            failure: Some(Failure::of(Cause::Timeout)),
        }),
    }
}

/// `workflow_direct` on the director's machine: run the session and return what it gave, with
/// its native session ID and times. The coordinator keeps the record.
pub(super) async fn answer(host: &Arc<Host>, args: &Value) -> Result<Value> {
    let call: Call = serde_json::from_value(args.clone())?;
    let reported = std::sync::Mutex::new((None::<String>, None::<String>));
    let observer = |id: Option<&str>| {
        authority::require_execution(&host.state)?;
        let mut reported = reported.lock().unwrap();
        if let Some(id) = id {
            valid_session(id)?;
            reported.0 = Some(id.into());
        }
        reported.1.get_or_insert_with(now_text);
        Ok(())
    };
    let direction = session(host, &call, &observer, Arc::new(|_| Ok(()))).await?;
    let (session_id, started_at) = reported.into_inner().unwrap();
    Ok(
        json!({"answer":direction.answer,"problem":direction.problem,"failure":direction.failure,"session_id":session_id,"started_at":started_at,"finished_at":now_text()}),
    )
}

/// Why the director's machine gave no answer, as it reported it: its `failure` within the
/// vocabulary; from a machine that predates causes, what its fixed problem says, else
/// `unknown`.
fn reported_failure(reply: &Value, problem: Option<&str>) -> Failure {
    if !reply["failure"].is_null() {
        return Failure::read(&reply["failure"]);
    }
    Failure::of(match problem {
        Some("The workflow director's answer is not the JSON it was asked for.") => {
            Cause::InvalidAnswer
        }
        Some("The workflow director did not answer in time.") => Cause::Timeout,
        _ => Cause::Unknown,
    })
}
/// The text of a director's session that did not run.
fn did_not_run(failure: &Failure) -> String {
    format!("The workflow director did not run. {}", failure.sentence())
}

/// The fixed problems a machine may report, so no other text of it reaches the record.
fn known_problem(problem: &Value) -> Option<&'static str> {
    [
        "The workflow director returned no answer.",
        "The workflow director's answer is not the JSON it was asked for.",
        "The workflow director did not answer in time.",
    ]
    .into_iter()
    .find(|known| problem == *known)
}

/// Ask the director on its machine and record the session on this coordinator.
pub(in crate::app) async fn ask(host: &Arc<Host>, ask: Ask<'_>) -> Result<Answer> {
    let state = &host.state.implementation;
    let own = state.registry.lock().await.id.clone();
    let local = ask.machine == own;
    if local {
        // The shared server runs no agent for another account.
        authority::require_execution(&host.state)?;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let mut item = ask.item.clone();
    item.dates.clear();
    let director = ask.director;
    let purpose = match ask.task {
        crate::llm::implementation::DirectTask::Classify => "classification",
        crate::llm::implementation::DirectTask::Summarize => "summary",
        crate::llm::implementation::DirectTask::Supervise => "supervision",
        crate::llm::implementation::DirectTask::Audit => "audit",
    };
    let named = local
        || state
            .registry
            .lock()
            .await
            .machines
            .iter()
            .any(|m| m.id == ask.machine && m.session_names);
    let record = json!({"id":id,"machine":ask.machine,"purpose":purpose,"project":item.project,"kind":item.kind,"provider":director.provider,"model":director.model,"effort":director.effort,"state":"generating","created_at":now_text(),"started_at":null,"finished_at":null,"session_id":null,"item":item,"output":"","permission_mode":crate::llm::implementation::permission_mode(&director.provider, true)});
    state.db.request_insert(&record)?;
    let call = Call {
        task: ask.task,
        id: id.clone(),
        provider: director.provider.clone(),
        model: director.model.clone(),
        effort: director.effort.clone(),
        instructions: ask.instructions.to_owned(),
        input: ask.input,
        item: named.then_some(item.id),
        seconds: ask.seconds,
    };
    let result: Result<(Direction, Option<String>, Option<String>)> = if local {
        let observer = |session: Option<&str>| {
            authority::require_execution(&host.state)?;
            state.db.request_update(&id, |record| {
                if let Some(session) = session {
                    valid_session(session)?;
                    record["session_id"] = json!(session);
                }
                if record["started_at"].is_null() {
                    record["started_at"] = json!(now_text());
                }
                Ok(())
            })
        };
        Box::pin(session(
            host,
            &call,
            &observer,
            super::remote_observer(host, &id),
        ))
        .await
        .map(|direction| (direction, None, None))
    } else {
        async {
            let reply = fleet::call(
                host,
                ask.machine,
                "workflow_direct",
                serde_json::to_value(&call)?,
            )
            .await?;
            // A session ID that is not one is dropped; the answer is still judged.
            let session = reply["session_id"]
                .as_str()
                .filter(|s| valid_session(s).is_ok())
                .map(str::to_owned);
            let started = reply["started_at"]
                .as_str()
                .filter(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok())
                .map(str::to_owned);
            let problem = known_problem(&reply["problem"]);
            let answer = if problem.is_none() {
                reply.get("answer").cloned().filter(|a| !a.is_null())
            } else {
                None
            };
            let failure = answer.is_none().then(|| reported_failure(&reply, problem));
            let direction = Direction {
                problem: problem.or(answer
                    .is_none()
                    .then_some("The workflow director returned no answer.")),
                answer,
                failure,
            };
            Ok((direction, session, started))
        }
        .await
    };
    let finished = |state_name: &str,
                    output: &str,
                    failure: Option<Failure>,
                    session: Option<&str>,
                    started: Option<&str>| {
        let (state_name, output) = (state_name.to_owned(), output.to_owned());
        LocalImplementation::new(&state.db).finish_request(&id, &mut |record| {
            record["state"] = json!(state_name);
            record["output"] = json!(output);
            if let Some(failure) = failure {
                record["failure"] = json!(failure);
            }
            if let Some(session) = session {
                record["session_id"] = json!(session);
            }
            if let Some(started) = started
                && record["started_at"].is_null()
            {
                record["started_at"] = json!(started);
            }
            record["finished_at"] = json!(now_text());
            Ok(())
        })
    };
    match result {
        Ok((direction, session, started)) => {
            finished(
                if direction.answer.is_some() {
                    "done"
                } else {
                    "failed"
                },
                direction.problem.unwrap_or_default(),
                direction.failure,
                session.as_deref(),
                started.as_deref(),
            )?;
            let session_id = state
                .db
                .requests(Some(&id))?
                .pop()
                .and_then(|r| r["session_id"].as_str().map(str::to_owned));
            Ok(Answer {
                run: id,
                session_id,
                direction,
            })
        }
        Err(error) => {
            // Another machine's call that fails carries its cause or never reached it.
            let failure = crate::llm::implementation::of_error(
                &error,
                if local {
                    Cause::Unknown
                } else {
                    Cause::MachineOffline
                },
            );
            let _ = finished("failed", &did_not_run(&failure), Some(failure), None, None);
            // An error that names its cause already says it; any other keeps its own fixed
            // text (this package's) and carries the cause.
            if error.chain().any(|e| e.is::<Failed>()) {
                Err(Failed::wrap(failure, did_not_run(&failure)))
            } else {
                Err(Failed::wrap(failure, error.to_string()))
            }
        }
    }
}

/// Keep code's verdict on the director's answer in the session's record.
pub(in crate::app) fn record(host: &Arc<Host>, run: &str, verdict: Value) -> Result<()> {
    LocalImplementation::new(&host.state.implementation.db).finish_request(run, &mut |record| {
        record["classification"] = verdict.clone();
        Ok(())
    })
}

/// Keep a field about the session (the workflow run a summary is of, the verdict on its
/// output) in its record.
pub(in crate::app) fn annotate(host: &Arc<Host>, run: &str, key: &str, value: Value) -> Result<()> {
    LocalImplementation::new(&host.state.implementation.db).finish_request(run, &mut |record| {
        record[key] = value.clone();
        Ok(())
    })
}
