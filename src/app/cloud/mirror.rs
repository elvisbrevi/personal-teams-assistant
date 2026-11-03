//! The host's side of the mirror (phase 3b): reading the local stores for the engine, talking to
//! the Worker, the background loop that keeps the copy current, and the operations `cloud_mirror`
//! (plan, start, stop) and `cloud_verify`. The Teams pipeline never waits on any of it: the loop
//! is its own task and only reads the stores.

use super::*;
use crate::{
    activity::store::Database as ActivityDatabase,
    index::{self, Index},
    mirror::{
        Collection, Exported,
        engine::{Engine, Failure, Limits, Local, Outcome, Page, Stored, Transport, Wire},
        key::{self, Content},
        ledger::Ledger,
    },
    state::Store,
    store::{
        activity::LocalActivity,
        audit::LocalAudit,
        implementation::LocalImplementation,
        notes::LocalNotes,
        organization::{LocalOrganization, OrganizationStore},
    },
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{path::PathBuf, time::Instant};

/// A sweep runs at least this often while mirroring, announced or not.
const SWEEP_SECONDS: u64 = 600;
/// A write that announces itself is sent after this pause, so a burst goes in one batch.
const DEBOUNCE_SECONDS: u64 = 5;
const PAGE_BYTES: usize = 16_000_000;

/// What the host shares between the loop and the operations.
#[derive(Default)]
pub(in crate::app) struct State {
    /// One pass, upload or verification at a time.
    gate: tokio::sync::Mutex<()>,
}

/// The local stores, read as the engine needs them.
struct HostLocal<'a>(&'a Host);

fn exists(path: &std::path::Path) -> bool {
    path.exists()
}

impl Local for HostLocal<'_> {
    fn collect(&self, collection: Collection) -> Result<Vec<Exported>> {
        let state = &self.0.state;
        let config = state.settings.config()?;
        let data = &config.server.data_dir;
        let index = |data: &PathBuf| -> Result<Option<Index>> {
            let path = data.join(index::FILE);
            exists(&path).then(|| Index::open(&path)).transpose()
        };
        let activity = |data: &PathBuf| -> Result<Option<ActivityDatabase>> {
            let path = data.join("activity-registration.db");
            exists(&path)
                .then(|| ActivityDatabase::open(&path))
                .transpose()
        };
        Ok(match collection {
            Collection::Settings => state.settings.export()?,
            Collection::Organization => vec![Exported {
                id: "organization".into(),
                value: serde_json::to_value(
                    LocalOrganization::new(&state.settings).organization()?,
                )?,
            }],
            Collection::Notes => index(data)?
                .map(|i| LocalNotes::new(&i).export_notes())
                .transpose()?
                .unwrap_or_default(),
            Collection::Evaluations => index(data)?
                .map(|i| LocalNotes::new(&i).export_evaluations())
                .transpose()?
                .unwrap_or_default(),
            Collection::Examples => index(data)?
                .map(|i| LocalNotes::new(&i).export_examples())
                .transpose()?
                .unwrap_or_default(),
            Collection::Audits => {
                let path = data.join("assistant.db");
                if exists(&path) {
                    LocalAudit::new(&Store::attach(&path)?).export()?
                } else {
                    Vec::new()
                }
            }
            Collection::ActivitySnapshots => activity(data)?
                .map(|d| LocalActivity::new(&d).export_days())
                .transpose()?
                .unwrap_or_default(),
            Collection::ActivitySummaries => activity(data)?
                .map(|d| LocalActivity::new(&d).export_summaries())
                .transpose()?
                .unwrap_or_default(),
            Collection::ActivityRuns => activity(data)?
                .map(|d| LocalActivity::new(&d).export_runs())
                .transpose()?
                .unwrap_or_default(),
            Collection::ImplementationRuns => {
                LocalImplementation::new(&state.implementation.db).export_runs()?
            }
            Collection::ImplementationRequests => {
                LocalImplementation::new(&state.implementation.db).export_requests()?
            }
        })
    }
}

/// The Worker, over HTTPS with both credentials.
struct Http {
    endpoint: String,
    credentials: Credentials,
}

