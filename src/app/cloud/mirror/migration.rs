//! Phase 4a: moving an account into the account store and back (design §8). `cloud_migrate`
//! takes a private backup of the profile's data and then uploads, verifies by reading back and
//! switches to mirror; `cloud_rollback` brings the configuration back when the store is its
//! source, verifies and switches to off, leaving the copy in place; `cloud_erase` removes that
//! copy from the Worker once rolled back. None of them deletes a local store.

use super::*;
use crate::maintenance::apply::{self as safety, Profile};

/// Newest `backups/cloud-*` directories kept.
const BACKUPS_KEPT: usize = 3;
/// Erasure requests in one `cloud_erase` (each removes up to 1,000 records).
const ERASE_ROUNDS: usize = 200;

/// What stays on this installation whatever the mode, and why.
const STAYS_LOCAL: [(&str, &str); 7] = [
    (
        "Secrets, credentials and the [secrets] and [cloud] sections",
        "they never leave this installation",
    ),
    (
        "The Microsoft sign-in, provider sessions and send nonces",
        "they belong to this computer and a send is never replayed",
    ),
    (
        "Messages, activity runs, implementation runs and requests still in progress",
        "only finished records are copied",
    ),
    (
        "Simulations and the local chat",
        "they are not part of the account's history",
    ),
    (
        "The search index, caches and repository clones",
        "each computer rebuilds them from its sources",
    ),
    (
        "Legacy activity entries",
        "they are read only and kept for older records",
    ),
    (
        "Machine files, account variables and their keys",
        "the coordinator and this computer keep them",
    ),
];

fn backups_root(host: &Host) -> PathBuf {
    host.state.settings.profile().join("backups")
}

fn data_files(host: &Host) -> Result<Vec<(PathBuf, bool)>> {
    let config = host.state.settings.config()?;
    Ok(safety::protected_files(&[Profile {
        dir: host.state.settings.profile().to_path_buf(),
        data: config.server.data_dir.clone(),
    }]))
}

/// A private `backups/cloud-<time>/` in the profile: a consistent copy of every database (online
/// `VACUUM INTO`) and the settings files, the same set as the updater's safety copy, with a
/// `manifest.json` naming where each came from. The newest three are kept.
pub(in crate::app::cloud) fn back_up_data(host: &Host, reason: &str) -> Result<Value> {
    let root = backups_root(host);
    security::private_dir(&root)?;
    let at = now();
    let mut name = format!("cloud-{at}");
    let mut n = 1;
    while root.join(&name).exists() {
        name = format!("cloud-{at}-{n}");
        n += 1;
    }
    let base = root.join(&name);
    security::private_dir(&base)?;
    let saved = match safety::copy_data(data_files(host)?, &base, || Ok(())) {
        Ok(saved) => saved,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&base);
            return Err(error);
        }
    };
    let bytes: u64 = saved
        .iter()
        .map(|s| std::fs::metadata(base.join(&s.file)).map_or(0, |m| m.len()))
        .sum();
    let manifest = json!({
        "created_at": at,
        "reason": reason,
        "host_version": env!("CARGO_PKG_VERSION"),
        "bytes": bytes,
        "files": saved,
    });
    write_private(
        &base.join("manifest.json"),
        &serde_json::to_string_pretty(&manifest)?,
    )?;
    prune(&root);
    Ok(
        json!({"path": base, "created_at": at, "reason": reason, "files": saved.len(), "bytes": bytes}),
    )
}

/// This profile's `backups/cloud-*` directories, oldest first.
pub(in crate::app::cloud) fn held(root: &Path) -> Vec<PathBuf> {
    held_backups(root)
}

fn held_backups(root: &Path) -> Vec<PathBuf> {
    let mut held: Vec<PathBuf> = std::fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("cloud-"))
                })
                .collect()
        })
        .unwrap_or_default();
    // `cloud-<unix seconds>[-n]`: the seconds decide, then the suffix.
    held.sort_by_key(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let mut parts = name.trim_start_matches("cloud-").split('-');
        let seconds = parts
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let suffix = parts
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        (seconds, suffix)
    });
    held
}

fn prune(root: &Path) {
    let mut held = held_backups(root);
    while held.len() > BACKUPS_KEPT {
        let _ = std::fs::remove_dir_all(held.remove(0));
    }
}

