//! The account's recent coding-agent sessions, for the agent sessions source
//! (`crate::sessions`). Each machine summarizes its own; the coordinator asks every online
//! machine of the account through the machine channel and adds the sessions PTA started, with
//! their work items; another installation asks the coordinator. A machine that is offline, too
//! old or does not answer is reported unavailable, never as having no sessions.

use super::*;
use crate::sessions::{Listing, Started, Summary, Unavailable};

/// The reader the message pipeline uses, installed when the host starts.
pub(in crate::app) struct FleetReader(std::sync::Weak<Host>);
impl FleetReader {
    pub(in crate::app) fn new(host: &Arc<Host>) -> Self {
        Self(Arc::downgrade(host))
    }
}
#[async_trait::async_trait]
impl crate::sessions::Reader for FleetReader {
    async fn recent(&self, limit: usize) -> Result<Listing> {
        let host = self.0.upgrade().context("The host stopped.")?;
        recent(&host, limit).await
    }
}

/// `implementation_recent_sessions`: what an agent sessions source would read now.
pub(super) async fn operate(host: &Arc<Host>, args: &Value) -> Result<Value> {
    Ok(serde_json::to_value(recent(host, limit(args)?).await?)?)
}
fn limit(args: &Value) -> Result<usize> {
    match &args["limit"] {
        Value::Null => Ok(crate::sessions::DEFAULT_LIMIT),
        value => {
            let limit = value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| (1..=crate::sessions::MAX_LIMIT).contains(n));
            limit.context("Choose between 1 and 50 sessions.")
        }
    }
}

/// Boxed with its `Send` bound written out: a machine call can reach this operation again
/// through the host's dispatch, and the compiler cannot infer the bound through that cycle.
pub(in crate::app) fn recent(
    host: &Arc<Host>,
    limit: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Listing>> + Send + '_>> {
    Box::pin(async move { recent_now(host, limit).await })
}
async fn recent_now(host: &Arc<Host>, limit: usize) -> Result<Listing> {
    if let Some(connection) = host.state.implementation.connection()? {
        // Another installation of the account: the coordinator gathers every machine's.
        let asked = fleet::remote(
            &connection,
            json!({"action":"call","method":"implementation_recent_sessions","args":{"limit":limit}}),
        )
        .await
        .and_then(|value| Ok(serde_json::from_value::<Listing>(value)?));
        return Ok(match asked {
            Ok(listing) => listing,
            Err(_) => {
                let mut listing = own(host, limit).await?;
                listing.unavailable.push(Unavailable {
                    machine: "coordinator".into(),
                    provider: String::new(),
                    reason: "no_answer".into(),
                });
                listing
            }
        });
    }
    gather(host, limit).await
}

