//! The host's side of primary mode (phase 3c): the account store is the source of truth of the
//! account's configuration (the settings records, the organization and the notes). A change to
//! it is written there first under compare-and-swap and saved here only once accepted; the loop
//! pulls what the store changed into the local stores, which stay a complete replica, so every
//! read (the Teams pipeline included) stays local and never waits on Cloudflare. Without an
//! answer from the store the configuration is read-only; nothing is queued.

use super::*;
use crate::{
    config::Config,
    index::{Index, Note},
    knowledge::KnowledgeMap,
    mirror::primary::{Accepted, Apply, Change, Pulled, Refusal},
    store::{
        notes::{LocalNotes, NotesStore},
        organization::OrganizationRecord,
        settings::{SettingsStore, read_back, records},
    },
};

const CONFLICT: &str = "[conflict] The account configuration changed in the account store since this computer last synchronized. Nothing was saved; it is being synchronized now: review it and repeat the change.";
const UNRESOLVED: &str = "[conflict] This computer and the account store changed the same configuration. Nothing was saved. Replace this computer's configuration with the account store's (pta cloud pull --replace, or Settings › Advanced › Cloud storage) and repeat the change.";
const OFFLINE: &str = "[network] The account store did not answer, so nothing was saved here and the configuration is read-only until it does. If the store received the change before the connection failed, it appears here after the next synchronization.";
const REFUSED: &str =
    "[not_ready] The account store refused the change; check pta cloud status. Nothing was saved.";
const TOO_LARGE: &str = "[invalid_input] The configuration is too large for the account store (1 MiB sealed per record). Nothing was saved.";
const BROKEN: &str = "The account store's key or ledger could not be read; nothing was saved.";
const BACKUPS: usize = 3;

/// Whether this installation writes its configuration to the account store first.
pub(in crate::app) fn active(state: &DesktopState) -> bool {
    state
        .settings
        .config()
        .is_ok_and(|c| c.cloud.mode == CloudMode::Primary)
}

/// A change the account store accepted; the caller saves it locally and then settles it, all
/// while no pass runs. Dropped without settling (the local save failed), the next pull brings the
/// accepted change here instead.
pub(in crate::app) struct Prepared<'a> {
    _gate: tokio::sync::MutexGuard<'a, ()>,
    ledger: Ledger,
    accepted: Accepted,
}

impl Prepared<'_> {
    pub(in crate::app) fn settle(self) {
        if self.accepted.settle(&self.ledger).is_err() {
            // The next pull acknowledges what the store and the local stores both hold.
            tracing::warn!(event = "cloud_settle_failed");
        }
    }
}

/// The configuration records of a configuration and its map, as the stores will export them.
fn changes(config: &Config, map: &KnowledgeMap) -> Result<Vec<Change>> {
    let (config, map) = read_back(config, map)?;
    let organization = OrganizationRecord {
        organization: map.organization.clone(),
        guides: map.work_item_guides.clone(),
    };
    let mut changes: Vec<Change> = records(config, map)?
        .into_iter()
        .map(|e| Change {
            collection: Collection::Settings,
            id: e.id,
            value: Some(e.value),
        })
        .collect();
    changes.push(Change {
        collection: Collection::Organization,
        id: "organization".into(),
        value: Some(serde_json::to_value(organization)?),
    });
    Ok(changes)
}

/// Before a settings commit: in primary mode the account store must accept the configuration
/// first. `Ok(None)` outside primary mode.
pub(in crate::app) async fn before_save<'a>(
    host: &'a Host,
    config: &Config,
    map: &KnowledgeMap,
) -> std::result::Result<Option<Prepared<'a>>, String> {
    if !active(&host.state) {
        return Ok(None);
    }
    let changes = changes(config, map).map_err(|_| BROKEN.to_owned())?;
    write_through(host, &changes).await.map(Some)
}