impl Http {
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> std::result::Result<Value, Failure> {
        exchange_limited(
            &self.endpoint,
            method,
            path,
            Some(&self.credentials),
            body,
            PAGE_BYTES,
        )
        .await
    }
}

fn stored(value: &Value) -> Option<Stored> {
    Some(Stored {
        collection: Collection::parse(value["collection"].as_str()?)?,
        id: value["id"].as_str()?.to_owned(),
        revision: value["revision"].as_u64()?,
        digest: value["digest"].as_str()?.to_owned(),
        schema: u32::try_from(value["schema"].as_u64()?).ok()?,
        deleted: value["deleted"].as_bool()?,
        generation: value["generation"].as_u64()?,
        sealed: match value["sealed"].as_str() {
            Some(text) => Some(STANDARD.decode(text).ok()?),
            None => None,
        },
    })
}

impl Transport for Http {
    async fn write(&self, records: &[Wire]) -> std::result::Result<Vec<Outcome>, Failure> {
        let body = json!({"records": records.iter().map(|r| json!({
            "collection": r.collection.name(),
            "id": r.id,
            "revision": r.revision,
            "digest": r.digest,
            "schema": r.schema,
            "deleted": r.deleted,
            "sealed": r.sealed.as_ref().map(|s| STANDARD.encode(s)),
        })).collect::<Vec<_>>()});
        let reply = self
            .call(reqwest::Method::POST, "/v1/records", Some(body))
            .await?;
        let results = reply["results"]
            .as_array()
            .filter(|results| results.len() == records.len())
            .ok_or_else(|| Failure::Refused("bad_reply".into()))?;
        results
            .iter()
            .zip(records)
            .map(|(result, wire)| {
                let same = result["id"] == json!(wire.id)
                    && result["revision"] == json!(wire.revision)
                    && result["collection"] == json!(wire.collection.name());
                match (same, result["result"].as_str()) {
                    (true, Some("created")) => Ok(Outcome::Created),
                    (true, Some("updated")) => Ok(Outcome::Updated),
                    (true, Some("unchanged")) => Ok(Outcome::Unchanged),
                    (true, Some("stale")) => Ok(Outcome::Stale),
                    (true, Some("conflict")) => Ok(Outcome::Conflict),
                    _ => Err(Failure::Refused("bad_reply".into())),
                }
            })
            .collect()
    }

    async fn page(
        &self,
        collection: Collection,
        since: u64,
        limit: usize,
    ) -> std::result::Result<Page, Failure> {
        let reply = self
            .call(
                reqwest::Method::GET,
                &format!(
                    "/v1/records/{}?since={since}&limit={limit}",
                    collection.name()
                ),
                None,
            )
            .await?;
        let records = reply["records"]
            .as_array()
            .ok_or_else(|| Failure::Refused("bad_reply".into()))?
            .iter()
            .map(|v| stored(v).ok_or_else(|| Failure::Refused("bad_reply".into())))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Page {
            records,
            more: reply["more"].as_bool().unwrap_or(false),
        })
    }

    async fn get(
        &self,
        collection: Collection,
        id: &str,
    ) -> std::result::Result<Option<Stored>, Failure> {
        match self
            .call(
                reqwest::Method::GET,
                &format!("/v1/records/{}/{id}", collection.name()),
                None,
            )
            .await
        {
            Ok(value) => stored(&value)
                .map(Some)
                .ok_or_else(|| Failure::Refused("bad_reply".into())),
            Err(Failure::Refused(code)) if code == "not_found" => Ok(None),
            Err(failure) => Err(failure),
        }
    }
}

/// Everything a pass needs, or why there is nothing to do.
struct Setup {
    http: Http,
    content: Content,
    ledger: Ledger,
}

enum Missing {
    /// Not enrolled, or the mode is off: nothing to do.
    NotLinked,
    NoKey,
    Broken,
}

fn keys_dir(host: &Host) -> PathBuf {
    host.state.settings.profile().join("keys")
}

fn ledger_path(host: &Host) -> PathBuf {
    host.state
        .settings
        .profile()
        .join("cloud")
        .join("mirror.db")
}