/// On the coordinator: its own sessions, every online machine's and PTA's own runs.
pub(super) async fn gather(host: &Arc<Host>, limit: usize) -> Result<Listing> {
    let version = management::version(host)?;
    let (own_id, machines) = {
        let data = host.state.implementation.registry.lock().await;
        let machines: Vec<(String, String, Option<&'static str>)> = data
            .machines
            .iter()
            .filter(|m| !m.revoked && m.id != data.id)
            .map(|m| {
                let gap = if !m.online() || m.account_version != version {
                    Some("offline")
                } else if !m.agent_sessions {
                    Some("outdated")
                } else {
                    None
                };
                (m.id.clone(), m.name.clone(), gap)
            })
            .collect();
        (data.id.clone(), machines)
    };
    let mut parts = vec![own(host, limit).await?];
    let mut unavailable = Vec::new();
    let mut asks = tokio::task::JoinSet::new();
    for (id, name, gap) in machines {
        if let Some(reason) = gap {
            unavailable.push(Unavailable {
                machine: name,
                provider: String::new(),
                reason: reason.into(),
            });
            continue;
        }
        let host = host.clone();
        asks.spawn(async move {
            let asked = tokio::time::timeout(
                Duration::from_secs(crate::sessions::READ_SECONDS - 5),
                fleet::call(&host, &id, "agent_sessions_local", json!({"limit":limit})),
            )
            .await;
            (name, asked)
        });
    }
    while let Some(Ok((name, asked))) = asks.join_next().await {
        match asked
            .ok()
            .and_then(Result::ok)
            .and_then(|v| serde_json::from_value::<Listing>(v).ok())
        {
            Some(mut listing) => {
                // The coordinator's name for the machine, whatever it calls itself.
                for session in &mut listing.sessions {
                    session.machine = name.clone();
                }
                for gap in &mut listing.unavailable {
                    gap.machine = name.clone();
                }
                parts.push(listing);
            }
            None => unavailable.push(Unavailable {
                machine: name,
                provider: String::new(),
                reason: "no_answer".into(),
            }),
        }
    }
    let names: Vec<(String, String)> = {
        let data = host.state.implementation.registry.lock().await;
        data.machines
            .iter()
            .map(|m| (m.id.clone(), m.name.clone()))
            .chain([(own_id, data.settings.name.clone())])
            .collect()
    };
    let runs = host
        .state
        .implementation
        .db
        .list(None, None, false)
        .unwrap_or_default();
    Ok(combine(parts, unavailable, &runs, &names, limit))
}

/// The listings and PTA's runs as one listing: a run whose native session is listed names it;
/// one that is not (its machine is offline, or the agent's store was cleaned) is listed from
/// its own record, with the item it worked on.
fn combine(
    parts: Vec<Listing>,
    unavailable: Vec<Unavailable>,
    runs: &[Run],
    names: &[(String, String)],
    limit: usize,
) -> Listing {
    let scrubber = crate::sessions::Scrubber::new();
    let started: Vec<(String, Started)> = runs
        .iter()
        .filter_map(|run| {
            Some((
                run.session_id.clone()?,
                Started {
                    run: run.launch.id.clone(),
                    work_item_url: run.launch.detail.item.url.clone(),
                    work_item_title: scrubber.scrub(&run.launch.detail.item.title),
                    state: run.state.clone(),
                },
            ))
        })
        .collect();
    let mut native = Listing::merge(parts, crate::sessions::MAX_LIMIT);
    let listed: Vec<String> = native
        .sessions
        .iter()
        .map(|s| s.session_id.clone())
        .collect();
    let recorded: Vec<Summary> = runs
        .iter()
        .filter(|run| {
            run.session_id
                .as_ref()
                .is_some_and(|id| !listed.contains(id))
        })
        .map(|run| {
            let updated = run
                .finished_at
                .clone()
                .or_else(|| run.started_at.clone())
                .unwrap_or_else(|| run.created_at.clone());
            let at = |t: &str| {
                chrono::DateTime::parse_from_rfc3339(t)
                    .map(|t| {
                        t.with_timezone(&chrono::Utc)
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                    })
                    .unwrap_or_default()
            };
            Summary {
                provider: run.launch.request.provider.clone(),
                session_id: run.session_id.clone().unwrap_or_default(),
                machine: names
                    .iter()
                    .find(|(id, _)| id == &run.launch.request.machine)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_default(),
                project: String::new(),
                started_at: at(run.started_at.as_deref().unwrap_or(&run.created_at)),
                updated_at: at(&updated),
                asked: scrubber.scrub(&format!(
                    "{} «{}»",
                    if run.launch.planning {
                        "Plan the children of"
                    } else {
                        "Implement"
                    },
                    run.launch.detail.item.title
                )),
                latest: String::new(),
                concluded: scrubber.scrub(tail(&run.output, 2000)),
                open: if matches!(run.state.as_str(), "running" | "planning") {
                    "The run is still working.".into()
                } else {
                    String::new()
                },
                started_by: None,
            }
        })
        .collect();
    native.sessions.extend(recorded);
    let mut listing = Listing::merge([native], limit);
    listing.unavailable.extend(unavailable);
    listing.attribute(&started);
    listing
}
/// The last `max` bytes of `text`, at a character boundary.
fn tail(text: &str, max: usize) -> &str {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// This computer's own sessions, when the person this host acts for owns the computer: a
/// shared account on another person's server never reads that person's agents.
pub(super) async fn own(host: &Arc<Host>, limit: usize) -> Result<Listing> {
    if !authority::invocation(&host.state)?.scope.owner {
        return Ok(Listing::default());
    }
    let name = host
        .state
        .implementation
        .registry
        .lock()
        .await
        .settings
        .name
        .clone();
    Ok(tokio::task::spawn_blocking(move || {
        crate::sessions::local(&crate::sessions::Roots::from_env(), limit, &name)
    })
    .await?)
}
/// `agent_sessions_local`, which the coordinator asks of a machine.
pub(super) async fn local(host: &Arc<Host>, args: &Value) -> Result<Value> {
    Ok(serde_json::to_value(own(host, limit(args)?).await?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_machines_are_unavailable_and_runs_keep_their_work_item() {
        let summary = |id: &str, at: &str| Summary {
            provider: "claude".into(),
            session_id: id.into(),
            machine: "laptop".into(),
            started_at: at.into(),
            updated_at: at.into(),
            asked: "Fix it".into(),
            ..Default::default()
        };
        let run: Run = serde_json::from_value(json!({"launch":{"id":"run-1","request":{"machine":"m-desk","item":{"provider":"azure","id":7,"project":"Payments","source":"azure","task":true},"context":"Synthetic task","directories":["/work"],"provider":"codex","model":"test-model","effort":"high"},"detail":{"item":{"id":7,"title":"Retry payments","url":"https://dev.azure.com/org/proj/_workitems/edit/7","project":"Payments","state":"Active","kind":"Task","created_at":null,"dates":[]},"fields":[],"comments":[],"relations":[],"history":[],"partial":false,"warnings":[]},"language":"en","planning":false},"state":"running","created_at":"2026-11-12T09:00:00Z","finished_at":null,"output":"Opened the branch; the pipeline is still running.","plan":null,"session_id":"codex-native","started_at":"2026-11-12T09:01:00Z"}))
        .unwrap();
        let listing = combine(
            vec![Listing {
                sessions: vec![summary("claude-1", "2026-11-12T10:00:00Z")],
                unavailable: vec![],
            }],
            vec![Unavailable {
                machine: "desk".into(),
                provider: String::new(),
                reason: "offline".into(),
            }],
            &[run],
            &[("m-desk".into(), "desk".into())],
            10,
        );
        assert_eq!(
            listing.unavailable,
            [Unavailable {
                machine: "desk".into(),
                provider: String::new(),
                reason: "offline".into()
            }]
        );
        assert_eq!(listing.sessions.len(), 2);
        let recorded = &listing.sessions[1];
        assert_eq!(recorded.session_id, "codex-native");
        assert_eq!(recorded.machine, "desk");
        assert_eq!(recorded.asked, "Implement «Retry payments»");
        assert_eq!(recorded.open, "The run is still working.");
        let started = recorded.started_by.as_ref().unwrap();
        assert_eq!(
            started.work_item_url,
            "https://dev.azure.com/org/proj/_workitems/edit/7"
        );
        assert_eq!(started.run, "run-1");
    }
}