/// Write configuration records to the account store before the local stores change.
pub(in crate::app) async fn write_through<'a>(
    host: &'a Host,
    changes: &[Change],
) -> std::result::Result<Prepared<'a>, String> {
    let gate = hold(host).await;
    let config = host
        .state
        .settings
        .config()
        .map_err(|_| BROKEN.to_owned())?;
    if config.cloud.mode != CloudMode::Primary {
        return Err(
            "[conflict] The account store stopped being primary meanwhile; repeat the change."
                .into(),
        );
    }
    let setup = open(host, &config.cloud).map_err(|missing| match missing {
        Missing::Broken => BROKEN.to_owned(),
        _ => REFUSED.to_owned(),
    })?;
    let local = HostLocal(host);
    let engine = Engine {
        local: &local,
        transport: &setup.http,
        ledger: &setup.ledger,
        content: &setup.content,
        cas: true,
    };
    let written = engine.write(changes).await.map_err(|_| BROKEN.to_owned())?;
    match written {
        Ok(accepted) => {
            if !accepted.rows.is_empty() {
                observe(host, &config.cloud, None);
            }
            Ok(Prepared {
                _gate: gate,
                ledger: setup.ledger,
                accepted,
            })
        }
        Err(Refusal::Conflict) => {
            observe(host, &config.cloud, None);
            // The loop pulls the newer configuration right away.
            crate::mirror::wake();
            Err(CONFLICT.into())
        }
        Err(Refusal::Unresolved) => Err(UNRESOLVED.into()),
        Err(Refusal::TooLarge) => Err(TOO_LARGE.into()),
        Err(Refusal::Failure(failure)) => {
            observe(host, &config.cloud, Some(&failure));
            Err(match failure {
                Failure::Refused(_) | Failure::Revoked | Failure::Unconfigured => REFUSED,
                Failure::Offline | Failure::Quota => OFFLINE,
            }
            .into())
        }
    }
}

/// Saves a note: in primary mode the account store accepts it first. The note's ID and times are
/// fixed before it is sent, so the local copy is exactly what the store holds.
pub(in crate::app) async fn save_note(
    host: &Host,
    index: &Index,
    mut note: Note,
) -> std::result::Result<Note, String> {
    let records = LocalNotes::new(index);
    let now = chrono::Utc::now().timestamp();
    if note.id.is_empty() {
        note.id = uuid::Uuid::new_v4().to_string();
        note.created_at = now;
    } else {
        let held = records
            .note(&note.id)
            .map_err(|_| BROKEN.to_owned())?
            .ok_or("[invalid_input] Unknown note.")?;
        note.created_at = held.created_at;
    }
    note.updated_at = now;
    let value = json!({"updated_at": note.updated_at, "note": note});
    let prepared = write_through(
        host,
        &[Change {
            collection: Collection::Notes,
            id: note.id.clone(),
            value: Some(value),
        }],
    )
    .await?;
    records.put_note(&note).map_err(|_| {
        "The note could not be saved here; it appears after the next synchronization.".to_owned()
    })?;
    prepared.settle();
    Ok(note)
}

/// Removes a note: in primary mode the account store accepts its removal first.
pub(in crate::app) async fn remove_note(
    host: &Host,
    index: &Index,
    id: &str,
) -> std::result::Result<bool, String> {
    let records = LocalNotes::new(index);
    if records.note(id).map_err(|_| BROKEN.to_owned())?.is_none() {
        return Ok(false);
    }
    let prepared = write_through(
        host,
        &[Change {
            collection: Collection::Notes,
            id: id.to_owned(),
            value: None,
        }],
    )
    .await?;
    let removed = records.remove_note(id).map_err(|_| {
        "The note could not be removed here; it disappears after the next synchronization."
            .to_owned()
    })?;
    prepared.settle();
    Ok(removed)
}

/// Writes what the account store holds into this installation's stores.
struct HostApply<'a>(&'a Host);

impl Apply for HostApply<'_> {
    fn apply(&self, collection: Collection, changes: &[(String, Option<Value>)]) -> Result<()> {
        let state = &self.0.state;
        match collection {
            Collection::Settings => {
                let mut config = state.settings.config()?;
                let mut map = state.settings.map()?;
                for (id, value) in changes {
                    let value = value
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("the settings are never removed"))?;
                    match id.as_str() {
                        // The account's: this installation keeps its own sections.
                        "config" => {
                            config =
                                crate::store::settings::install_account_config(&config, value)?;
                        }
                        "knowledge_map" => {
                            let mut next: KnowledgeMap = serde_json::from_value(value)?;
                            next.organization = std::mem::take(&mut map.organization);
                            next.work_item_guides = std::mem::take(&mut map.work_item_guides);
                            // Repository folders are each computer's own binding.
                            next.repositories = std::mem::take(&mut map.repositories);
                            map = next;
                        }
                        // This installation's own settings: local first, the copy is a backup.
                        id if id.starts_with("installation/") => {}
                        _ => anyhow::bail!("unknown settings record"),
                    }
                }
                commit(state, &config, &map)
            }
            Collection::Organization => {
                let [(_, Some(value))] = changes else {
                    anyhow::bail!("the organization is one record that is never removed");
                };
                let record: OrganizationRecord = serde_json::from_value(value.clone())?;
                let config = state.settings.config()?;
                let mut map = state.settings.map()?;
                map.organization = record.organization;
                map.work_item_guides = record.guides;
                commit(state, &config, &map)
            }
            Collection::Notes => {
                let config = state.settings.config()?;
                let index = Index::open(&config.server.data_dir.join(crate::index::FILE))?;
                let records = LocalNotes::new(&index);
                for (id, value) in changes {
                    match value {
                        Some(value) => {
                            let note: Note = serde_json::from_value(value["note"].clone())?;
                            anyhow::ensure!(note.id == *id, "a note filed under another ID");
                            records.put_note(&note)?;
                        }
                        None => {
                            records.remove_note(id)?;
                        }
                    }
                }
                Ok(())
            }
            _ => anyhow::bail!("only configuration is pulled"),
        }
    }
}