fn open(host: &Host, cloud: &Cloud) -> std::result::Result<Setup, Missing> {
    if cloud.endpoint.is_empty() || !enrolled(cloud) {
        return Err(Missing::NotLinked);
    }
    let credentials = stored_credentials()
        .filter(|c| c.token.is_some())
        .ok_or(Missing::NotLinked)?;
    let content = key::load(&keys_dir(host))
        .map_err(|_| Missing::Broken)?
        .ok_or(Missing::NoKey)?;
    let ledger = Ledger::open(&ledger_path(host)).map_err(|_| Missing::Broken)?;
    Ok(Setup {
        http: Http {
            endpoint: origin(&cloud.endpoint),
            credentials,
        },
        content,
        ledger,
    })
}

fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

/// One sweep and upload of what changed. Used by the loop; the engine does the work.
async fn pass(host: &Arc<Host>) -> std::result::Result<Option<String>, ()> {
    let _gate = host.state.cloud.gate.lock().await;
    let Ok(config) = host.state.settings.config() else {
        return Err(());
    };
    if config.cloud.mode == CloudMode::Off {
        return Ok(None);
    }
    let Ok(setup) = open(host, &config.cloud) else {
        return Err(());
    };
    let local = HostLocal(host);
    let engine = Engine {
        local: &local,
        transport: &setup.http,
        ledger: &setup.ledger,
        content: &setup.content,
    };
    let at = now();
    let Ok(flushed) = engine.flush(at, Limits::default()).await else {
        return Err(());
    };
    if flushed.batches > 0 {
        let mut millis = flushed.batch_millis.clone();
        let _ = setup.ledger.set_meta(
            "last_pass",
            &json!({"at": at, "sent": flushed.sent, "refused": flushed.refused, "batches": flushed.batches,
                "batch_ms_median": median(&mut millis), "batch_ms_max": flushed.batch_millis.iter().max()})
            .to_string(),
        );
    }
    Ok(flushed.failure)
}

/// The background loop: sends what changed soon after it happens and sweeps now and then, so a
/// write that announced nothing is caught too. A Worker that does not answer is retried slowly.
/// The caller spawns it on its runtime.
pub(in crate::app) async fn run(host: Arc<Host>) {
    // The first sweep after a start catches what changed while the host was down.
    let mut pause = DEBOUNCE_SECONDS;
    loop {
        tokio::select! {
            _ = crate::mirror::WAKE.notified() => {
                tokio::time::sleep(std::time::Duration::from_secs(DEBOUNCE_SECONDS)).await;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(pause)) => {}
        }
        pause = match pass(&host).await {
            Ok(None) => SWEEP_SECONDS,
            Ok(Some(code)) if code == "revoked" || code == "unconfigured" => 900,
            Ok(Some(code)) if code == "quota" => 1800,
            Ok(Some(_)) | Err(()) => 120,
        };
    }
}

fn failure_reply(failure: &Failure) -> Reply {
    match failure {
        Failure::Offline | Failure::Quota => {
            Reply::error("dependency_or_network", 5, &problem(failure))
        }
        Failure::Revoked | Failure::Unconfigured => Reply::error("not_ready", 3, &problem(failure)),
        Failure::Refused(_) => Reply::error("not_ready", 3, &problem(failure)),
    }
}

