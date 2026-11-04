//! Keeping the account's other computers' account store credentials current (phase 5 of the cloud
//! storage design): a computer trusted with the account store keeps the key pair its sign-in
//! started with (`mirror::handoff::Receiver::machine`) and the coordinator records the public key
//! the person approved. When the coordinator's service token or content key changes (the token's
//! secret rotated in Access, the key rotated with `cloud_rotate_key`), each such computer asks over
//! the machine protocol (`store_delivery`, telling only fingerprints and versions) and receives the
//! same bundle as at trust time, sealed to that key: the portal, the tunnel and Cloudflare carry
//! only ciphertext, and a revoked machine is refused by the machine protocol before anything is
//! sealed for it. `cloud_token` shows, on the coordinator, which computers hold the current
//! credentials and how to replace the service token.
use super::*;
use crate::mirror::{handoff, key};

/// How often a computer asks while nothing tells it to ask sooner.
const EVERY_SECONDS: i64 = 600;
/// The launcher that replaces the account's service token by rotating its secret in Access
/// (`scripts/configure-cloud-access.py production --rotate`), named by convention.
pub(super) const ROTATE_LAUNCHER: &str = "cloud-token-rotate";
const FILE: &str = "delivery.json";

/// What this computer last learned from its coordinator, in `cloud/delivery.json` (private).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Last {
    pub checked_at: i64,
    /// `current`, `installed`, `retrust`, `unavailable`, `failed` or `unsupported`.
    pub state: String,
    pub problem: String,
    /// The newest bundle installed here (issue time on the coordinator), so an older one replayed
    /// on the way is never installed over it.
    pub issued_at: i64,
    pub installed_at: i64,
    /// The content key version the coordinator last said is active (0: not known yet).
    pub coordinator_key: u32,
}

fn path(host: &Host) -> std::path::PathBuf {
    host.state.settings.profile().join("cloud").join(FILE)
}