/// The newest backup, for `cloud_status`.
pub(super) fn latest_backup(host: &Host) -> Value {
    let root = backups_root(host);
    let held = held_backups(&root);
    let Some(latest) = held.last() else {
        return json!({"count": 0});
    };
    let manifest: Value = std::fs::read_to_string(latest.join("manifest.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    json!({
        "count": held.len(),
        "latest": {
            "path": latest,
            "created_at": manifest["created_at"],
            "reason": manifest["reason"],
            "bytes": manifest["bytes"],
        },
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::app::cloud) struct MigrateArgs {
    /// `plan` (what would move, nothing is written) or `start`.
    #[serde(default = "plan")]
    action: String,
    /// Another computer of the account migrated first: adopt the account's configuration from
    /// the store (after a private backup of this one's) instead of uploading this computer's.
    #[serde(default)]
    adopt: bool,
}

impl MigrateArgs {
    pub(in crate::app::cloud) fn new(action: &str) -> Self {
        Self {
            action: action.into(),
            adopt: false,
        }
    }
}

const ADOPT: &str = "The account store already holds this account's configuration from another computer. Adopt it here (pta cloud migrate --adopt --confirmed): this computer's configuration is backed up privately, then replaced by the account's, and its own history is uploaded. Nothing was uploaded.";
const NEED_KEY: &str = "The account store already holds this account's copy, sealed with a content key this computer does not have. Give it the key first (Trust this machine with the account store from the coordinator, or pta variables key restore). Nothing was uploaded.";

/// Whether the store already holds configuration this installation never acknowledged: another
/// computer of the account migrated first. A computer signed in to a coordinator keeps only its
/// history in the store, so it never adopts here.
async fn held_elsewhere(host: &Host, worker: &Remote) -> std::result::Result<bool, Failure> {
    if !HostLocal(host).configuration() {
        return Ok(false);
    }
    let page = worker.page(Collection::Settings, 0, 1).await?;
    if page.records.is_empty() {
        return Ok(false);
    }
    let acknowledged = Ledger::open(&ledger_path(host))
        .ok()
        .and_then(|ledger| ledger.rows(Collection::Settings).ok())
        .is_some_and(|rows| rows.iter().any(|r| r.acked_revision > 0));
    Ok(!acknowledged)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(in crate::app::cloud) struct RollbackArgs {
    /// Keep this computer's configuration as it is, without a last pull or a clean verification.
    #[serde(default)]
    keep_local: bool,
}

pub(in crate::app::cloud) async fn operate_migrate(host: &Arc<Host>, args: MigrateArgs) -> Reply {
    match args.action.as_str() {
        "plan" => dry_run(host),
        "start" => migrate(host, args.adopt)
            .await
            .unwrap_or_else(|reply| *reply),
        _ => Reply::error("invalid_input", 2, "Unknown migration action."),
    }
}

fn dry_run(host: &Host) -> Reply {
    let (Ok(plan), Ok(files)) = (preview(host), data_files(host)) else {
        return Reply::error(
            "operation_failed",
            1,
            "The local records could not be read.",
        );
    };
    let backup_bytes: u64 = files
        .iter()
        .map(|(path, _)| std::fs::metadata(path).map_or(0, |m| m.len()))
        .sum();
    let mode = host
        .state
        .settings
        .config()
        .map(|c| c.cloud.mode)
        .unwrap_or_default();
    let key = key::load(&keys_dir(host)).ok().flatten().is_some();
    Reply::success(json!({
        "action": "plan",
        "mode": mode,
        "key": key,
        "plan": plan,
        "stays_local": STAYS_LOCAL.iter().map(|(what, why)| json!({"what": what, "why": why})).collect::<Vec<_>>(),
        "backup": {"files": files.len(), "bytes": backup_bytes, "path": backups_root(host).join("cloud-<time>")},
        "next": "pta cloud migrate --confirmed backs up this profile first, then uploads, verifies by reading back and switches to mirror.",
    }))
}

async fn migrate(host: &Arc<Host>, adopt: bool) -> std::result::Result<Reply, Box<Reply>> {
    let config = host.state.settings.config().map_err(|_| {
        Box::new(Reply::error(
            "operation_failed",
            1,
            "The configuration could not be read.",
        ))
    })?;
    if config.cloud.mode == CloudMode::Primary {
        return Ok(Reply::success(json!({
            "action": "start", "mode": "primary", "changed": false,
            "next": "This account is already migrated and the account store is its source; pta cloud verify compares the copy.",
        })));
    }
    let worker = remote(host, &config.cloud).map_err(|m| Box::new(missing_reply(&m)))?;
    let elsewhere = held_elsewhere(host, &worker).await.map_err(|failure| {
        observe(host, &config.cloud, Some(&failure));
        Box::new(failure_reply(&failure))
    })?;
    if elsewhere && !adopt {
        return Err(Box::new(Reply::error("state_conflict", 6, ADOPT)));
    }
    if elsewhere && !matches!(key::load(&keys_dir(host)), Ok(Some(_))) {
        return Err(Box::new(Reply::error("not_ready", 3, NEED_KEY)));
    }
    if elsewhere && let Some(refusal) = super::super::outdated_elsewhere(host).await {
        return Err(Box::new(refusal));
    }
    let backup = back_up_data(host, "migrate").map_err(|_| {
        Box::new(Reply::error(
            "operation_failed",
            1,
            "This profile could not be backed up; nothing was uploaded.",
        ))
    })?;
    let mut adopted = Value::Null;
    if elsewhere {
        // The account's configuration replaces this computer's, which the backups keep.
        let replaced = super::primary::adopt(host, &config)
            .await
            .map_err(|mut reply| {
                reply.data = json!({"backup": backup.clone()});
                reply
            })?;
        adopted = replaced;
    }
    let mut reply = match start(host).await {
        Ok(reply) => reply,
        Err(mut reply) => {
            reply.data = json!({"backup": backup});
            return Err(reply);
        }
    };
    reply.data["backup"] = backup;
    if !adopted.is_null() {
        reply.data["adopted"] = adopted;
    }
    Ok(reply)
}

pub(in crate::app::cloud) async fn operate_rollback(host: &Arc<Host>, args: RollbackArgs) -> Reply {
    rollback(host, args).await.unwrap_or_else(|reply| *reply)
}

async fn rollback(host: &Arc<Host>, args: RollbackArgs) -> std::result::Result<Reply, Box<Reply>> {
    let fail = |text: &str| Box::new(Reply::error("operation_failed", 1, text));
    let _operations = host.state.operations.lock().await;
    let _gate = hold(host).await;
    let config = host
        .state
        .settings
        .config()
        .map_err(|_| fail("The configuration could not be read."))?;
    if config.cloud.mode == CloudMode::Off {
        return Ok(Reply::success(
            json!({"action": "rollback", "mode": "off", "changed": false}),
        ));
    }
    let backup = back_up_data(host, "rollback")
        .map_err(|_| fail("This profile could not be backed up; nothing was changed."))?;
    let with_backup = |reply: Reply, extra: Value| {
        let mut data = json!({"backup": backup});
        if let (Some(data), Some(extra)) = (data.as_object_mut(), extra.as_object()) {
            data.extend(extra.clone());
        }
        Box::new(Reply { data, ..reply })
    };
    let mut pulled = Value::Null;
    let mut applied = 0;
    if config.cloud.mode == CloudMode::Primary && !args.keep_local {
        match primary::pull_now(host, &config, false).await {
            Ok(done) => {
                applied = done.applied;
                pulled = json!(done);
            }
            Err(error) => {
                let refusal = primary::pull_reply(&error);
                let message = format!(
                    "{} Nothing was changed: roll back again when the account store answers, or with --keep-local to keep this computer's configuration as it is.",
                    refusal.message.as_deref().unwrap_or_default()
                );
                return Err(with_backup(
                    Reply::error(&refusal.code, refusal.exit_code, &message),
                    json!({}),
                ));
            }
        }
    }
    let setup = open(host, &config.cloud).map_err(|m| Box::new(missing_reply(&m)))?;
    let conflicts = setup.ledger.conflicts().unwrap_or(0);
    if conflicts > 0 && !args.keep_local {
        return Err(with_backup(
            Reply::error(
                "state_conflict",
                6,
                "Some configuration changed both here and in the account store; nothing was changed. Adopt the store's version with pta cloud pull --replace --confirmed, or keep this computer's with pta cloud rollback --keep-local --confirmed.",
            ),
            json!({"conflicts": conflicts, "pulled": pulled}),
        ));
    }
    let local = HostLocal(host);
    let engine = Engine {
        local: &local,
        transport: &setup.http,
        ledger: &setup.ledger,
        content: &setup.content,
        cas: false,
    };
    // What the store holds that this computer does not would stay only in the store.
    let verify = match engine.verify().await {
        Ok(Ok(report)) => {
            observe(host, &config.cloud, None);
            let _ = setup.ledger.set_meta(
                "last_verify",
                &json!({"at": now(), "ok": report.ok, "report": report}).to_string(),
            );
            Some(report)
        }
        Ok(Err(failure)) => {
            observe(host, &config.cloud, Some(&failure));
            if !args.keep_local {
                let refusal = failure_reply(&failure);
                let message = format!(
                    "{} Nothing was changed: roll back again when the account store answers, or with --keep-local.",
                    refusal.message.as_deref().unwrap_or_default()
                );
                return Err(with_backup(
                    Reply::error(&refusal.code, refusal.exit_code, &message),
                    json!({"pulled": pulled}),
                ));
            }
            None
        }
        Err(_) if !args.keep_local => {
            return Err(with_backup(
                Reply::error(
                    "operation_failed",
                    1,
                    "The copy could not be verified; nothing was changed.",
                ),
                json!({"pulled": pulled}),
            ));
        }
        Err(_) => None,
    };
    let only_remote: u64 = verify
        .as_ref()
        .map_or(0, |r| r.collections.iter().map(|c| c.unknown_remote).sum());
    if only_remote > 0 && !args.keep_local {
        return Err(with_backup(
            Reply::error(
                "state_conflict",
                6,
                "The account store holds records this computer does not have; nothing was changed. Check pta cloud verify, or roll back with --keep-local to leave them only in the store.",
            ),
            json!({"verify": verify, "pulled": pulled}),
        ));
    }
    save_mode(host, CloudMode::Off)
        .map_err(|reply| with_backup(*reply, json!({"pulled": pulled})))?;
    let _ = setup.ledger.set_meta("rolled_back_at", &now().to_string());
    Ok(Reply::success(json!({
        "action": "rollback",
        "mode": "off",
        "changed": true,
        "kept_local": args.keep_local,
        "backup": backup,
        "pulled": pulled,
        "verify": verify,
        "only_in_store": only_remote,
        "restart_required": applied > 0
            && host.state.running.try_lock().map_or(true, |r| r.is_some()),
        "next": "This computer is the only source again and nothing is uploaded. The account store keeps its copy until pta cloud erase --confirmed; pta cloud migrate --confirmed continues from it.",
    })))
}

pub(in crate::app::cloud) async fn operate_rotate(host: &Arc<Host>) -> Reply {
    rotate(host).await.unwrap_or_else(|reply| *reply)
}

/// Rotate the account content key (decision 18 of the design, for a lost computer): a new key
/// becomes active, the earlier ones stay so what is not sealed again yet still opens, and every
/// record the store holds is sent again sealed with the new key, through the outbox (under
/// compare-and-swap in primary). The blinded IDs and the content account do not change.
async fn rotate(host: &Arc<Host>) -> std::result::Result<Reply, Box<Reply>> {
    let failed = |text: &str| Box::new(Reply::error("operation_failed", 1, text));
    let _operations = host.state.operations.lock().await;
    let _gate = hold(host).await;
    let config = host
        .state
        .settings
        .config()
        .map_err(|_| failed("The configuration could not be read."))?;
    if config.cloud.mode == CloudMode::Off {
        return Err(Box::new(Reply::error(
            "not_ready",
            3,
            "This computer does not use the account store: there is no copy to seal again. Nothing was rotated.",
        )));
    }
    let setup = open(host, &config.cloud).map_err(|m| Box::new(missing_reply(&m)))?;
    let keys = host.state.settings.profile().join("keys");
    let content = crate::mirror::key::rotate(&keys)
        .map_err(|_| failed("The account content key could not be rotated; nothing changed."))?;
    let resealing = setup
        .ledger
        .reseal(now())
        .map_err(|_| failed("The new key is active, but the copy could not be scheduled to be sealed again: run the rotation again."))?;
    let _ = setup.ledger.set_meta("rotated_at", &now().to_string());
    crate::mirror::wake();
    Ok(Reply::success(json!({
        "rotated": true,
        "version": content.active,
        "resealing": resealing,
        "next": "Back up the new key now (pta variables key backup). The account's other computers trusted with the account store receive it from this coordinator within minutes (pta cloud token lists them); one trusted before 0.10.38 is trusted again with the account store.",
    })))
}

pub(in crate::app::cloud) async fn operate_erase(host: &Arc<Host>) -> Reply {
    erase(host).await.unwrap_or_else(|reply| *reply)
}

async fn erase(host: &Arc<Host>) -> std::result::Result<Reply, Box<Reply>> {
    let refuse = |text: &str| Box::new(Reply::error("not_ready", 3, text));
    let _operations = host.state.operations.lock().await;
    let _gate = hold(host).await;
    let config = host.state.settings.config().map_err(|_| {
        Box::new(Reply::error(
            "operation_failed",
            1,
            "The configuration could not be read.",
        ))
    })?;
    if config.cloud.mode != CloudMode::Off {
        return Err(refuse(
            "Roll back first (pta cloud rollback --confirmed): only a copy that is no longer in use can be erased. Nothing was erased.",
        ));
    }
    let worker = remote(host, &config.cloud).map_err(|m| Box::new(missing_reply(&m)))?;
    let health = worker.health().await.map_err(|failure| {
        observe(host, &config.cloud, Some(&failure));
        Box::new(failure_reply(&failure))
    })?;
    if !health["features"]
        .as_array()
        .is_some_and(|f| f.iter().any(|v| v == "erase"))
    {
        return Err(refuse(
            "The account store Worker cannot erase a copy yet (Worker 0.3.0 or later); nothing was erased.",
        ));
    }
    let (mut records, mut backups) = (0u64, 0u64);
    let mut finished = false;
    for _ in 0..ERASE_ROUNDS {
        match worker.erase().await {
            Ok(round) => {
                records += round["erased"].as_u64().unwrap_or(0);
                backups += round["backups"].as_u64().unwrap_or(0);
                if round["more"] == false {
                    finished = true;
                    break;
                }
            }
            Err(failure) => {
                observe(host, &config.cloud, Some(&failure));
                let refusal = match &failure {
                    Failure::Refused(code) if code == "conflict" => Reply::error(
                        "state_conflict",
                        6,
                        "The account has other connected computers: disconnect them (or revoke them from another computer) first, so none keeps writing into the copy.",
                    ),
                    other => failure_reply(other),
                };
                let message = if records + backups > 0 {
                    format!(
                        "{} Part of the copy was erased; run pta cloud erase --confirmed again to finish.",
                        refusal.message.as_deref().unwrap_or_default()
                    )
                } else {
                    format!(
                        "{} Nothing was erased.",
                        refusal.message.as_deref().unwrap_or_default()
                    )
                };
                return Err(Box::new(Reply {
                    data: json!({"erased": records, "backups": backups}),
                    ..Reply::error(&refusal.code, refusal.exit_code, &message)
                }));
            }
        }
    }
    observe(host, &config.cloud, None);
    if !finished {
        return Err(Box::new(Reply {
            data: json!({"erased": records, "backups": backups}),
            ..Reply::error(
                "operation_failed",
                1,
                "The account store is still erasing the copy; run pta cloud erase --confirmed again to finish.",
            )
        }));
    }
    let at = now();
    // The copy is gone: nothing is acknowledged any more, and the key that sealed it is set aside.
    if let Ok(ledger) = Ledger::open(&ledger_path(host)) {
        let _ = ledger.clear();
        let _ = ledger.set_meta("erased_at", &at.to_string());
    }
    let retired = key::retire(&keys_dir(host), at).map_err(|_| {
        Box::new(Reply::error(
            "operation_failed",
            1,
            "The copy was erased, but the account content key could not be set aside; check the keys directory of this profile.",
        ))
    })?;
    Ok(Reply::success(json!({
        "action": "erase",
        "erased": records,
        "backups": backups,
        "key_retired": retired,
        "recovery": "Cloudflare keeps a 30-day point-in-time recovery of the account store: only the operator can restore it, and only the retired content key (this computer's keys directory or a key backup) can read it. Erased R2 backups cannot be recovered.",
        "next": "This computer keeps all of its data. pta cloud migrate --confirmed starts a new copy with a new content key; back that key up afterwards.",
    })))
}

#[cfg(test)]
mod tests;
