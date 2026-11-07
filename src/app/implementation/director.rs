//! The workflow director's session. It runs on the director's machine in the same retained,
//! tool-less, read-only, closed-JSON session as planning (`llm::implementation::direct`), in a
//! private `implementation/requests/<run-id>` directory, with the restricted child environment;
//! on another machine it is the additive one-shot call `workflow_direct` (`answer`), which a
//! worker runs beside its poll and advertises with `workflow_direct_support`. The coordinator
//! records each session as an agent session in `request_runs` (purpose `classification`: the
//! machine, provider, model, effort, native session ID, timestamps and the classified item's
//! verified link), so Agent history lists it; startup marks one still generating
//! `interrupted`. Nothing here starts a run or writes to a provider.
use super::*;
use crate::llm::implementation::Direction;

/// How long one director session may take on its machine.
pub(in crate::app) const SESSION_SECONDS: u64 = 85;
/// How long the coordinator waits for another machine's answer: with the item read before it,
/// a classification answers within the control channel's 120-second client deadline.
pub(super) const WAIT_SECONDS: u64 = 90;
const INSTRUCTIONS: usize = 12_000;
const INPUT: usize = 200_000;

/// What the coordinator asks the director.
pub(in crate::app) struct Ask<'a> {
    pub machine: &'a str,
    pub director: &'a crate::workflows::director::Director,
    /// The resolved `direct` prompt.
    pub instructions: &'a str,
    /// The item and the candidate workflows, as JSON (untrusted input to the director).
    pub input: String,
    /// The classified item, read fresh: its verified link goes in the record.
    pub item: &'a crate::tools::todo::Item,
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
    id: String,
    provider: String,
    model: String,
    effort: String,
    instructions: String,
    input: String,
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
    crate::llm::implementation::validate(&call.provider, &call.model, &call.effort).await?;
    let directory = host
        .state
        .implementation
        .root
        .join("requests")
        .join(&call.id);
    security::private_dir(&directory)?;
    match tokio::time::timeout(
        Duration::from_secs(SESSION_SECONDS),
        crate::llm::implementation::direct(
            (&call.provider, &call.model, &call.effort),
            directory,
            &call.instructions,
            &call.input,
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
        json!({"answer":direction.answer,"problem":direction.problem,"session_id":session_id,"started_at":started_at,"finished_at":now_text()}),
    )
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
    let record = json!({"id":id,"machine":ask.machine,"purpose":"classification","project":item.project,"kind":item.kind,"provider":director.provider,"model":director.model,"effort":director.effort,"state":"generating","created_at":now_text(),"started_at":null,"finished_at":null,"session_id":null,"item":item,"output":"","permission_mode":crate::llm::implementation::permission_mode(&director.provider, true)});
    state.db.request_insert(&record)?;
    let call = Call {
        id: id.clone(),
        provider: director.provider.clone(),
        model: director.model.clone(),
        effort: director.effort.clone(),
        instructions: ask.instructions.to_owned(),
        input: ask.input,
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
            let session = reply["session_id"].as_str().map(str::to_owned);
            if let Some(session) = &session {
                valid_session(session)?;
            }
            let started = reply["started_at"]
                .as_str()
                .filter(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok())
                .map(str::to_owned);
            let problem = known_problem(&reply["problem"]);
            let direction = Direction {
                answer: if problem.is_none() {
                    reply.get("answer").cloned().filter(|a| !a.is_null())
                } else {
                    None
                },
                problem: problem.or(reply["answer"]
                    .is_null()
                    .then_some("The workflow director returned no answer.")),
            };
            Ok((direction, session, started))
        }
        .await
    };
    let finished =
        |state_name: &str, output: &str, session: Option<&str>, started: Option<&str>| {
            let (state_name, output) = (state_name.to_owned(), output.to_owned());
            LocalImplementation::new(&state.db).finish_request(&id, &mut |record| {
                record["state"] = json!(state_name);
                record["output"] = json!(output);
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
            let _ = finished(
                "failed",
                "The workflow director did not run. Check its machine, installed CLI, model and login.",
                None,
                None,
            );
            Err(error)
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