/// Install pulled settings with the same checks as an edit; a running Teams service keeps the
/// previous ones until it restarts, as after any settings change.
fn commit(state: &DesktopState, config: &Config, map: &KnowledgeMap) -> Result<()> {
    validate_local(config, map)?;
    let tunnel = state.settings.tunnel().unwrap_or_default();
    state.settings.commit(config, map, &tunnel)?;
    forget_removed_sources(config, map);
    if let Ok(mut graph) = state.activity_graph.try_lock() {
        *graph = None;
    }
    Ok(())
}

/// One pull with the locks a settings change takes (operations, then the pass gate).
pub(super) async fn pull_now(
    host: &Host,
    config: &Config,
    replace: bool,
) -> std::result::Result<Pulled, PullError> {
    let setup = open(host, &config.cloud).map_err(PullError::Missing)?;
    let local = HostLocal(host);
    let engine = Engine {
        local: &local,
        transport: &setup.http,
        ledger: &setup.ledger,
        content: &setup.content,
        cas: true,
    };
    let pulled = match engine.pull(&HostApply(host), replace).await {
        Ok(Ok(pulled)) => pulled,
        Ok(Err(failure)) => {
            observe(host, &config.cloud, Some(&failure));
            return Err(PullError::Failure(failure));
        }
        Err(_) => return Err(PullError::Apply),
    };
    observe(host, &config.cloud, None);
    let _ = setup.ledger.set_meta(
        "last_pull",
        &json!({"at": now(), "applied": pulled.applied, "conflicts": pulled.conflicts,
            "undecryptable": pulled.undecryptable})
        .to_string(),
    );
    Ok(pulled)
}

pub(super) enum PullError {
    Missing(Missing),
    Failure(Failure),
    /// A pulled record could not be installed here (it fails this installation's checks).
    Apply,
}

pub(super) fn pull_reply(error: &PullError) -> Reply {
    match error {
        PullError::Missing(missing) => missing_reply(missing),
        PullError::Failure(failure) => failure_reply(failure),
        PullError::Apply => Reply::error(
            "operation_failed",
            1,
            "The account store's configuration could not be installed on this computer; nothing more was changed. Check pta cloud status.",
        ),
    }
}

/// The loop's pull, before its upload: brings what the store changed. `Err` stops the pass.
pub(super) async fn pass_pull(host: &Host) -> std::result::Result<(), Option<String>> {
    let _operations = host.state.operations.lock().await;
    let _gate = hold(host).await;
    let Ok(config) = host.state.settings.config() else {
        return Err(None);
    };
    if config.cloud.mode != CloudMode::Primary {
        return Ok(());
    }
    match pull_now(host, &config, false).await {
        Ok(_) => Ok(()),
        Err(PullError::Failure(failure)) => Err(Some(failure.code())),
        Err(_) => Err(None),
    }
}