pub(super) fn last(host: &Host) -> Last {
    std::fs::read_to_string(path(host))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn remember(host: &Host, value: &Last) {
    let target = path(host);
    if let Some(parent) = target.parent() {
        let _ = security::private_dir(parent);
    }
    let _ = write_private(&target, &serde_json::to_string(value).unwrap_or_default());
}

fn keys(host: &Host) -> std::path::PathBuf {
    host.state.settings.profile().join("keys")
}

/// The account store credentials an installation holds, as fingerprints: its service token's and
/// its content key's active version (0 without one). `None` without a service token.
pub(super) fn held(host: &Host) -> Option<(String, u32)> {
    let credentials = stored_credentials()?;
    let active = key::load(&keys(host))
        .ok()
        .flatten()
        .map(|content| content.active)
        .unwrap_or(0);
    Some((
        handoff::token_fingerprint(&credentials.client_id, &credentials.client_secret),
        active,
    ))
}

/// On the coordinator, answering a machine's `store_delivery`: `current` when it holds this
/// service token and this content key (or a newer one); otherwise the account store sealed to the
/// key the person approved for it (`sealed`, with `issued_at`), or why not: `retrust` (it was
/// trusted without a key this coordinator recorded) or `unavailable`. Every answer but
/// `unavailable` names the active content key version (`key`).
pub(in crate::app) fn deliver(
    host: &Host,
    machine: &str,
    recipient: &str,
    token: &str,
    key: u32,
) -> Value {
    let enrolled_here = host
        .state
        .settings
        .config()
        .is_ok_and(|config| enrolled(&config.cloud));
    let current = held(host).filter(|_| enrolled_here);
    let active = current.as_ref().map(|(_, active)| *active).unwrap_or(0);
    match verdict(
        current
            .as_ref()
            .map(|(token, active)| (token.as_str(), *active)),
        recipient,
        token,
        key,
    ) {
        Verdict::Unavailable => {
            return json!({"state": "unavailable", "problem": "The coordinator is not connected to the account store."});
        }
        Verdict::Current => return json!({"state": "current", "key": active}),
        Verdict::Retrust => return json!({"state": "retrust", "key": active}),
        Verdict::Seal => {}
    }
    let issued_at = now();
    let mut reply = handoff(host, recipient, &context(machine, issued_at));
    if reply.get("sealed").is_some() {
        reply["state"] = json!("sealed");
        reply["issued_at"] = json!(issued_at);
        reply["key"] = json!(active);
    } else {
        reply["problem"] = reply["unavailable"].clone();
        reply["state"] = json!("unavailable");
    }
    reply
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Unavailable,
    Current,
    Retrust,
    Seal,
}

/// What the coordinator answers a machine holding `token` and content key version `key`, given
/// its own (`None`: it has no account store) and the key recorded for the machine (empty: none).
fn verdict(current: Option<(&str, u32)>, recipient: &str, token: &str, key: u32) -> Verdict {
    match current {
        None => Verdict::Unavailable,
        Some((current, active)) if current == token && key >= active => Verdict::Current,
        Some(_) if recipient.is_empty() => Verdict::Retrust,
        Some(_) => Verdict::Seal,
    }
}

/// What a delivery is bound to: the machine it is for and when it was issued.
fn context(machine: &str, issued_at: i64) -> String {
    format!("store-delivery:{machine}:{issued_at}")
}

/// On a computer signed in to a coordinator and connected to the account store: ask whether its
/// credentials are current, at most every ten minutes unless `now` (the store refused this
/// computer's service token), and install what the coordinator sealed for it.
pub(in crate::app) async fn refresh(host: &Arc<Host>, soon: bool) {
    let Ok(Some(link)) = crate::app::implementation::link(&host.state) else {
        return;
    };
    let Ok(config) = host.state.settings.config() else {
        return;
    };
    if !enrolled(&config.cloud) {
        return;
    }
    let previous = last(host);
    if !soon && now() - previous.checked_at < EVERY_SECONDS {
        return;
    }
    let Some((token, key)) = held(host) else {
        return;
    };
    let next = match crate::app::implementation::fleet::store_delivery(
        host,
        json!({"token": token, "key": key}),
    )
    .await
    {
        Ok(reply) => answer(host, &link.machine, &previous, &reply, |name, value| {
            security::put_desktop_secret("default", name, value)
        }),
        // An older coordinator does not know the action, or did not answer.
        Err(error) => Last {
            state: if error.to_string().contains("Machine operation failed") {
                "unsupported".into()
            } else {
                "failed".into()
            },
            problem: "The coordinator could not be asked for the account store's credentials."
                .into(),
            ..previous.clone()
        },
    };
    remember(
        host,
        &Last {
            checked_at: now(),
            ..next
        },
    );
}

fn answer(
    host: &Host,
    machine: &str,
    previous: &Last,
    reply: &Value,
    store: impl Fn(&str, &str) -> Result<()>,
) -> Last {
    let coordinator_key = reply["key"]
        .as_u64()
        .and_then(|key| u32::try_from(key).ok())
        .unwrap_or(previous.coordinator_key);
    Last {
        coordinator_key,
        ..outcome(host, machine, previous, reply, store)
    }
}

fn outcome(
    host: &Host,
    machine: &str,
    previous: &Last,
    reply: &Value,
    store: impl Fn(&str, &str) -> Result<()>,
) -> Last {
    let state = reply["state"].as_str().unwrap_or_default();
    let keep = |state: &str, problem: &str| Last {
        state: state.into(),
        problem: problem.into(),
        ..previous.clone()
    };
    match state {
        "current" => keep("current", ""),
        "retrust" => keep(
            "retrust",
            "This computer was trusted before it could receive the account store's new credentials: trust it again with the account store.",
        ),
        "sealed" => {
            let issued_at = reply["issued_at"].as_i64().unwrap_or(0);
            if issued_at <= previous.issued_at {
                return keep(
                    "failed",
                    "The coordinator sent an older account store delivery.",
                );
            }
            let Ok(sealed) = serde_json::from_value::<handoff::Sealed>(reply["sealed"].clone())
            else {
                return keep("failed", "The coordinator sent an unreadable delivery.");
            };
            let keys = keys(host);
            let opened = (handoff::Receiver::kept(&keys))
                .then(|| handoff::Receiver::machine(&keys).ok())
                .flatten()
                .and_then(|receiver| receiver.open(&context(machine, issued_at), &sealed).ok());
            let Some(plain) = opened else {
                return keep(
                    "failed",
                    "The account store delivery could not be opened on this computer.",
                );
            };
            let installed = install(host, &plain, store);
            if installed["installed"] == true {
                crate::mirror::wake();
                Last {
                    state: "installed".into(),
                    problem: String::new(),
                    issued_at,
                    installed_at: now(),
                    checked_at: 0,
                    coordinator_key: 0,
                }
            } else {
                keep("failed", installed["problem"].as_str().unwrap_or_default())
            }
        }
        _ => keep(
            "unavailable",
            reply["problem"]
                .as_str()
                .unwrap_or("The coordinator has no account store to hand over."),
        ),
    }
}

/// Install a delivery this computer opened: the same account store only; a new service token goes
/// to the credential store (and its expiry to `[cloud]`), and the content key replaces this
/// computer's only when it is the same account's with every version kept (`key::update`, a
/// rotation). Nothing else changes: the enrollment and the mode stay.
fn install(host: &Host, plain: &[u8], store: impl Fn(&str, &str) -> Result<()>) -> Value {
    let Ok(bundle) = serde_json::from_slice::<Handoff>(plain) else {
        return json!({"installed": false, "problem": "The coordinator sent an unreadable delivery."});
    };
    if Cloud::check_endpoint(&bundle.endpoint).is_err()
        || !valid_client_id(&bundle.client_id)
        || !valid_client_secret(&bundle.client_secret)
    {
        return json!({"installed": false, "problem": "The coordinator sent an invalid delivery."});
    }
    let Ok(config) = host.state.settings.config() else {
        return json!({"installed": false, "problem": "The configuration could not be read."});
    };
    if origin(&config.cloud.endpoint) != origin(&bundle.endpoint) {
        return json!({"installed": false, "problem": "The coordinator uses another account store than this computer."});
    }
    let key_written = match key::update(&keys(host), &bundle.content_key) {
        Ok(written) => written,
        Err(error) => return json!({"installed": false, "problem": error.to_string()}),
    };
    let replaced = stored_credentials().is_none_or(|current| {
        handoff::token_fingerprint(&current.client_id, &current.client_secret)
            != handoff::token_fingerprint(&bundle.client_id, &bundle.client_secret)
    });
    if replaced {
        for (name, value) in [
            (ACCESS_ID, bundle.client_id.as_str()),
            (ACCESS_SECRET, bundle.client_secret.as_str()),
        ] {
            if store(name, value).is_err() {
                return json!({"installed": false, "problem": "The credential store refused the account store credentials."});
            }
        }
        if bundle.token_expires_at > 0 && bundle.token_expires_at != config.cloud.token_expires_at {
            let mut config = config;
            config.cloud.token_expires_at = bundle.token_expires_at;
            if host.state.settings.save_config(&config).is_err() {
                return json!({"installed": false, "problem": "The configuration could not be saved."});
            }
        }
    }
    json!({"installed": true, "token_replaced": replaced, "key_written": key_written})
}

pub(in crate::app) const KEY_BEHIND: &str = "This computer does not have the account's current content key yet; it receives it from the coordinator within minutes.";

/// What `cloud_status` says about this computer's deliveries, when it is signed in to a
/// coordinator; `None` otherwise.
pub(super) fn describe(host: &Host) -> Option<Value> {
    crate::app::implementation::link(&host.state)
        .ok()
        .flatten()?;
    let last = last(host);
    let active = key::load(&keys(host))
        .ok()
        .flatten()
        .map(|content| content.active)
        .unwrap_or(0);
    // The coordinator's key is newer than this computer's: what it seals since then is unreadable
    // here until the delivery arrives (or the computer is trusted again).
    let behind = last.coordinator_key > active;
    let problem = match (behind, last.problem.is_empty()) {
        (_, false) => Some(last.problem.as_str()),
        (true, true) => Some(KEY_BEHIND),
        (false, true) => None,
    };
    Some(json!({
        "receives": handoff::Receiver::kept(&keys(host)),
        "state": (!last.state.is_empty()).then_some(last.state.as_str()),
        "problem": problem,
        "key": {"here": active, "coordinator": (last.coordinator_key > 0).then_some(last.coordinator_key), "behind": behind},
        "checked_at": (last.checked_at > 0).then_some(last.checked_at),
        "installed_at": (last.installed_at > 0).then_some(last.installed_at),
    }))
}

/// `cloud_token`: the account's service token and how to replace it, with, on the coordinator,
/// which of the account's computers hold the current one. Read-only.
pub(super) async fn operate_token(host: &Arc<Host>) -> Reply {
    let Ok(config) = host.state.settings.config() else {
        return Reply::error(
            "operation_failed",
            1,
            "The configuration could not be read.",
        );
    };
    let signed_in = crate::app::implementation::link(&host.state)
        .ok()
        .flatten()
        .is_some();
    let held = held(host);
    let launcher = crate::app::consumers::launcher_state(&host.state, ROTATE_LAUNCHER);
    let mut data = json!({
        "connected": enrolled(&config.cloud),
        "service_token": {
            "stored": held.is_some(),
            "expires_at": (config.cloud.token_expires_at > 0).then_some(config.cloud.token_expires_at),
        },
        "coordinator": !signed_in,
    });
    if signed_in {
        data["delivery"] = describe(host).unwrap_or(Value::Null);
        data["replace"] = json!({
            "where": "coordinator",
            "detail": "The account's coordinator replaces the service token; this computer receives the new one automatically.",
        });
        return Reply::success(data);
    }
    let shared = host.state.authority.shared;
    // Launchers run only from this machine's window or local CLI, for its owner.
    let runs_here = !shared
        && authority::invocation(&host.state).is_ok_and(|invocation| {
            invocation.scope.owner
                && matches!(
                    invocation.transport,
                    authority::Transport::Local | authority::Transport::Window
                )
        });
    data["replace"] = json!({
        "where": if shared { "operator" } else { "here" },
        "launcher": {"id": ROTATE_LAUNCHER, "defined": launcher.0, "approved": launcher.1},
        "runs_here": runs_here,
        "command": format!("pta variables launcher run {ROTATE_LAUNCHER} --confirmed"),
        "detail": if shared {
            "The portal operator replaces this account's service token (configure-cloud-access.py production --rotate --portal-user USER); the account's computers then receive it automatically."
        } else {
            "Run the approved launcher that rotates the service token's secret in Cloudflare Access (configure-cloud-access.py production --rotate): the previous secret stops working at once, this computer stores the new one and the account's other computers receive it automatically."
        },
    });
    let current = held.as_ref().map(|(token, _)| token.clone());
    let active = held.as_ref().map(|(_, active)| *active);
    let machines = crate::app::implementation::store_machines(host).await;
    let computers: Vec<Value> = machines
        .iter()
        .map(|machine| {
            let token = match (&machine.held, &current) {
                (None, _) | (_, None) => "unknown",
                (Some(held), Some(current)) if &held.0 == current => "current",
                (Some(held), _) if held.0.is_empty() => "none",
                _ => "previous",
            };
            let key = match (&machine.held, active) {
                (None, _) | (_, None) => "unknown",
                (Some(held), Some(active)) if held.1 >= active => "current",
                _ => "previous",
            };
            json!({
                "machine": machine.id,
                "name": machine.name,
                "store_installation": (!machine.store_installation.is_empty()).then_some(&machine.store_installation),
                "receives": machine.receives,
                "token": token,
                "key": key,
                "reported_at": machine.held.as_ref().map(|held| held.2),
            })
        })
        .collect();
    data["pending"] = json!(
        computers
            .iter()
            .filter(|c| c["token"] != "current" || c["key"] != "current")
            .count()
    );
    data["computers"] = json!(computers);
    Reply::success(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delivery_is_bound_to_its_machine_and_time() {
        assert_ne!(context("a", 1), context("a", 2));
        assert_ne!(context("a", 1), context("b", 1));
    }

    #[test]
    fn the_coordinator_seals_only_for_a_key_it_recorded() {
        let key = "1".repeat(64);
        assert_eq!(verdict(None, &key, "a", 1), Verdict::Unavailable);
        assert_eq!(verdict(Some(("a", 1)), &key, "a", 1), Verdict::Current);
        assert_eq!(verdict(Some(("a", 1)), "", "a", 1), Verdict::Current);
        assert_eq!(verdict(Some(("b", 1)), "", "a", 1), Verdict::Retrust);
        assert_eq!(verdict(Some(("b", 1)), &key, "a", 1), Verdict::Seal);
        // A rotated content key is sealed for the machine too, with the same token.
        assert_eq!(verdict(Some(("a", 2)), &key, "a", 1), Verdict::Seal);
        assert_eq!(verdict(Some(("a", 2)), "", "a", 1), Verdict::Retrust);
        assert_eq!(verdict(Some(("a", 2)), &key, "a", 3), Verdict::Current);
    }

    fn connected(host: &Host, endpoint: &str) {
        let mut config = host.state.settings.config().unwrap();
        config.cloud = Cloud {
            endpoint: endpoint.into(),
            installation: "00000000-0000-4000-8000-000000000001".into(),
            mode: CloudMode::Mirror,
            token_expires_at: 0,
        };
        host.state.settings.save_config(&config).unwrap();
    }

    fn bundle(endpoint: &str, secret: &str, content: &key::Content) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "endpoint": endpoint,
            "client_id": "synthetic.access",
            "client_secret": secret,
            "token_expires_at": 1_900_000_000,
            "content_key": content.backup(),
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn a_computer_installs_only_a_newer_delivery_sealed_to_its_own_key() {
        let root = tempfile::tempdir().unwrap();
        let host = super::super::tests::host(root.path());
        connected(&host, "https://store.example.invalid");
        let receiver = handoff::Receiver::machine(&keys(&host)).unwrap();
        let content = key::Content::generate();
        let kept = std::sync::Mutex::new(Vec::new());
        let store = |name: &str, value: &str| {
            kept.lock()
                .unwrap()
                .push((name.to_owned(), value.to_owned()));
            Ok(())
        };
        let sealed = |issued: i64, plain: &[u8]| {
            json!({"state": "sealed", "issued_at": issued,
                "sealed": handoff::seal(receiver.public(), &context("machine-1", issued), plain).unwrap()})
        };
        let plain = bundle(
            "https://store.example.invalid",
            "cfast_synthetic_new_secret",
            &content,
        );
        let previous = Last {
            issued_at: 100,
            ..Last::default()
        };
        // An older delivery replayed on the way, or one for another machine, is not installed.
        let replayed = answer(&host, "machine-1", &previous, &sealed(100, &plain), store);
        assert_eq!(replayed.state, "failed");
        let other = answer(&host, "machine-2", &previous, &sealed(200, &plain), store);
        assert_eq!(other.state, "failed");
        assert!(kept.lock().unwrap().is_empty());
        let installed = answer(&host, "machine-1", &previous, &sealed(200, &plain), store);
        assert_eq!(installed.state, "installed", "{installed:?}");
        assert_eq!(installed.issued_at, 200);
        assert_eq!(
            kept.lock().unwrap().as_slice(),
            &[
                (ACCESS_ID.to_owned(), "synthetic.access".to_owned()),
                (
                    ACCESS_SECRET.to_owned(),
                    "cfast_synthetic_new_secret".to_owned()
                )
            ]
        );
        // The key came with it (this computer had none) and the expiry is recorded.
        assert_eq!(
            key::load(&keys(&host)).unwrap().unwrap().backup(),
            content.backup()
        );
        assert_eq!(
            host.state.settings.config().unwrap().cloud.token_expires_at,
            1_900_000_000
        );
        // The coordinator rotated the content key: the same account's set with version 2 replaces it.
        let coordinator = root.path().join("coordinator-keys");
        key::import(&coordinator, &content.backup()).unwrap();
        let rotated = key::rotate(&coordinator).unwrap();
        let again = bundle(
            "https://store.example.invalid",
            "cfast_synthetic_new_secret",
            &rotated,
        );
        let mut previous = installed.clone();
        previous.coordinator_key = 2;
        let installed = answer(
            &host,
            "machine-1",
            &previous,
            &{
                let mut reply = sealed(250, &again);
                reply["key"] = json!(2);
                reply
            },
            store,
        );
        assert_eq!(installed.state, "installed", "{installed:?}");
        assert_eq!(installed.coordinator_key, 2);
        assert_eq!(key::load(&keys(&host)).unwrap().unwrap().active, 2);
        let stored = kept.lock().unwrap().len();
        // Another account store is never installed over this one's.
        let elsewhere = bundle(
            "https://other.example.invalid",
            "cfast_synthetic_other_secret",
            &content,
        );
        let refused = answer(
            &host,
            "machine-1",
            &installed,
            &sealed(300, &elsewhere),
            store,
        );
        assert_eq!(refused.state, "failed");
        assert!(refused.problem.contains("another account store"));
        assert_eq!(kept.lock().unwrap().len(), stored);
    }

    #[test]
    fn the_coordinators_other_answers_are_kept_as_they_are() {
        let root = tempfile::tempdir().unwrap();
        let host = super::super::tests::host(root.path());
        let none = |_: &str, _: &str| -> Result<()> { panic!("nothing is stored") };
        let previous = Last::default();
        assert_eq!(
            answer(&host, "m", &previous, &json!({"state": "current"}), none).state,
            "current"
        );
        let retrust = answer(&host, "m", &previous, &json!({"state": "retrust"}), none);
        assert_eq!(retrust.state, "retrust");
        assert!(retrust.problem.contains("trust it again"));
        assert_eq!(
            answer(
                &host,
                "m",
                &previous,
                &json!({"state": "unavailable"}),
                none
            )
            .state,
            "unavailable"
        );
    }
}
