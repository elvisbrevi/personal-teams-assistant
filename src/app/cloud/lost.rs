//! A lost or stolen computer (phase 6 of the cloud storage design): one confirmed action, on the
//! account's coordinator, that takes away what that computer could still use. It revokes the
//! machine here and its installation in the account store, replaces the service token with the
//! approved launcher `cloud-token-rotate` (Access rotates the secret, so the copy that computer
//! keeps stops working at once) and rotates the content key (what is sealed from then on does not
//! open there). The account's other computers receive the new token and key from this coordinator,
//! announced in their next poll. Each step's outcome is recorded in `cloud/lost.json`, so repeating
//! the call for the same computer continues where it stopped and never runs a finished step again.
use super::*;

const FILE: &str = "lost.json";
const STEPS: [&str; 4] = ["machine", "store", "token", "key"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LostArgs {
    /// The computer's installation in the account store (`cloud installations`).
    #[serde(default)]
    installation: Option<String>,
    /// Or its machine ID on this coordinator (`machine_list`).
    #[serde(default)]
    machine: Option<String>,
}

/// The last lost computer secured from here, in `cloud/lost.json` (private).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Record {
    /// Its machine ID on this coordinator (empty: none).
    pub machine: String,
    /// Its installation in the account store (empty: none).
    pub installation: String,
    pub name: String,
    pub started_at: i64,
    /// When every step was done or not needed (0: not yet).
    pub finished_at: i64,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Step {
    pub step: String,
    /// `done`, `not_needed`, `pending` (waits for the user or the portal operator) or `failed`.
    pub state: String,
    pub at: i64,
    pub detail: String,
}

impl Record {
    fn settled(&self, step: &str) -> bool {
        self.steps
            .iter()
            .any(|s| s.step == step && matches!(s.state.as_str(), "done" | "not_needed"))
    }
    fn set(&mut self, step: &str, state: &str, detail: impl Into<String>) {
        self.steps.retain(|s| s.step != step);
        self.steps.push(Step {
            step: step.into(),
            state: state.into(),
            at: now(),
            detail: detail.into(),
        });
        self.steps
            .sort_by_key(|s| STEPS.iter().position(|n| *n == s.step));
    }
    fn complete(&self) -> bool {
        STEPS.iter().all(|step| self.settled(step))
    }
}

fn path(host: &Host) -> std::path::PathBuf {
    host.state.settings.profile().join("cloud").join(FILE)
}

pub(super) fn last(host: &Host) -> Option<Record> {
    std::fs::read_to_string(path(host))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

fn remember(host: &Host, record: &Record) {
    let target = path(host);
    if let Some(parent) = target.parent() {
        let _ = security::private_dir(parent);
    }
    let _ = write_private(&target, &serde_json::to_string(record).unwrap_or_default());
}

/// What `cloud_token` and the reply show of a record.
pub(super) fn view(record: &Record) -> Value {
    json!({
        "name": record.name,
        "machine": (!record.machine.is_empty()).then_some(&record.machine),
        "installation": (!record.installation.is_empty()).then_some(&record.installation),
        "started_at": record.started_at,
        "finished_at": (record.finished_at > 0).then_some(record.finished_at),
        "complete": record.complete(),
        "steps": record.steps,
    })
}

/// One run at a time per profile: a second call while one runs is refused, not queued.
static RUNNING: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
> = std::sync::LazyLock::new(Default::default);

/// `cloud_lost`: secure the account after a lost or stolen computer, step by step.
pub(super) async fn operate(host: &Arc<Host>, args: LostArgs) -> Reply {
    let invalid = |message: &str| Reply::error("invalid_input", 2, message);
    let given = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let (machine, installation) = (given(&args.machine), given(&args.installation));
    if machine.is_some() == installation.is_some() {
        return invalid(
            "Give the lost computer's installation (cloud installations) or its machine ID, not both.",
        );
    }
    if [&machine, &installation]
        .into_iter()
        .flatten()
        .any(|id| uuid::Uuid::parse_str(id).is_err())
    {
        return invalid("Give the lost computer's installation or machine ID as listed.");
    }
    if crate::app::implementation::link(&host.state)
        .ok()
        .flatten()
        .is_some()
    {
        return Reply::error(
            "not_ready",
            3,
            "Do this on the account's coordinator, which revokes the computer and gives the other computers the new service token and content key.",
        );
    }
    let Ok(config) = host.state.settings.config() else {
        return Reply::error(
            "operation_failed",
            1,
            "The configuration could not be read.",
        );
    };
    let connected = enrolled(&config.cloud);
    if installation
        .as_deref()
        .is_some_and(|id| id == config.cloud.installation)
    {
        return invalid("That is this computer: it cannot secure itself.");
    }
    let known = match crate::app::implementation::known_machine(
        host,
        machine.as_deref(),
        installation.as_deref(),
    )
    .await
    {
        Ok(known) => known,
        Err(()) => return invalid("That is this computer: it cannot secure itself."),
    };
    if machine.is_some() && known.is_none() {
        return invalid("This coordinator has no such machine.");
    }
    if known.is_none() && !connected {
        return invalid(
            "This computer is not connected to the account store, so it does not know that installation.",
        );
    }
    let profile = host.state.settings.profile().to_path_buf();
    if !RUNNING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(profile.clone())
    {
        return Reply::error(
            "state_conflict",
            6,
            "A lost computer is already being secured here; wait for it to finish.",
        );
    }
    let reply = run(host, known, installation, &config).await;
    RUNNING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&profile);
    reply
}

async fn run(
    host: &Arc<Host>,
    known: Option<crate::app::implementation::KnownMachine>,
    installation: Option<String>,
    config: &crate::config::Config,
) -> Reply {
    let connected = enrolled(&config.cloud);
    let machine = known.as_ref().map(|m| m.id.clone()).unwrap_or_default();
    let installation = installation
        .or_else(|| known.as_ref().map(|m| m.store_installation.clone()))
        .unwrap_or_default();
    let name = known
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| installation.clone());
    // The same computer again continues its record; another one starts a new record.
    let mut record = last(host)
        .filter(|r| r.machine == machine && r.installation == installation && r.finished_at == 0)
        .unwrap_or_else(|| Record {
            machine: machine.clone(),
            installation: installation.clone(),
            name: name.clone(),
            started_at: now(),
            ..Record::default()
        });
    remember(host, &record);

    // 1. The machine on this coordinator: its credential stops working here.
    if !record.settled("machine") {
        match &known {
            None => record.set("machine", "not_needed", "No machine of this coordinator."),
            Some(m) if m.revoked => record.set("machine", "done", "It was already revoked."),
            Some(m) => match crate::app::implementation::revoke_machine(host, &m.id).await {
                Ok(_) => record.set("machine", "done", "Revoked on this coordinator."),
                Err(_) => {
                    record.set(
                        "machine",
                        "failed",
                        "The machine could not be revoked here.",
                    );
                    return finish(host, record);
                }
            },
        }
        remember(host, &record);
    }
    // 2. Its installation in the account store: its own credential stops working there.
    if !record.settled("store") {
        if installation.is_empty() || !connected {
            record.set(
                "store",
                "not_needed",
                "It had no installation in the account store.",
            );
        } else {
            let revoked = revoke_installation(host, &installation).await;
            if revoked["revoked"] == true {
                record.set("store", "done", "Revoked in the account store.");
            } else {
                record.set(
                    "store",
                    "failed",
                    revoked["problem"]
                        .as_str()
                        .unwrap_or("The account store did not answer."),
                );
                return finish(host, record);
            }
        }
        remember(host, &record);
    }
    // 3. The service token: it could still enroll again with the one it keeps.
    if !record.settled("token") {
        token(host, &mut record, connected).await;
        remember(host, &record);
    }
    // 4. The content key: what is sealed from now on does not open there.
    if !record.settled("key") {
        if !connected || config.cloud.mode == CloudMode::Off {
            record.set(
                "key",
                "not_needed",
                "This computer keeps no copy in the account store.",
            );
        } else {
            let rotated = migration::operate_rotate(host).await;
            if rotated.ok {
                record.set(
                    "key",
                    "done",
                    format!(
                        "Content key version {} is active; {} records are being sealed again.",
                        rotated.data["version"], rotated.data["resealing"]
                    ),
                );
            } else {
                record.set(
                    "key",
                    "failed",
                    rotated
                        .message
                        .unwrap_or_else(|| "The content key could not be rotated.".into()),
                );
            }
        }
    }
    finish(host, record)
}