/// Before serving the account configuration to another installation: bring what the store
/// changed, when it answers; otherwise the replica here is served as it is.
pub(in crate::app) async fn refresh(host: &Host) {
    if !active(&host.state) {
        return;
    }
    let _operations = host.state.operations.lock().await;
    let _gate = hold(host).await;
    if let Ok(config) = host.state.settings.config() {
        let _ = pull_now(host, &config, false).await;
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::app::cloud) struct PromoteArgs {
    /// `promote` (mirror → primary) or `demote` (primary → mirror).
    action: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::app::cloud) struct PullArgs {
    /// Adopt the account store's version of records changed on both sides (a backup first).
    #[serde(default)]
    replace: bool,
}

pub(in crate::app::cloud) async fn operate_promote(host: &Arc<Host>, args: PromoteArgs) -> Reply {
    match args.action.as_str() {
        "promote" => promote(host).await.unwrap_or_else(|reply| *reply),
        "demote" => demote(host).await,
        _ => Reply::error("invalid_input", 2, "Unknown action."),
    }
}

async fn demote(host: &Arc<Host>) -> Reply {
    let _gate = hold(host).await;
    let Ok(config) = host.state.settings.config() else {
        return Reply::error(
            "operation_failed",
            1,
            "The configuration could not be read.",
        );
    };
    match config.cloud.mode {
        CloudMode::Primary => match save_mode(host, CloudMode::Mirror) {
            Ok(()) => Reply::success(
                json!({"action": "demote", "mode": "mirror", "changed": true,
                "next": "This computer's configuration is the source of truth again; the account store keeps mirroring it."}),
            ),
            Err(reply) => *reply,
        },
        CloudMode::Mirror => {
            Reply::success(json!({"action": "demote", "mode": "mirror", "changed": false}))
        }
        CloudMode::Off => Reply::error(
            "not_ready",
            3,
            "The account store is not in use on this installation.",
        ),
    }
}

async fn promote(host: &Arc<Host>) -> std::result::Result<Reply, Box<Reply>> {
    let fail = |text: &str| Box::new(Reply::error("operation_failed", 1, text));
    let refuse = |text: &str| Box::new(Reply::error("not_ready", 3, text));
    let config = host
        .state
        .settings
        .config()
        .map_err(|_| fail("The configuration could not be read."))?;
    match config.cloud.mode {
        CloudMode::Primary => {
            return Ok(Reply::success(
                json!({"action": "promote", "mode": "primary", "changed": false}),
            ));
        }
        CloudMode::Off => {
            return Err(refuse(
                "Migrate this installation first (pta cloud migrate --confirmed); only a verified copy can become the source of truth.",
            ));
        }
        CloudMode::Mirror => {}
    }
    if crate::app::implementation::link(&host.state)
        .ok()
        .flatten()
        .is_some()
    {
        return Err(refuse(
            "This installation takes its configuration from the account's coordinator; promote the account store on the coordinator instead.",
        ));
    }
    let setup = open(host, &config.cloud).map_err(|m| Box::new(missing_reply(&m)))?;
    let health = setup.http.health().await.map_err(|failure| {
        observe(host, &config.cloud, Some(&failure));
        Box::new(failure_reply(&failure))
    })?;
    if !health["features"]
        .as_array()
        .is_some_and(|f| f.iter().any(|v| v == "cas"))
    {
        return Err(refuse(
            "The account store Worker does not offer compare-and-swap writes yet (Worker 0.2.0 or later); nothing was changed.",
        ));
    }
    let _operations = host.state.operations.lock().await;
    let _gate = hold(host).await;
    let local = HostLocal(host);
    let mirror = Engine {
        local: &local,
        transport: &setup.http,
        ledger: &setup.ledger,
        content: &setup.content,
        cas: false,
    };
    // Everything waiting in the outbox first, so the store holds exactly this installation.
    for _ in 0..200 {
        let flushed = mirror
            .flush(now(), Limits::default())
            .await
            .map_err(|_| fail("The mirror could not run."))?;
        if let Some(code) = flushed.failure {
            let failure = failure_of(&code);
            observe(host, &config.cloud, Some(&failure));
            return Err(Box::new(failure_reply(&failure)));
        }
        if flushed.batches == 0 {
            break;
        }
    }
    let report = match mirror
        .verify()
        .await
        .map_err(|_| fail("The copy could not be verified."))?
    {
        Ok(report) => report,
        Err(failure) => {
            observe(host, &config.cloud, Some(&failure));
            return Err(Box::new(failure_reply(&failure)));
        }
    };
    let _ = setup.ledger.set_meta(
        "last_verify",
        &json!({"at": now(), "ok": report.ok, "report": report}).to_string(),
    );
    let pending: u64 = Collection::ALL
        .into_iter()
        .filter(|c| c.is_configuration())
        .map(|c| setup.ledger.outbox(Some(c), now()).map_or(1, |o| o.pending))
        .sum();
    if !report.ok || pending > 0 {
        return Err(Box::new(Reply {
            data: json!({"verify": report, "configuration_pending": pending}),
            ..Reply::error(
                "state_conflict",
                6,
                "The account store's copy is not identical to this installation yet; nothing was changed. Run pta cloud verify, then promote again.",
            )
        }));
    }
    observe(host, &config.cloud, None);
    // A way back to exactly this profile before the store becomes its source.
    let backup = migration::back_up_data(host, "promote")
        .map_err(|_| fail("This profile could not be backed up; nothing was changed."))?;
    save_mode(host, CloudMode::Primary)?;
    let _ = setup.ledger.set_meta("promoted_at", &now().to_string());
    // Pulls start from what the copy holds now.
    let _ = pull_now(host, &host.state.settings.config().unwrap_or(config), false).await;
    Ok(Reply::success(json!({
        "action": "promote",
        "mode": "primary",
        "changed": true,
        "verify": report,
        "backup": backup,
        "next": "The account store is now the source of truth of this account's configuration: changes are saved there first, and without an answer from it the configuration is read-only. pta cloud demote --confirmed returns to mirror.",
    })))
}

pub(in crate::app::cloud) async fn operate_pull(host: &Arc<Host>, args: PullArgs) -> Reply {
    let _operations = host.state.operations.lock().await;
    let _gate = hold(host).await;
    let Ok(config) = host.state.settings.config() else {
        return Reply::error(
            "operation_failed",
            1,
            "The configuration could not be read.",
        );
    };
    if config.cloud.mode != CloudMode::Primary {
        return Reply::error(
            "not_ready",
            3,
            "The account store is not the source of truth on this installation (pta cloud promote first).",
        );
    }
    let backup = if args.replace {
        match back_up(host) {
            Ok(path) => Some(path),
            Err(_) => {
                return Reply::error(
                    "operation_failed",
                    1,
                    "This computer's configuration could not be backed up; nothing was replaced.",
                );
            }
        }
    } else {
        None
    };
    match pull_now(host, &config, args.replace).await {
        Ok(pulled) => {
            let conflicts = open(host, &config.cloud)
                .ok()
                .and_then(|s| s.ledger.conflicts().ok())
                .unwrap_or(0);
            Reply::success(json!({
                "replace": args.replace,
                "pulled": pulled,
                "conflicts": conflicts,
                "backup": backup,
                "restart_required": host.state.running.try_lock().map_or(true, |r| r.is_some())
                    && pulled.applied > 0,
            }))
        }
        Err(error) => pull_reply(&error),
    }
}

/// A private copy of the configuration and the notes about to be replaced, in
/// `cloud/backups/replaced-<time>.json`; the newest three are kept.
fn back_up(host: &Host) -> Result<String> {
    let state = &host.state;
    let config = state.settings.config()?;
    let map = state.settings.map()?;
    let notes = Index::open(&config.server.data_dir.join(crate::index::FILE))
        .and_then(|index| LocalNotes::new(&index).export_notes())
        .unwrap_or_default();
    let mut settings = records(config, map.clone())?;
    let dir = state.settings.profile().join("cloud").join("backups");
    security::private_dir(&dir)?;
    let path = dir.join(format!("replaced-{}.json", now()));
    let body = json!({
        "settings": settings.drain(..).map(|e| json!({"id": e.id, "value": e.value})).collect::<Vec<_>>(),
        "organization": OrganizationRecord { organization: map.organization, guides: map.work_item_guides },
        "notes": notes.into_iter().map(|e| json!({"id": e.id, "value": e.value})).collect::<Vec<_>>(),
    });
    write_private(&path, &serde_json::to_string(&body)?)?;
    let mut held: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("replaced-"))
        })
        .collect();
    held.sort();
    while held.len() > BACKUPS {
        let _ = std::fs::remove_file(held.remove(0));
    }
    Ok(path.to_string_lossy().into_owned())
}

/// What primary mode knows locally, for `cloud_status`.
pub(super) fn describe(ledger: &Ledger) -> Value {
    let meta = |name: &str| -> Value {
        ledger
            .meta(name)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null)
    };
    let mut pulled = serde_json::Map::new();
    for collection in Collection::ALL.into_iter().filter(|c| c.is_configuration()) {
        pulled.insert(
            collection.name().into(),
            meta(&format!("pulled:{}", collection.name())),
        );
    }
    json!({
        "promoted_at": meta("promoted_at"),
        "conflicts": ledger.conflicts().unwrap_or(0),
        "last_pull": meta("last_pull"),
        "pulled_generations": pulled,
    })
}

#[cfg(test)]
mod tests;
