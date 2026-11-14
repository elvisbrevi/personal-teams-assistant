//! The account's recent coding-agent sessions, for the agent sessions source
//! (`crate::sessions`). Each machine summarizes its own; the coordinator asks every online
//! machine of the account through the machine channel and adds the sessions PTA started, with
//! their work items; another installation asks the coordinator. A machine that is offline, too
//! old or does not answer is reported unavailable, never as having no sessions.

use super::*;
use crate::sessions::{Listing, Started, Unavailable};

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

/// On the coordinator: its own sessions and every online machine's, named with the runs PTA
/// recorded for them.
pub(super) async fn gather(host: &Arc<Host>, limit: usize) -> Result<Listing> {
    let version = management::version(host)?;
    let (own_name, machines) = {
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
        (data.settings.name.clone(), machines)
    };
    let mut parts = vec![own(host, limit).await?];
    let mut unavailable = Vec::new();
    let mut asks = tokio::task::JoinSet::new();
    let mut asked_of = std::collections::HashMap::new();
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
        let task = asks.spawn(async move {
            // Bounded inside the call, so a machine that does not answer leaves nothing pending.
            fleet::call_within(
                &host,
                &id,
                "agent_sessions_local",
                json!({"limit":limit}),
                Duration::from_secs(crate::sessions::READ_SECONDS - 5),
            )
            .await
        });
        asked_of.insert(task.id(), name);
    }
    let (answered, silent) = answers(asks, asked_of).await;
    parts.extend(answered);
    unavailable.extend(silent);
    let (runs, gap) = runs_or_gap(
        host.state.implementation.db.list(None, None, false),
        own_name,
    );
    unavailable.extend(gap);
    Ok(combine(parts, unavailable, &runs, limit))
}
/// The machines' answers, each named by the coordinator; a machine whose answer failed, was
/// not a listing or whose task ended abnormally is `no_answer`, never left out.
async fn answers(
    mut asks: tokio::task::JoinSet<Result<Value>>,
    mut asked_of: std::collections::HashMap<tokio::task::Id, String>,
) -> (Vec<Listing>, Vec<Unavailable>) {
    let (mut parts, mut unavailable) = (Vec::new(), Vec::new());
    while let Some(joined) = asks.join_next_with_id().await {
        let (task, answer) = match joined {
            Ok((task, answer)) => (task, answer.ok()),
            Err(error) => (error.id(), None),
        };
        let name = asked_of.remove(&task).unwrap_or_default();
        match answer.and_then(|v| serde_json::from_value::<Listing>(v).ok()) {
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
    (parts, unavailable)
}
/// PTA's recorded runs; a records database that cannot be read is reported, not taken as none.
fn runs_or_gap(runs: Result<Vec<Run>>, machine: String) -> (Vec<Run>, Option<Unavailable>) {
    match runs {
        Ok(runs) => (runs, None),
        Err(_) => (
            Vec::new(),
            Some(Unavailable {
                machine,
                provider: "pta".into(),
                reason: "unreadable".into(),
            }),
        ),
    }
}

/// The machines' listings as one, newest first, each session PTA started named with its run
/// and work item. A run is never listed from its own record: its session is read only from the
/// machine that holds it, which leaves out a private one and is not asked when offline.
fn combine(
    parts: Vec<Listing>,
    unavailable: Vec<Unavailable>,
    runs: &[Run],
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
    let mut listing = Listing::merge(parts, limit);
    listing.unavailable.extend(unavailable);
    listing.attribute(&started);
    listing
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
    use crate::sessions::Summary;

    fn run(session: &str) -> Run {
        let mut run: Run = serde_json::from_value(json!({"launch":{"id":"run-1","request":{"machine":"m-desk","item":{"provider":"azure","id":7,"project":"Payments","source":"azure","task":true},"context":"Synthetic task","directories":["/work"],"provider":"codex","model":"test-model","effort":"high"},"detail":{"item":{"id":7,"title":"Retry payments","url":"https://dev.azure.com/org/proj/_workitems/edit/7","project":"Payments","state":"Active","kind":"Task","created_at":null,"dates":[]},"fields":[],"comments":[],"relations":[],"history":[],"partial":false,"warnings":[]},"language":"en","planning":false},"state":"running","created_at":"2026-11-12T09:00:00Z","finished_at":null,"output":"Opened the branch; the pipeline is still running.","plan":null,"session_id":"x","started_at":"2026-11-12T09:01:00Z"}))
        .unwrap();
        run.session_id = Some(session.into());
        run
    }
    fn summary(id: &str, at: &str) -> Summary {
        Summary {
            provider: "codex".into(),
            session_id: id.into(),
            machine: "laptop".into(),
            started_at: at.into(),
            updated_at: at.into(),
            asked: "Fix it".into(),
            ..Default::default()
        }
    }

    #[test]
    fn runs_name_their_listed_sessions_and_never_add_one_the_machine_left_out() {
        let offline = Unavailable {
            machine: "desk".into(),
            provider: String::new(),
            reason: "offline".into(),
        };
        // "private-run" is a PTA run whose session its machine left out as private (or did
        // not list); its record must not bring it back with its output.
        let listing = combine(
            vec![Listing {
                sessions: vec![summary("codex-native", "2026-11-12T10:00:00Z")],
                unavailable: vec![],
            }],
            vec![offline.clone()],
            &[run("codex-native"), run("private-run")],
            10,
        );
        assert_eq!(listing.unavailable, [offline]);
        assert_eq!(listing.sessions.len(), 1);
        let started = listing.sessions[0].started_by.as_ref().unwrap();
        assert_eq!(
            (started.run.as_str(), started.work_item_url.as_str()),
            ("run-1", "https://dev.azure.com/org/proj/_workitems/edit/7")
        );
        let text = serde_json::to_string(&listing).unwrap();
        assert!(!text.contains("private-run") && !text.contains("Opened the branch"));
    }

    #[tokio::test]
    async fn a_failed_or_panicked_machine_answer_is_no_answer_and_the_runs_database_is_reported() {
        let mut asks = tokio::task::JoinSet::new();
        let mut asked_of = std::collections::HashMap::new();
        let answered = asks.spawn(async {
            Ok(json!({"sessions":[summary("a", "2026-11-12T10:00:00Z")],"unavailable":[]}))
        });
        asked_of.insert(answered.id(), "laptop".to_string());
        let failed = asks.spawn(async { Err(anyhow::anyhow!("refused")) });
        asked_of.insert(failed.id(), "desk".to_string());
        let panicked = asks.spawn(async { panic!("synthetic panic") });
        asked_of.insert(panicked.id(), "build".to_string());
        let (parts, mut gaps) = answers(asks, asked_of).await;
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].sessions[0].machine, "laptop");
        gaps.sort_by(|a, b| a.machine.cmp(&b.machine));
        let named: Vec<_> = gaps
            .iter()
            .map(|g| (g.machine.as_str(), g.reason.as_str()))
            .collect();
        assert_eq!(named, [("build", "no_answer"), ("desk", "no_answer")]);
        let (runs, gap) = runs_or_gap(Err(anyhow::anyhow!("locked")), "coordinator".into());
        assert!(runs.is_empty());
        assert_eq!(gap.unwrap().reason, "unreadable");
    }

    #[tokio::test(start_paused = true)]
    async fn a_machine_that_does_not_answer_leaves_no_pending_call() {
        let root = tempfile::tempdir().unwrap();
        let host = super::super::tests::host(root.path());
        let id = uuid::Uuid::new_v4().to_string();
        fleet::register(
            &host,
            &json!({"id":id,"name":"Worker","os":"linux","account_version":"synthetic"}),
        )
        .await
        .unwrap();
        let version = management::version(&host).unwrap();
        {
            let mut data = host.state.implementation.registry.lock().await;
            let machine = data.machines.iter_mut().find(|m| m.id == id).unwrap();
            machine.account_version = version;
            machine.seen = now();
            machine.agent_sessions = true;
        }
        let pending = || async { host.state.implementation.calls.lock().await.len() };
        // A bounded wait that ends without an answer removes its call.
        let bounded = fleet::call_within(
            &host,
            &id,
            "agent_sessions_local",
            json!({"limit":10}),
            Duration::from_secs(2),
        )
        .await;
        assert!(bounded.is_err());
        assert_eq!(pending().await, 0);
        // So does a caller that stops waiting, many times over: the queue never fills.
        for _ in 0..70 {
            let gave_up = tokio::time::timeout(
                Duration::from_secs(1),
                fleet::call(&host, &id, "agent_sessions_local", json!({"limit":10})),
            )
            .await;
            assert!(gave_up.is_err());
        }
        tokio::task::yield_now().await;
        assert_eq!(pending().await, 0);
    }
}