fn missing_reply(missing: &Missing) -> Reply {
    match missing {
        Missing::NotLinked => Reply::error(
            "not_ready",
            3,
            "This installation is not connected to an account store: run pta cloud connect first.",
        ),
        Missing::NoKey => Reply::error(
            "not_ready",
            3,
            "There is no account content key on this installation: run pta cloud mirror to create it, or restore it from the key backup.",
        ),
        Missing::Broken => Reply::error(
            "operation_failed",
            1,
            "The mirror's key or ledger could not be read.",
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MirrorArgs {
    /// `plan` (what would be uploaded), `start` (upload, verify, then mirror) or `stop`.
    #[serde(default = "plan")]
    action: String,
}

fn plan() -> String {
    "plan".into()
}

fn save_mode(host: &Host, mode: CloudMode) -> std::result::Result<(), Box<Reply>> {
    let fail = || {
        Box::new(Reply::error(
            "operation_failed",
            1,
            "The configuration could not be saved.",
        ))
    };
    let mut config = host.state.settings.config().map_err(|_| fail())?;
    config.cloud.mode = mode;
    config
        .cloud
        .validate()
        .and_then(|_| host.state.settings.save_config(&config))
        .map_err(|_| fail())
}

/// What mirroring would upload, without a key, a ledger or a request.
fn preview(host: &Host) -> Result<Value> {
    let local = HostLocal(host);
    let mut total = 0u64;
    let mut collections = serde_json::Map::new();
    for collection in Collection::ALL {
        let records = local.collect(collection)?;
        let bytes: usize = records
            .iter()
            .map(|e| crate::mirror::plaintext(&e.id, &e.value).len())
            .sum();
        total += records.len() as u64;
        collections.insert(
            collection.name().into(),
            json!({"records": records.len(), "bytes": bytes}),
        );
    }
    Ok(json!({"records": total, "collections": collections}))
}

pub(super) async fn operate_mirror(host: &Arc<Host>, args: MirrorArgs) -> Reply {
    match args.action.as_str() {
        "plan" => match preview(host) {
            Ok(plan) => {
                let key = key::load(&keys_dir(host)).ok().flatten().is_some();
                Reply::success(json!({"action": "plan", "key": key, "plan": plan,
                    "excluded": "Secrets, credentials, sessions, send nonces and runs still in progress never leave this installation."}))
            }
            Err(_) => Reply::error(
                "operation_failed",
                1,
                "The local records could not be read.",
            ),
        },
        "stop" => match save_mode(host, CloudMode::Off) {
            Ok(()) => Reply::success(json!({"action": "stop", "mode": "off"})),
            Err(reply) => *reply,
        },
        "start" => start(host).await.unwrap_or_else(|reply| *reply),
        _ => Reply::error("invalid_input", 2, "Unknown mirror action."),
    }
}

async fn start(host: &Arc<Host>) -> std::result::Result<Reply, Box<Reply>> {
    let _gate = host.state.cloud.gate.lock().await;
    let config = host.state.settings.config().map_err(|_| {
        Box::new(Reply::error(
            "operation_failed",
            1,
            "The configuration could not be read.",
        ))
    })?;
    if config.cloud.endpoint.is_empty() || !enrolled(&config.cloud) {
        return Err(Box::new(missing_reply(&Missing::NotLinked)));
    }
    let mut key_created = false;
    match key::load(&keys_dir(host)) {
        Ok(Some(_)) => {}
        Ok(None) => {
            key::create(&keys_dir(host)).map_err(|_| {
                Box::new(Reply::error(
                    "operation_failed",
                    1,
                    "The account content key could not be created.",
                ))
            })?;
            key_created = true;
        }
        Err(_) => return Err(Box::new(missing_reply(&Missing::Broken))),
    }
    let setup = open(host, &config.cloud).map_err(|m| Box::new(missing_reply(&m)))?;
    // The Worker must be reachable and speak this protocol before anything is queued.
    health(&setup.http.endpoint)
        .await
        .map_err(|failure| Box::new(failure_reply(&failure)))?;
    let local = HostLocal(host);
    let engine = Engine {
        local: &local,
        transport: &setup.http,
        ledger: &setup.ledger,
        content: &setup.content,
    };
    let internal = |_| {
        Box::new(Reply::error(
            "operation_failed",
            1,
            "The mirror could not run.",
        ))
    };
    let (mut uploaded, mut passes, mut refused) = (0u64, 0u32, 0u64);
    let mut millis = Vec::new();
    loop {
        let flushed = engine
            .flush(now(), Limits::default())
            .await
            .map_err(internal)?;
        passes += 1;
        uploaded += flushed.sent;
        refused += flushed.refused;
        millis.extend(flushed.batch_millis);
        if let Some(code) = flushed.failure {
            let failure = match code.as_str() {
                "revoked" => Failure::Revoked,
                "unconfigured" => Failure::Unconfigured,
                "quota" => Failure::Quota,
                _ => Failure::Offline,
            };
            return Err(Box::new(failure_reply(&failure)));
        }
        if flushed.batches == 0 || passes >= 200 {
            break;
        }
    }
    let outbox = setup.ledger.outbox(None, now()).map_err(internal)?;
    let report = match engine.verify().await.map_err(internal)? {
        Ok(report) => report,
        Err(failure) => return Err(Box::new(failure_reply(&failure))),
    };
    let _ = setup.ledger.set_meta(
        "last_verify",
        &json!({"at": now(), "ok": report.ok, "report": report}).to_string(),
    );
    let complete = report.ok && outbox.pending == outbox.blocked;
    if complete {
        let _ = setup
            .ledger
            .set_meta("initial_verified_at", &now().to_string());
        save_mode(host, CloudMode::Mirror)?;
    }
    let mut sorted = millis;
    Ok(Reply::success(json!({
        "action": "start",
        "mode": if complete { json!("mirror") } else { json!(config.cloud.mode) },
        "complete": complete,
        "key_created": key_created,
        "uploaded": uploaded,
        "refused": refused,
        "passes": passes,
        "outbox": outbox,
        "verify": report,
        "batch_ms_median": median(&mut sorted),
        "next": if key_created {
            "Back up the new account content key now: pta variables key backup --output PATH (keep the file apart from this profile)."
        } else if complete {
            "The account store now mirrors this installation; local stays the source of truth."
        } else {
            "The copy is not complete yet: run pta cloud mirror again (the outbox continues where it stopped)."
        },
    })))
}

pub(super) async fn operate_verify(host: &Arc<Host>) -> Reply {
    let _gate = host.state.cloud.gate.lock().await;
    let Ok(config) = host.state.settings.config() else {
        return Reply::error(
            "operation_failed",
            1,
            "The configuration could not be read.",
        );
    };
    let setup = match open(host, &config.cloud) {
        Ok(setup) => setup,
        Err(missing) => return missing_reply(&missing),
    };
    let local = HostLocal(host);
    let engine = Engine {
        local: &local,
        transport: &setup.http,
        ledger: &setup.ledger,
        content: &setup.content,
    };
    let started = Instant::now();
    let report = match engine.verify().await {
        Ok(Ok(report)) => report,
        Ok(Err(failure)) => return failure_reply(&failure),
        Err(_) => return Reply::error("operation_failed", 1, "The copy could not be verified."),
    };
    let _ = setup.ledger.set_meta(
        "last_verify",
        &json!({"at": now(), "ok": report.ok, "report": report}).to_string(),
    );
    Reply::success(json!({
        "ok": report.ok,
        "verify": report,
        "outbox": setup.ledger.outbox(None, now()).ok(),
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }))
}

/// What the mirror knows locally, for `cloud_status`: the key, the outbox and the last results.
/// Reads only this installation's files, so it works offline.
pub(super) fn describe(host: &Host, cloud: &Cloud) -> Value {
    let key = key::load(&keys_dir(host));
    let key_view = match &key {
        Ok(Some(content)) => json!({
            "present": true,
            "versions": content.backup().keys.keys().collect::<Vec<_>>(),
            "active": content.active,
            "fingerprint": crate::mirror::hex_sha256(
                format!("pta-cloud-key-fingerprint:{}", content.account).as_bytes())[..12].to_string(),
        }),
        Ok(None) => json!({"present": false}),
        Err(_) => json!({"present": false, "damaged": true}),
    };
    let path = ledger_path(host);
    let mut view = json!({"key": key_view, "mode": cloud.mode});
    if !path.exists() {
        return view;
    }
    let Ok(ledger) = Ledger::open(&path) else {
        return view;
    };
    let at = now();
    let meta = |name: &str| -> Value {
        ledger
            .meta(name)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null)
    };
    let mut collections = serde_json::Map::new();
    for collection in Collection::ALL {
        if let Ok(outbox) = ledger.outbox(Some(collection), at) {
            collections.insert(collection.name().into(), json!(outbox));
        }
    }
    view["outbox"] = json!(ledger.outbox(None, at).ok());
    view["outbox_collections"] = Value::Object(collections);
    view["initial_verified_at"] = meta("initial_verified_at");
    view["last_success_at"] = meta("last_success_at");
    view["last_error"] = ledger
        .meta("last_error")
        .ok()
        .flatten()
        .map_or(Value::Null, Value::String);
    view["last_pass"] = meta("last_pass");
    view["last_verify"] = meta("last_verify");
    view
}

#[cfg(test)]
mod tests;