/// Replace the service token by running the approved launcher, or say who does it.
async fn token(host: &Arc<Host>, record: &mut Record, connected: bool) {
    if !connected {
        record.set(
            "token",
            "not_needed",
            "This computer is not connected to the account store.",
        );
        return;
    }
    if host.state.authority.shared {
        record.set(
            "token",
            "pending",
            "Ask the portal operator to replace this account's service token (configure-cloud-access.py production --rotate --portal-user USER); then run this again.",
        );
        return;
    }
    let (defined, approved) =
        crate::app::consumers::launcher_state(&host.state, delivery::ROTATE_LAUNCHER);
    // Launchers run only from this machine's window or local CLI (not from the portal).
    let runs_here = authority::invocation(&host.state).is_ok_and(|invocation| {
        invocation.scope.owner
            && matches!(
                invocation.transport,
                authority::Transport::Local | authority::Transport::Window
            )
    });
    if approved && !runs_here {
        record.set(
            "token",
            "pending",
            "Launchers run only from the coordinator's own local page or local CLI: run pta cloud lost again there (or pta variables launcher run cloud-token-rotate --confirmed).",
        );
        return;
    }
    if !approved {
        record.set(
            "token",
            "pending",
            if defined {
                "Approve the launcher cloud-token-rotate in Settings › Variables › Local programs, then run this again."
            } else {
                "Define the launcher cloud-token-rotate (configure-cloud-access.py production --rotate) and approve it in Settings › Variables › Local programs, then run this again."
            },
        );
        return;
    }
    let mut request = control::Request::new("variable_launcher_run");
    request.args = json!({"id": delivery::ROTATE_LAUNCHER});
    let ran = crate::app::consumers::operate(host, &request).await;
    if ran.ok && ran.data["result"]["status"] == "succeeded" {
        record.set(
            "token",
            "done",
            "The service token's secret was replaced in Cloudflare Access.",
        );
    } else {
        record.set(
            "token",
            "failed",
            ran.message.unwrap_or_else(|| {
                "The launcher did not finish; the token may or may not be replaced. Check `pta cloud token`, then run this again.".into()
            }),
        );
    }
}

fn finish(host: &Host, mut record: Record) -> Reply {
    if record.complete() {
        record.finished_at = now();
    }
    remember(host, &record);
    let complete = record.complete();
    let next = if complete {
        "Back up the new content key now (pta variables key backup). The account's other computers receive the new service token and content key from this coordinator within a minute; pta cloud token lists them."
    } else {
        "Do what the steps that are not done say, then run this again for the same computer: finished steps are not repeated."
    };
    let mut data = view(&record);
    data["next"] = json!(next);
    Reply::success(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_stay_in_order_and_settle_only_when_done_or_not_needed() {
        let mut record = Record::default();
        record.set("key", "failed", "x");
        record.set("machine", "done", "x");
        record.set("token", "pending", "x");
        record.set("store", "not_needed", "x");
        let order: Vec<_> = record.steps.iter().map(|s| s.step.as_str()).collect();
        assert_eq!(order, STEPS);
        assert!(record.settled("machine") && record.settled("store"));
        assert!(!record.settled("token") && !record.settled("key") && !record.complete());
        record.set("token", "done", "x");
        record.set("key", "done", "x");
        assert!(record.complete());
        assert_eq!(record.steps.len(), 4);
    }
}
