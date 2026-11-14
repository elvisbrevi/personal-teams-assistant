//! Local installation maintenance: update discovery, staging and unattended updating.
//!
//! A check is a bounded, read-only look at the pinned public registry. Staging builds the newest
//! release with Cargo (or, for an archive installation, downloads its release archive from this
//! package's repository) into a directory of this profile while the installed executables keep
//! running, then verifies the matched host/CLI pair; it replaces nothing and installs nothing.
//! Only the genuine local owner of the installation (the control endpoint's own token or the
//! local page, on a profile that is not a shared portal account) can check, stage or turn
//! unattended updating on; a portal session, a machine credential or an ordinary account only sees
//! that the server's updates are managed by its operator. The shared server's own timer
//! (`scripts/vps-update.py`) is independent of everything here.
use super::{
    authority::{self, Transport},
    control::{self, Reply},
    *,
};
use crate::maintenance::{self as core, Installation, Manifest, Version, apply as journal};
use serde_json::{Value, json};
use std::{collections::BTreeMap, ffi::OsString, sync::Mutex as StdMutex, time::Duration};

const SCHEMA: u32 = 1;
const DIRECTORY: &str = core::DIRECTORY;
/// Cargo's build cache for staging, kept between updates so a release recompiles only what changed.
const BUILD_CACHE: &str = "target";
/// Above this the cache is removed before the next build, which then compiles everything again.
const MAX_BUILD_CACHE: u64 = 8 << 30;
const AUTO_AFTER: u64 = 90;
/// How often the background loop looks: the registry only when a check is due, unattended updating
/// on every pass.
const AUTO_EVERY: u64 = 10 * 60;
/// A registry check is due this long after the last one.
const AUTO_DUE: i64 = 6 * 60 * 60;
/// An unattended staging that failed (a release whose archive is not published yet, a network
/// failure) is tried again after this pause, never in a tight loop.
const RETRY_STAGE: i64 = 60 * 60;
/// How many unattended-updating events the status keeps.
const HISTORY: usize = 50;
/// How long an update asked for before its release archive was attached waits for it, and how long
/// the background check keeps looking at a release that lacks it on every pass.
const ARCHIVE_WAIT: i64 = 30 * 60;
/// How often a waiting update looks at the release again.
const ARCHIVE_POLL: u64 = 60;
/// Why an unattended update waits while the assistant's Microsoft session needs renewal.
pub(super) const RENEWAL: &str = "The Microsoft session needs renewal, so the assistant would not start again after a restart. Sign in to Microsoft again (Settings > Microsoft Teams, or pta auth microsoft login); the update is installed afterwards.";
/// Why an unattended update waits for the person on a Mac whose credentials are in the Keychain.
const KEYCHAIN: &str = "On this Mac the Keychain asks to allow each new version of the application before it may read the saved credentials, so the prepared version waits for you: install it from Settings > Advanced > Updates (or pta update apply) and choose Always Allow when macOS asks.";
const MANAGED: &str =
    "Updates for this server are applied by its operator; there is nothing to install from here.";
const LOCAL_ONLY: &str = "Updating this installation needs the local owner of the computer: use the application (the local page pta app open shows) or pta on that computer.";
/// Why a Mac whose credentials are in the Keychain is not updated at another device's request.
const KEYCHAIN_REMOTE: &str = "On this Mac the Keychain asks to allow each new version of the application before it may read the saved credentials, so it is updated at the computer: run pta update there (or use Settings > Advanced > Updates) and choose Always Allow when macOS asks.";

pub(super) fn handles(method: &str) -> bool {
    method.starts_with("crate_update_")
}

/// The flag of the helper process that replaces the executables (see [`apply`]).
pub(crate) const HELPER_FLAG: &str = apply::FLAG;

/// What the host needs from its surroundings; tests replace it with synthetic ones.
#[derive(Clone)]
pub(super) struct Environment {
    pub registry: String,
    pub exe: PathBuf,
    pub variables: Vec<(OsString, OsString)>,
    pub running: Version,
    /// The flags this host was started with that a restart must repeat (`--headless`, `--host`,
    /// `--service`).
    pub launch: Vec<String>,
    /// Tests drive the helper themselves: nothing is spawned.
    pub manual: bool,
    /// Where release archives come from: this package's repository (tests use a local double).
    pub release: Option<core::release::Source>,
    /// Whether the Keychain holds this installation's credentials; `None` detects it (macOS).
    pub keychain: Option<bool>,
    /// How often a waiting update looks for its archive, and for how long (seconds).
    pub archive_poll: Duration,
    pub archive_wait: i64,
}

#[cfg(test)]
pub(super) static TEST: StdMutex<Option<Environment>> = StdMutex::new(None);

fn environment() -> Environment {
    #[cfg(test)]
    if let Some(environment) = TEST.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return environment;
    }
    Environment {
        registry: core::INDEX.into(),
        exe: std::env::current_exe().unwrap_or_default(),
        variables: std::env::vars_os().collect(),
        running: Version::parse(env!("CARGO_PKG_VERSION")).expect("a stable package version"),
        launch: std::env::args()
            .skip(1)
            .filter(|a| matches!(a.as_str(), "--headless" | "--host" | "--service"))
            .collect(),
        manual: false,
        release: core::release::Source::official().ok(),
        keychain: None,
        archive_poll: Duration::from_secs(ARCHIVE_POLL),
        archive_wait: ARCHIVE_WAIT,
    }
}

// ---------------------------------------------------------------------------------------------
// What is remembered.

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    schema: u32,
    checked_at: Option<i64>,
    /// The newest stable release the registry listed at the last successful check.
    latest: Option<String>,
    check_error: Option<String>,
    dismissed: Option<String>,
    stage: Option<Stage>,
    /// An apply request that waits for work in progress to end.
    deferred: Option<Deferred>,
    /// The per-computer choice of unattended updating. Never part of the account's portable
    /// configuration: only the local owner changes it, on this computer.
    unattended: Option<Unattended>,
    /// A release unattended updating does not try on its own: one whose installation failed and
    /// was rolled back, or whose unattended request the person cancelled.
    skip: Option<Skip>,
    /// Why the prepared release waits for the person instead of being installed unattended.
    waiting: Option<String>,
    /// The last update whose outcome was recorded.
    observed: Option<String>,
    /// The audit of unattended updating: changes of the setting and every attempt, newest last.
    history: Vec<Event>,
    /// Archive installations: the newest release whose archive for this computer's target was not
    /// in its release at the last check. It is not offered until the archive is there.
    archive_missing: Option<Missing>,
    /// An update asked for before its release archive was attached: prepared (and installed, when
    /// that was asked) once the archive appears, or ended after [`ARCHIVE_WAIT`].
    pending: Option<Pending>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Missing {
    version: String,
    /// When the archive was first found missing.
    since: i64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    version: String,
    since: i64,
    until: i64,
    /// Install once prepared (`pta update`, another device), not only prepare (`update stage`).
    #[serde(default)]
    install: bool,
    /// `cli`, `local_page` or `remote`.
    by: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Deferred {
    version: String,
    at: i64,
    reasons: Vec<String>,
    /// Requested by unattended updating rather than by the person.
    #[serde(default)]
    unattended: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Unattended {
    enabled: bool,
    changed_at: i64,
    /// `cli` or `local_page`.
    by: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Skip {
    version: String,
    /// `failed` or `cancelled`.
    reason: String,
    at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Event {
    at: i64,
    event: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    by: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Stage {
    version: String,
    /// `staging`, `staged` or `failed`.
    state: String,
    /// `building` or `verifying` while staging.
    phase: String,
    started_at: i64,
    finished_at: Option<i64>,
    crates: u64,
    current: Option<String>,
    error: Option<String>,
    tail: Vec<String>,
    /// Bytes of the release archive received and announced while downloading.
    #[serde(default)]
    received: u64,
    #[serde(default)]
    total: Option<u64>,
}

/// Progress of a build in this process, by profile.
static LIVE: StdMutex<BTreeMap<PathBuf, Stage>> = StdMutex::new(BTreeMap::new());
/// Serializes read-modify-write of a profile's saved state.
static SAVING: StdMutex<()> = StdMutex::new(());

fn live() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Stage>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

fn directory(host: &Host) -> PathBuf {
    host.state.settings.profile().join(DIRECTORY)
}

fn load(dir: &Path) -> Saved {
    fs::read_to_string(dir.join("status.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save(dir: &Path, edit: impl FnOnce(&mut Saved)) -> Result<()> {
    let _guard = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    security::private_dir(dir)?;
    let mut saved = load(dir);
    saved.schema = SCHEMA;
    edit(&mut saved);
    write_private(&dir.join("status.json"), &serde_json::to_string(&saved)?)
}

/// Appends to the audit of unattended updating (bounded) and to the log.
fn audit(
    dir: &Path,
    event: &str,
    version: Option<Version>,
    detail: Option<String>,
    by: Option<&str>,
) {
    tracing::info!(event = "update_audit", kind = event, version = ?version.map(|v| v.to_string()));
    let _ = save(dir, |saved| {
        saved.history.push(Event {
            at: now(),
            event: event.to_owned(),
            version: version.map(|v| v.to_string()),
            detail,
            by: by.map(str::to_owned),
        });
        let extra = saved.history.len().saturating_sub(HISTORY);
        saved.history.drain(..extra);
    });
}

fn unattended_on(saved: &Saved) -> bool {
    saved.unattended.as_ref().is_some_and(|u| u.enabled)
}

fn staged_dir(dir: &Path, version: &str) -> PathBuf {
    dir.join("staged").join(version)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Whether this call is the genuine local owner of the installation.
pub(super) fn is_local_owner(scope: &authority::Scope, transport: Transport) -> bool {
    matches!(transport, Transport::Local | Transport::Window) && scope.owner && !scope.shared
}

// ---------------------------------------------------------------------------------------------
// Reading the state.

/// The newest release that is worth offering: newer than both the installed and the running one
/// and, for an archive installation, with its archive already in the release.
fn available(saved: &Saved, running: Version, installed: Option<Version>) -> Option<Version> {
    let baseline = installed.map_or(running, |i| i.max(running));
    saved
        .latest
        .as_deref()
        .and_then(Version::parse)
        .filter(|l| *l > baseline)
        .filter(|l| {
            saved
                .archive_missing
                .as_ref()
                .is_none_or(|m| m.version != l.to_string())
        })
}

/// A newer release that is published but not offered yet: its archive for this computer is not in
/// the release.
fn late(saved: &Saved, running: Version, installed: Option<Version>) -> Option<Version> {
    let baseline = installed.map_or(running, |i| i.max(running));
    saved
        .archive_missing
        .as_ref()
        .and_then(|m| Version::parse(&m.version))
        .filter(|v| *v > baseline)
}

fn stage_json(stage: &Stage) -> Value {
    json!({
        "version": stage.version, "state": stage.state, "phase": stage.phase,
        "started_at": stage.started_at, "finished_at": stage.finished_at,
        "progress": {"crates": stage.crates, "current": stage.current,
            "received": stage.received, "total": stage.total},
        "error": stage.error, "tail": stage.tail,
    })
}

/// The manifest of a staged pair that is still complete and still worth installing.
fn staged(dir: &Path, saved: &Saved, wanted: Option<Version>) -> Option<Manifest> {
    let stage = saved.stage.as_ref().filter(|s| s.state == "staged")?;
    let version = Version::parse(&stage.version)?;
    wanted.filter(|w| *w == version)?;
    let base = staged_dir(dir, &stage.version);
    let manifest: Manifest =
        serde_json::from_str(&fs::read_to_string(base.join("manifest.json")).ok()?).ok()?;
    core::present(&base.join("root"), &manifest).then_some(manifest)
}

/// The full local view: versions, installation, check and staging state.
fn status(host: &Host, env: &Environment) -> Value {
    let dir = directory(host);
    // An update whose driver died is undone before it is reported.
    let _ = journal::recover(&dir, now());
    let saved = load(&dir);
    let last = journal::read(&dir);
    let applying = last.as_ref().is_some_and(|j| !j.phase.finished());
    let installation = core::inspect(&env.exe);
    let installed = installation.as_ref().ok().map(|i| i.version);
    let offer = available(&saved, env.running, installed);
    let late = late(&saved, env.running, installed);
    let in_flight = live().get(dir.parent().unwrap()).cloned();
    // A build recorded as running by a process that is gone was interrupted.
    let recorded = match (&in_flight, &saved.stage) {
        (Some(stage), _) => Some(stage.clone()),
        (None, Some(stage)) if stage.state == "staging" => Some(Stage {
            state: "failed".into(),
            error: Some(
                "The application stopped before the build finished. Prepare the update again."
                    .into(),
            ),
            ..stage.clone()
        }),
        (None, stage) => stage.clone(),
    };
    let manifest = staged(&dir, &saved, offer);
    let state = match (
        &in_flight,
        &manifest,
        recorded.as_ref().map(|s| s.state.as_str()),
    ) {
        _ if applying => "applying",
        (Some(_), ..) => "staging",
        (None, ..) if saved.pending.is_some() => "waiting",
        (None, Some(_), _) if saved.deferred.is_some() => "deferred",
        (None, Some(_), _) => "staged",
        (None, None, Some("failed")) => "failed",
        _ => "idle",
    };
    // The last update's outcome stays visible: a success for a day, a rollback or failure until the
    // release is dismissed. Completion is only ever what the journal says, never the installed version.
    let result = last
        .as_ref()
        .filter(|j| {
            !j.phase.finished()
                || match j.phase {
                    journal::Phase::Committed => now() - j.finished_at.unwrap_or(0) < 24 * 60 * 60,
                    _ => saved.dismissed.as_deref() != Some(j.to.as_str()),
                }
        })
        .map(journal::describe);
    let unsupported = apply::unsupported(env);
    let can_apply = installation.is_ok()
        && manifest.is_some()
        && !applying
        && unsupported.is_none()
        && matches!(state, "staged" | "deferred");
    let install = match &installation {
        Ok(i) => json!({"supported":true,"kind":kind(i),
            "features":i.features,"all_features":i.all_features,
            "no_default_features":i.no_default_features,"target":i.target,"prebuilt":i.prebuilt}),
        Err(diagnostic) => json!({"supported":false,"diagnostic":diagnostic}),
    };
    let unattended = json!({
        "enabled": unattended_on(&saved),
        "available": installation.is_ok() && unsupported.is_none(),
        "changed_at": saved.unattended.as_ref().map(|u| u.changed_at),
        "changed_by": saved.unattended.as_ref().map(|u| u.by.clone()),
        "waiting": saved.waiting.as_ref().filter(|_| unattended_on(&saved) && state == "staged"),
        "skipped": saved.skip.as_ref().filter(|s| offer.is_some_and(|o| o.to_string() == s.version))
            .map(|s| json!({"version": s.version, "reason": s.reason, "at": s.at})),
        "history": saved.history.iter().rev().take(10).map(|e| json!({
            "at": e.at, "event": e.event, "version": e.version, "detail": e.detail, "by": e.by,
        })).collect::<Vec<_>>(),
    });
    // A failure about a release that is no longer on offer (or waited for) is stale.
    let shown = recorded.filter(|s| {
        s.state == "staging" || offer.or(late).is_some_and(|o| o.to_string() == s.version)
    });
    json!({
        "managed": "local",
        "state": state,
        "running": env.running.to_string(),
        "installed": installed.map(|v| v.to_string()),
        "available": offer.map(|v| v.to_string()),
        "restart_required": installed.is_some_and(|i| i > env.running),
        "checked_at": saved.checked_at,
        "check_error": saved.check_error,
        "dismissed": offer.is_some_and(|o| saved.dismissed.as_deref() == Some(&o.to_string())),
        "install": install,
        // A release whose archive is not attached yet can be asked for: it is prepared once it is.
        "can_stage": installation.is_ok() && offer.or(late).is_some()
            && !matches!(state, "staging" | "applying" | "waiting"),
        "can_apply": can_apply,
        "apply_unsupported": unsupported,
        "apply": result,
        "deferred": saved.deferred.as_ref().filter(|_| state == "deferred").map(|d| json!({
            "version": d.version, "since": d.at, "reasons": d.reasons, "unattended": d.unattended,
        })),
        "unattended": unattended,
        "archive_missing": late.zip(saved.archive_missing.as_ref()).map(|(v, m)| json!({
            "version": v.to_string(), "since": m.since,
        })),
        "waiting_for_archive": saved.pending.as_ref().filter(|_| state == "waiting").map(|p| json!({
            "version": p.version, "since": p.since, "until": p.until, "install": p.install,
            "message": waiting_text(&p.version),
        })),
        "stage": shown.as_ref().map(stage_json),
        "staged": manifest.map(|m| json!({
            "version": m.version, "staged_at": m.staged_at, "features": m.features,
            "no_default_features": m.no_default_features, "target": m.target,
            "binaries": m.binaries.keys().collect::<Vec<_>>(),
        })),
    })
}

/// What a waiting update says about itself.
fn waiting_text(version: &str) -> String {
    format!(
        "Waiting for the archive of {version}: the release is published, but its archive for this computer is not attached yet. The update goes ahead by itself when it appears, for up to {} minutes.",
        ARCHIVE_WAIT / 60
    )
}

/// How a wait for an archive that never appeared ends.
fn never_text(version: &str, target: &str) -> String {
    format!(
        "The archive of version {version} for this computer ({target}) did not appear in its release within {} minutes, so nothing was prepared or installed. Look at the release's assets at {}/releases/tag/v{version} (re-running its binaries workflow attaches a missing archive), then update again.",
        ARCHIVE_WAIT / 60,
        core::release::REPOSITORY.trim_end_matches('/'),
    )
}

fn kind(installation: &Installation) -> &'static str {
    if installation.archive {
        "archive"
    } else if installation.prebuilt {
        "binstall"
    } else {
        "cargo"
    }
}

/// What a worker reports to its coordinator with each poll: the update state of this computer,
/// from local files only (no network, nothing undone). Versions only; never a path or an error
/// text. The portal shows it and cannot act on it.
pub(super) fn report(host: &Host) -> Value {
    // A shared portal account's host is the operator's server: its updates are not the account's.
    if !is_local_owner(&host.state.authority, Transport::Local) {
        return Value::Null;
    }
    let env = environment();
    let dir = directory(host);
    let saved = load(&dir);
    let Ok(installation) = core::inspect(&env.exe) else {
        return json!({"state": "unsupported", "pending": null, "failed": null,
            "unattended": unattended_on(&saved)});
    };
    let offer = available(&saved, env.running, Some(installation.version));
    let waiting = saved
        .pending
        .as_ref()
        .and_then(|p| Version::parse(&p.version));
    let last = journal::read(&dir);
    // A wait for the release archive reads as preparing: the fixed states every coordinator knows.
    let state = if last.as_ref().is_some_and(|j| !j.phase.finished()) {
        "applying"
    } else if live().contains_key(dir.parent().unwrap()) || waiting.is_some() {
        "staging"
    } else if saved.deferred.is_some() && offer.is_some() {
        "deferred"
    } else if staged(&dir, &saved, offer).is_some() {
        "staged"
    } else if offer.is_some() {
        "available"
    } else {
        "current"
    };
    // A failed installation of a release that is still newer than what runs.
    let failed = saved
        .skip
        .as_ref()
        .filter(|s| s.reason == "failed")
        .map(|s| s.version.clone())
        .or_else(|| {
            last.filter(|j| matches!(j.phase, journal::Phase::RolledBack | journal::Phase::Failed))
                .map(|j| j.to)
        })
        .filter(|v| Version::parse(v).is_some_and(|v| v > installation.version.max(env.running)));
    json!({"state": state, "pending": offer.or(waiting).map(|v| v.to_string()), "failed": failed,
        "unattended": unattended_on(&saved)})
}

/// What a portal session or an ordinary account sees: only that the server is managed.
fn managed(env: &Environment) -> Value {
    json!({
        "managed": "server", "running": env.running.to_string(), "can_stage": false,
        "message": MANAGED,
    })
}

// ---------------------------------------------------------------------------------------------
// Operations.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionArgs {
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnattendedArgs {
    enabled: bool,
}

/// Who asked to update to the newest release, as its audit records it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum By {
    Cli,
    LocalPage,
    /// Another device of the account, through the coordinator.
    Remote,
}
impl By {
    fn label(self) -> &'static str {
        match self {
            By::Cli => "cli",
            By::LocalPage => "local_page",
            By::Remote => "remote",
        }
    }
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Reply {
    let Ok(invocation) = authority::invocation(&host.state) else {
        return Reply::error("authority_denied", 6, authority::ACCOUNT_UNAVAILABLE);
    };
    let env = environment();
    let local = is_local_owner(&invocation.scope, invocation.transport);
    match request.method.as_str() {
        "crate_update_status" if !local => Reply::success(managed(&env)),
        "crate_update_status" => Reply::success(status(host, &env)),
        _ if !local => Reply::error("authority_denied", 6, LOCAL_ONLY),
        "crate_update_check" => match serde_json::from_value::<NoArgs>(request.args.clone()) {
            Ok(_) => check(host, &env).await,
            Err(_) => Reply::error("invalid_input", 2, "This operation takes no arguments."),
        },
        "crate_update_stage" => match serde_json::from_value::<VersionArgs>(request.args.clone()) {
            Ok(args) => {
                let by = if invocation.transport == Transport::Window {
                    By::LocalPage
                } else {
                    By::Cli
                };
                stage(host, &env, &args.version, Some(by.label())).await
            }
            Err(_) => Reply::error(
                "invalid_input",
                2,
                "Give the version to prepare, as returned by the update check.",
            ),
        },
        "crate_update_apply" => match serde_json::from_value::<VersionArgs>(request.args.clone()) {
            Ok(args) => apply::request(host, &env, &args.version, false).await,
            Err(_) => Reply::error(
                "invalid_input",
                2,
                "Give the prepared version to install, as shown by the update status.",
            ),
        },
        "crate_update_latest" => match serde_json::from_value::<NoArgs>(request.args.clone()) {
            Ok(_) => {
                let by = if invocation.transport == Transport::Window {
                    By::LocalPage
                } else {
                    By::Cli
                };
                latest(host, &env, by).await
            }
            Err(_) => Reply::error("invalid_input", 2, "This operation takes no arguments."),
        },
        "crate_update_cancel" => match serde_json::from_value::<NoArgs>(request.args.clone()) {
            Ok(_) => apply::cancel(host, &env),
            Err(_) => Reply::error("invalid_input", 2, "This operation takes no arguments."),
        },
        "crate_update_unattended" => {
            match serde_json::from_value::<UnattendedArgs>(request.args.clone()) {
                Ok(args) => {
                    let by = if invocation.transport == Transport::Window {
                        "local_page"
                    } else {
                        "cli"
                    };
                    set_unattended(host, &env, args.enabled, by)
                }
                Err(_) => Reply::error(
                    "invalid_input",
                    2,
                    "Say whether unattended updating is on (enabled: true or false).",
                ),
            }
        }
        "crate_update_dismiss" => match serde_json::from_value::<VersionArgs>(request.args.clone())
        {
            Ok(args) => dismiss(host, &env, &args.version),
            Err(_) => Reply::error(
                "invalid_input",
                2,
                "Give the version whose notice to dismiss.",
            ),
        },
        _ => Reply::error("invalid_input", 2, "Unknown update operation."),
    }
}

/// Whether the release of `version` holds this computer's archive yet: `None` when that does not
/// matter (not an archive installation, or nothing newer), otherwise the answer of the release host.
async fn archive_published(
    env: &Environment,
    version: Version,
) -> Option<std::result::Result<bool, String>> {
    let installation = core::inspect(&env.exe).ok().filter(|i| i.archive)?;
    if version <= env.running.max(installation.version) {
        return None;
    }
    let source = env.release.as_ref()?;
    let target = installation.target.as_deref()?;
    Some(
        core::release::published(source, &version, target)
            .await
            .map_err(|e| e.to_string()),
    )
}

/// Reads the registry and remembers what it listed and, for an archive installation, whether the
/// newest release's archive is attached yet. Never installs or stages anything.
async fn refresh(
    host: &Host,
    env: &Environment,
) -> std::result::Result<Option<core::Release>, String> {
    let result = async {
        let body = core::fetch_index(&env.registry).await?;
        core::newest_stable(&body)
    }
    .await;
    let archive = match &result {
        Ok(Some(release)) => archive_published(env, release.version).await,
        _ => None,
    };
    let dir = directory(host);
    let at = now();
    let outcome = match &result {
        Ok(release) => Ok(release.as_ref().map(|r| r.version.to_string())),
        Err(error) => Err(error.to_string()),
    };
    let probe_error = archive.as_ref().and_then(|a| a.as_ref().err().cloned());
    let _ = save(&dir, |saved| {
        saved.checked_at = Some(at);
        match &outcome {
            Ok(latest) => {
                saved.latest = latest.clone();
                saved.check_error = probe_error.clone();
                // Not known to be attached (missing, or the release host did not answer): not
                // offered. The first time it was missing is kept, so waits stay bounded.
                saved.archive_missing = match (&archive, latest) {
                    (Some(Ok(true)) | None, _) | (_, None) => None,
                    (Some(_), Some(version)) => Some(Missing {
                        version: version.clone(),
                        since: saved
                            .archive_missing
                            .as_ref()
                            .filter(|m| &m.version == version)
                            .map_or(at, |m| m.since),
                    }),
                };
            }
            Err(message) => saved.check_error = Some(message.clone()),
        }
    });
    match (result, probe_error) {
        (Ok(_), Some(message)) => Err(message),
        (result, _) => result.map_err(|e| e.to_string()),
    }
}

async fn check(host: &Host, env: &Environment) -> Reply {
    match refresh(host, env).await {
        Ok(_) => Reply::success(status(host, env)),
        Err(message) => Reply::error("dependency_or_network", 5, &message),
    }
}

fn dismiss(host: &Host, env: &Environment, version: &str) -> Reply {
    let Some(version) = Version::parse(version) else {
        return Reply::error("invalid_input", 2, "That is not a release version.");
    };
    match save(&directory(host), |saved| {
        saved.dismissed = Some(version.to_string())
    }) {
        Ok(()) => Reply::success(status(host, env)),
        Err(_) => Reply::error("operation_failed", 1, "The choice could not be saved."),
    }
}

/// Turns unattended updating on or off for this computer. Only the local owner reaches this: the
/// setting is never synchronized, and a portal session, a machine credential or a remote agent is
/// refused before it.
fn set_unattended(host: &Host, env: &Environment, enabled: bool, by: &str) -> Reply {
    if enabled {
        if let Err(diagnostic) = core::inspect(&env.exe) {
            return Reply::error("not_ready", 3, &diagnostic);
        }
        if let Some(diagnostic) = apply::unsupported(env) {
            return Reply::error("not_ready", 3, diagnostic);
        }
    }
    let dir = directory(host);
    let changed = unattended_on(&load(&dir)) != enabled;
    let saved = save(&dir, |saved| {
        saved.unattended = Some(Unattended {
            enabled,
            changed_at: now(),
            by: by.to_owned(),
        });
        if !enabled {
            saved.waiting = None;
            // What unattended updating asked for and still waits is withdrawn with it.
            if saved.deferred.as_ref().is_some_and(|d| d.unattended) {
                saved.deferred = None;
            }
        }
    });
    if saved.is_err() {
        return Reply::error("operation_failed", 1, "The choice could not be saved.");
    }
    if changed {
        audit(
            &dir,
            if enabled {
                "unattended_on"
            } else {
                "unattended_off"
            },
            None,
            None,
            Some(by),
        );
    }
    Reply::success(status(host, env))
}

/// Where Cargo (or one of its tools) is: the build's own `PATH` first, then the place `rustup`
/// and `cargo install` put them.
fn find_tool(variables: &[(OsString, OsString)], name: &str) -> Option<PathBuf> {
    let get = |name: &str| {
        variables
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    core::find_program(name, get("PATH").as_deref()).or_else(|| {
        let home = get("CARGO_HOME").map(PathBuf::from).or_else(|| {
            get("HOME")
                .or_else(|| get("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".cargo"))
        })?;
        core::find_program(name, Some(home.join("bin").as_os_str()))
    })
}

/// Removes this profile's entry from `LIVE` however the build ends.
struct Building(PathBuf);
impl Drop for Building {
    fn drop(&mut self) {
        live().remove(&self.0);
    }
}

/// Prepares `requested`, which must be the newest release. When its archive is not attached yet,
/// `wait` (who asked) keeps the request until it is; without it, nothing starts.
async fn stage(host: &Arc<Host>, env: &Environment, requested: &str, wait: Option<&str>) -> Reply {
    let Some(version) = Version::parse(requested) else {
        return Reply::error("invalid_input", 2, "That is not a release version.");
    };
    let installation = match core::inspect(&env.exe) {
        Ok(installation) => installation,
        Err(diagnostic) => return Reply::error("not_ready", 3, &diagnostic),
    };
    let dir = directory(host);
    if journal::read(&dir).is_some_and(|j| !j.phase.finished()) {
        return Reply::error(
            "state_conflict",
            6,
            "An update is being applied. Wait for it to finish.",
        );
    }
    let profile = dir.parent().unwrap().to_path_buf();
    let started = now();
    // One build at a time per profile.
    {
        let mut running = live();
        if running.contains_key(&profile) {
            return Reply::error(
                "state_conflict",
                6,
                "An update is already being prepared. Wait for it to finish.",
            );
        }
        running.insert(
            profile.clone(),
            Stage {
                version: version.to_string(),
                state: "staging".into(),
                phase: "checking".into(),
                started_at: started,
                finished_at: None,
                crates: 0,
                current: None,
                error: None,
                tail: Vec::new(),
                received: 0,
                total: None,
            },
        );
    }
    let building = Building(profile.clone());
    // The version must be the newest one the registry lists right now, never a caller's choice.
    let release = match refresh(host, env).await {
        Ok(Some(release)) => release,
        Ok(None) => {
            return Reply::error("state_conflict", 6, "The registry lists no stable release.");
        }
        Err(message) => return Reply::error("dependency_or_network", 5, &message),
    };
    if release.version != version {
        return Reply::error(
            "state_conflict",
            6,
            &format!(
                "The newest release is {}, not {version}. Check for updates again.",
                release.version
            ),
        );
    }
    if version <= env.running.max(installation.version) {
        return Reply::error("state_conflict", 6, "That version is already installed.");
    }
    if late(&load(&dir), env.running, Some(installation.version)) == Some(version) {
        drop(building);
        let Some(by) = wait else {
            return Reply::error(
                "not_ready",
                3,
                &format!(
                    "The archive of version {version} for this computer is not in its release yet, so nothing was staged."
                ),
            );
        };
        wait_for_archive(host, env, version, false, by);
        return Reply::success(status(host, env));
    }
    let (cargo, source, build_env) = if installation.archive {
        // An archive installation downloads the release's own archive: no Cargo, no compiler.
        let Some(source) = env.release.clone() else {
            return Reply::error(
                "not_ready",
                3,
                "This package's release address could not be derived, so the update cannot be downloaded. Run the install command again to upgrade.",
            );
        };
        (
            None,
            Some(source),
            core::build_environment(env.variables.clone()),
        )
    } else {
        match cargo_checks(env, &installation, &release, version).await {
            Ok((cargo, build_env)) => (Some(cargo), None, build_env),
            Err(reply) => return *reply,
        }
    };
    // Only one prepared pair is kept: this build replaces any earlier one.
    let partial = dir.join("staged").join(format!("{version}.partial"));
    let prepared = security::private_dir(&dir)
        .and_then(|()| security::private_dir(&dir.join("staged")))
        .and_then(|()| {
            for entry in fs::read_dir(dir.join("staged"))? {
                fs::remove_dir_all(entry?.path())?;
            }
            fs::create_dir_all(partial.join("root"))?;
            Ok(())
        });
    if prepared.is_err() {
        return Reply::error(
            "operation_failed",
            1,
            "The staging directory could not be prepared.",
        );
    }
    let stage = {
        let mut running = live();
        let stage = running.get_mut(&profile).expect("registered above");
        stage.phase = if source.is_some() {
            "downloading"
        } else {
            "building"
        }
        .into();
        stage.clone()
    };
    let _ = save(&dir, |saved| saved.stage = Some(stage));
    // The task owns the registration until it finishes.
    let work = Work {
        dir,
        profile,
        version,
        installation,
        cargo,
        source,
        build_env,
        partial,
        started,
        _building: building,
    };
    tokio::spawn(work.run());
    Reply::success(status(host, env))
}

/// Cargo and the build environment for a Cargo or `cargo binstall` installation, or why the build
/// cannot start.
async fn cargo_checks(
    env: &Environment,
    installation: &Installation,
    release: &core::Release,
    version: Version,
) -> std::result::Result<(PathBuf, Vec<(OsString, OsString)>), Box<Reply>> {
    let Some(cargo) = find_tool(&env.variables, "cargo") else {
        return Err(Box::new(Reply::error(
            "not_ready",
            3,
            "Cargo was not found. Install the Rust toolchain from https://rustup.rs and try again.",
        )));
    };
    if installation.prebuilt && find_tool(&env.variables, "cargo-binstall").is_none() {
        return Err(Box::new(Reply::error(
            "not_ready",
            3,
            "This installation came from `cargo binstall`, which was not found. Install cargo-binstall again and try again.",
        )));
    }
    let mut build_env = core::build_environment(env.variables.clone());
    if installation.prebuilt {
        // Fetching the release's own archive reports nothing to third parties.
        build_env.push(("BINSTALL_DISABLE_TELEMETRY".into(), "true".into()));
    }
    match core::cargo_version(&cargo, &build_env).await {
        Err(message) => return Err(Box::new(Reply::error("not_ready", 3, &message))),
        Ok(have) => {
            // A prebuilt archive needs no compiler of the release's own age.
            let need = release.rust_version.filter(|_| !installation.prebuilt);
            if let Some(need) = need.filter(|need| have < *need) {
                return Err(Box::new(Reply::error(
                    "not_ready",
                    3,
                    &format!(
                        "Version {version} needs Rust {need} or newer; this computer has {have}. Run `rustup update` and try again."
                    ),
                )));
            }
        }
    }
    Ok((cargo, build_env))
}

/// One staging build, run in the background.
struct Work {
    dir: PathBuf,
    profile: PathBuf,
    version: Version,
    installation: Installation,
    /// Cargo, for a Cargo or `cargo binstall` installation.
    cargo: Option<PathBuf>,
    /// The release source, for an archive installation.
    source: Option<core::release::Source>,
    build_env: Vec<(OsString, OsString)>,
    partial: PathBuf,
    started: i64,
    _building: Building,
}

impl Work {
    fn update(&self, edit: impl FnOnce(&mut Stage)) {
        if let Some(stage) = live().get_mut(&self.profile) {
            edit(stage);
        }
    }

    async fn run(self) {
        let outcome = match (&self.cargo, &self.source) {
            (_, Some(source)) => self.download(source).await,
            (Some(cargo), None) => self.build(cargo).await,
            (None, None) => Err(("Nothing can prepare this update.".to_owned(), Vec::new())),
        };
        let finished = now();
        let (state, error, tail) = match &outcome {
            Ok(()) => ("staged", None, Vec::new()),
            Err((reason, tail)) => {
                let _ = fs::remove_dir_all(&self.partial);
                ("failed", Some(reason.clone()), tail.clone())
            }
        };
        let progress = live().get(&self.profile).cloned();
        let _ = save(&self.dir, |saved| {
            saved.stage = Some(Stage {
                version: self.version.to_string(),
                state: state.into(),
                phase: "done".into(),
                started_at: self.started,
                finished_at: Some(finished),
                crates: progress.as_ref().map_or(0, |p| p.crates),
                current: progress.as_ref().and_then(|p| p.current.clone()),
                error,
                tail,
                received: progress.as_ref().map_or(0, |p| p.received),
                total: progress.and_then(|p| p.total),
            });
        });
    }

    /// Downloads, checks and unpacks the release archive for the installation's own target.
    async fn download(
        &self,
        source: &core::release::Source,
    ) -> std::result::Result<(), (String, Vec<String>)> {
        let target = self.installation.target.clone().unwrap_or_default();
        let manifest = core::release::stage(
            source,
            &self.version,
            &target,
            &self.partial,
            &self.build_env,
            now(),
            |received, total| {
                self.update(|stage| {
                    stage.received = received;
                    stage.total = total;
                })
            },
        )
        .await
        .map_err(|e| (e.to_string(), Vec::new()))?;
        self.place(&manifest)
            .map_err(|e| (e.to_string(), Vec::new()))
    }

    async fn build(&self, cargo: &Path) -> std::result::Result<(), (String, Vec<String>)> {
        let root = self.partial.join("root");
        let cache = self.dir.join(BUILD_CACHE);
        let trimmed = cache.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if core::tree_size(&trimmed) > MAX_BUILD_CACHE {
                let _ = fs::remove_dir_all(&trimmed);
            }
            security::private_dir(&trimmed)
        })
        .await;
        let args = core::install_args(&self.installation, &self.version, &root, &cache);
        let built = core::build(
            cargo,
            &args,
            &self.build_env,
            &self.dir,
            Duration::from_secs(core::BUILD_SECONDS),
            |progress| {
                self.update(|stage| {
                    stage.crates = progress.crates;
                    stage.current = progress.current.clone();
                })
            },
        )
        .await;
        match built {
            Err(failure) => {
                // A full disk is the one failure the cache itself may cause: give the space back.
                if failure
                    .tail
                    .iter()
                    .any(|line| line.contains("No space left on device"))
                {
                    let _ = tokio::task::spawn_blocking(move || fs::remove_dir_all(cache)).await;
                }
                Err((failure.reason, failure.tail))
            }
            Ok(()) => {
                self.update(|stage| stage.phase = "verifying".into());
                let manifest = core::verify(
                    &root,
                    &self.version,
                    &self.installation,
                    &self.build_env,
                    now(),
                )
                .await
                .map_err(|e| (e.to_string(), Vec::new()))?;
                self.place(&manifest)
                    .map_err(|e| (e.to_string(), Vec::new()))
            }
        }
    }

    /// Stores the manifest of a verified pair and moves it to its final place.
    fn place(&self, manifest: &Manifest) -> Result<()> {
        write_private(
            &self.partial.join("manifest.json"),
            &serde_json::to_string(manifest)?,
        )?;
        let finished = staged_dir(&self.dir, &self.version.to_string());
        fs::rename(&self.partial, &finished)
            .context("The staged pair could not be put in place.")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The daily check.

/// Looks for a newer release now and then on the owner's own installation. Read-only unless the
/// owner turned unattended updating on for this computer: then a newer release is prepared and
/// installed when nothing would be interrupted.
pub(super) async fn schedule(host: Arc<Host>) {
    if !is_local_owner(&host.state.authority, Transport::Local) {
        return;
    }
    // A wait for a release archive that a restart interrupted goes on, within its bound.
    if load(&directory(&host)).pending.is_some() {
        watch(host.clone(), environment());
    }
    tokio::join!(daily(host.clone()), apply::retry_deferred(host));
}

async fn daily(host: Arc<Host>) {
    // After the first pause, so the helper of an update that just restarted this host has finished.
    tokio::time::sleep(Duration::from_secs(AUTO_AFTER)).await;
    tidy(&host);
    loop {
        let env = environment();
        let saved = load(&directory(&host));
        // A release whose archive is not attached yet is looked at again on every pass, for a while.
        let due = saved.checked_at.is_none_or(|at| now() - at > AUTO_DUE)
            || saved
                .archive_missing
                .as_ref()
                .is_some_and(|m| now() - m.since < env.archive_wait);
        if due && core::inspect(&env.exe).is_ok() {
            let _ = refresh(&host, &env).await;
        }
        let _ = unattended(&host, &env).await;
        tidy(&host);
        tokio::time::sleep(Duration::from_secs(AUTO_EVERY)).await;
    }
}

/// What one pass of unattended updating did.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Pass {
    Off,
    Unsupported,
    /// An update or a staging is in progress.
    Busy,
    Current,
    /// The release failed before, or the person cancelled its unattended request.
    Skipped,
    /// A failed staging of this release waits before it is tried again.
    Pausing,
    /// The newest release's archive is not attached yet: nothing is tried or recorded meanwhile.
    Waiting,
    Staging,
    /// Waiting for work in progress (or the Microsoft session) before installing.
    Deferred,
    /// Waiting for the person: installing would ask the Keychain.
    Interactive,
    Applying,
}

/// Whether installing a new version here would make the Keychain ask the person.
fn keychain(env: &Environment) -> bool {
    env.keychain.unwrap_or_else(security::keychain_in_use)
}

/// Records, once, how the last update ended. A version whose installation was rolled back or failed
/// is not tried again on its own.
fn observe(dir: &Path, last: Option<&journal::Journal>) {
    let Some(last) = last.filter(|j| j.phase.finished()) else {
        return;
    };
    if load(dir).observed.as_deref() == Some(last.id.as_str()) {
        return;
    }
    let failed = last.phase != journal::Phase::Committed;
    let _ = save(dir, |saved| {
        saved.observed = Some(last.id.clone());
        if failed {
            saved.skip = Some(Skip {
                version: last.to.clone(),
                reason: "failed".into(),
                at: now(),
            });
        }
    });
    if last.intent.unattended {
        let outcome = match last.phase {
            journal::Phase::Committed => "installed",
            journal::Phase::RolledBack => "rolled_back",
            _ => "failed",
        };
        audit(
            dir,
            "unattended_result",
            Version::parse(&last.to),
            Some(
                last.error
                    .as_ref()
                    .map_or_else(|| outcome.to_owned(), |e| format!("{outcome}: {e}")),
            ),
            None,
        );
    }
}

/// One pass of unattended updating: prepare the newest release, then ask to install it through
/// the ordinary request, which defers while work is in progress. Nothing happens unless the local
/// owner turned it on for this computer.
pub(super) async fn unattended(host: &Arc<Host>, env: &Environment) -> Pass {
    let dir = directory(host);
    if !unattended_on(&load(&dir)) {
        return Pass::Off;
    }
    let Ok(installation) = core::inspect(&env.exe) else {
        return Pass::Unsupported;
    };
    if apply::unsupported(env).is_some() {
        return Pass::Unsupported;
    }
    let _ = journal::recover(&dir, now());
    let last = journal::read(&dir);
    if last.as_ref().is_some_and(|j| !j.phase.finished()) {
        return Pass::Busy;
    }
    observe(&dir, last.as_ref());
    let saved = load(&dir);
    // A person's update waits for the release archive: its own task prepares it.
    if saved.pending.is_some() {
        return Pass::Waiting;
    }
    let Some(offer) = available(&saved, env.running, Some(installation.version)) else {
        return if late(&saved, env.running, Some(installation.version)).is_some() {
            Pass::Waiting
        } else {
            Pass::Current
        };
    };
    if saved
        .skip
        .as_ref()
        .is_some_and(|s| s.version == offer.to_string())
    {
        return Pass::Skipped;
    }
    if live().contains_key(dir.parent().unwrap()) {
        return Pass::Busy;
    }
    if staged(&dir, &saved, Some(offer)).is_none() {
        let paused = saved.stage.as_ref().is_some_and(|s| {
            s.version == offer.to_string()
                && s.state == "failed"
                && s.finished_at.is_some_and(|at| now() - at < RETRY_STAGE)
        });
        if paused {
            return Pass::Pausing;
        }
        let reply = stage(host, env, &offer.to_string(), None).await;
        if !reply.ok {
            // Refused before it began (the registry unreachable, a newer release meanwhile): the
            // same pause as a failed download, so a lasting problem is not retried every pass.
            let message = reply.message.clone();
            let _ = save(&dir, |saved| {
                saved.stage = Some(failed_stage(offer, now(), message));
            });
        }
        audit(
            &dir,
            "unattended_stage",
            Some(offer),
            (!reply.ok).then(|| reply.message.clone().unwrap_or_default()),
            None,
        );
        return Pass::Staging;
    }
    if saved.deferred.is_some() {
        return Pass::Deferred;
    }
    if keychain(env) {
        if saved.waiting.as_deref() != Some(KEYCHAIN) {
            let _ = save(&dir, |saved| saved.waiting = Some(KEYCHAIN.into()));
            audit(
                &dir,
                "unattended_waiting",
                Some(offer),
                Some(KEYCHAIN.into()),
                None,
            );
        }
        return Pass::Interactive;
    }
    let _ = save(&dir, |saved| saved.waiting = None);
    let reply = apply::request(host, env, &offer.to_string(), true).await;
    if !reply.ok {
        audit(
            &dir,
            "unattended_apply",
            Some(offer),
            reply.message.clone(),
            None,
        );
        return Pass::Busy;
    }
    if reply.data["state"] == "deferred" {
        Pass::Deferred
    } else {
        Pass::Applying
    }
}

// ---------------------------------------------------------------------------------------------
// Updating to the newest release in one request.

/// Profiles whose requested update waits for its preparation to end before it is installed.
static FOLLOWING: StdMutex<std::collections::BTreeSet<PathBuf>> =
    StdMutex::new(std::collections::BTreeSet::new());

fn with_outcome(mut data: Value, outcome: &str, version: Option<Version>) -> Reply {
    data["outcome"] = json!(outcome);
    data["target"] = json!(version.map(|v| v.to_string()));
    Reply::success(data)
}

/// Updates this installation to the newest stable release in one request: it checks the registry,
/// prepares that release (or reuses the prepared copy) and then asks for the ordinary install,
/// which defers while work is in progress, restarts, checks health and rolls back. The version is
/// never the caller's: it is whatever the pinned registry lists as newest right now.
pub(super) async fn latest(host: &Arc<Host>, env: &Environment, by: By) -> Reply {
    if let Err(diagnostic) = core::inspect(&env.exe) {
        return Reply::error("not_ready", 3, &diagnostic);
    }
    if let Some(diagnostic) = apply::unsupported(env) {
        return Reply::error("not_ready", 3, diagnostic);
    }
    let dir = directory(host);
    let _ = journal::recover(&dir, now());
    if journal::read(&dir).is_some_and(|j| !j.phase.finished()) {
        return with_outcome(status(host, env), "applying", None);
    }
    if let Err(message) = refresh(host, env).await {
        return Reply::error("dependency_or_network", 5, &message);
    }
    let saved = load(&dir);
    let installed = core::inspect(&env.exe).ok().map(|i| i.version);
    let waiting = saved
        .pending
        .as_ref()
        .and_then(|p| Version::parse(&p.version));
    let Some(offer) = available(&saved, env.running, installed).filter(|_| waiting.is_none())
    else {
        // Published but its archive is not attached yet: the request waits for it instead of
        // failing. The outcome is `staging`, which requesters of every version read correctly.
        let Some(version) = waiting.or_else(|| late(&saved, env.running, installed)) else {
            return with_outcome(status(host, env), "current", None);
        };
        if by == By::Remote && keychain(env) {
            return Reply::error("not_ready", 3, KEYCHAIN_REMOTE);
        }
        if !saved.pending.as_ref().is_some_and(|p| p.install) {
            audit(
                &dir,
                "update_requested",
                Some(version),
                None,
                Some(by.label()),
            );
        }
        wait_for_archive(host, env, version, true, by.label());
        return with_outcome(status(host, env), "staging", Some(version));
    };
    if by == By::Remote && keychain(env) {
        return Reply::error("not_ready", 3, KEYCHAIN_REMOTE);
    }
    if saved.deferred.is_some() && staged(&dir, &saved, Some(offer)).is_some() {
        return with_outcome(status(host, env), "deferred", Some(offer));
    }
    audit(
        &dir,
        "update_requested",
        Some(offer),
        None,
        Some(by.label()),
    );
    let preparing = live()
        .get(dir.parent().unwrap())
        .map(|stage| stage.version.clone());
    match preparing {
        Some(version) if version == offer.to_string() => {}
        Some(_) => {
            return Reply::error(
                "state_conflict",
                6,
                "Another version is being prepared. Wait for it to finish, then update again.",
            );
        }
        None if staged(&dir, &saved, Some(offer)).is_some() => {
            let reply = apply::request(host, env, &offer.to_string(), false).await;
            if !reply.ok {
                return reply;
            }
            let outcome = if reply.data["state"] == "deferred" {
                "deferred"
            } else {
                "applying"
            };
            return with_outcome(reply.data, outcome, Some(offer));
        }
        None => {
            let reply = stage(host, env, &offer.to_string(), None).await;
            if !reply.ok {
                return reply;
            }
        }
    }
    follow(host.clone(), env.clone(), offer);
    with_outcome(status(host, env), "staging", Some(offer))
}

/// Installs the requested release once its preparation ends; a failed preparation is recorded and
/// left for the person, never retried.
fn follow(host: Arc<Host>, env: Environment, version: Version) {
    let dir = directory(&host);
    let profile = dir.parent().unwrap().to_path_buf();
    if !FOLLOWING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(profile.clone())
    {
        return;
    }
    tokio::spawn(async move {
        while live().contains_key(&profile) {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        FOLLOWING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&profile);
        let saved = load(&dir);
        if staged(&dir, &saved, Some(version)).is_none() {
            let error = saved
                .stage
                .filter(|s| s.version == version.to_string())
                .and_then(|s| s.error);
            audit(&dir, "update_stage", Some(version), error, None);
            return;
        }
        if saved.deferred.is_some() {
            return;
        }
        let reply = apply::request(&host, &env, &version.to_string(), false).await;
        if !reply.ok {
            audit(&dir, "update_apply", Some(version), reply.message, None);
        }
    });
}

/// A failed preparation of `version`, as the status shows it.
fn failed_stage(version: Version, started: i64, error: Option<String>) -> Stage {
    Stage {
        version: version.to_string(),
        state: "failed".into(),
        phase: "done".into(),
        started_at: started,
        finished_at: Some(now()),
        crates: 0,
        current: None,
        error,
        tail: Vec::new(),
        received: 0,
        total: None,
    }
}

// ---------------------------------------------------------------------------------------------
// Waiting for a release archive.

/// Profiles with a task that waits for a release archive.
static WATCHING: StdMutex<std::collections::BTreeSet<PathBuf>> =
    StdMutex::new(std::collections::BTreeSet::new());

/// Removes this profile's entry from `WATCHING` however the task ends.
struct Watching(PathBuf);
impl Drop for Watching {
    fn drop(&mut self) {
        WATCHING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Keeps an update asked for before its release archive was attached and makes sure one task
/// waits for it. Asking again joins the same wait (and its bound); it never starts a second one.
fn wait_for_archive(
    host: &Arc<Host>,
    env: &Environment,
    version: Version,
    install: bool,
    by: &str,
) {
    let dir = directory(host);
    let at = now();
    let mut fresh = false;
    let _ = save(&dir, |saved| match &mut saved.pending {
        Some(pending) if pending.version == version.to_string() => pending.install |= install,
        _ => {
            fresh = true;
            saved.pending = Some(Pending {
                version: version.to_string(),
                since: at,
                until: at + env.archive_wait,
                install,
                by: by.to_owned(),
            });
        }
    });
    if fresh {
        audit(&dir, "update_waiting", Some(version), None, Some(by));
    }
    watch(host.clone(), env.clone());
}

fn watch(host: Arc<Host>, env: Environment) {
    let profile = directory(&host).parent().unwrap().to_path_buf();
    if !WATCHING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(profile.clone())
    {
        return;
    }
    tokio::spawn(async move {
        let _watching = Watching(profile);
        loop {
            tokio::time::sleep(env.archive_poll).await;
            if !look_for_archive(&host, &env).await {
                break;
            }
        }
    });
}

/// One look at the release for the waiting update; false once the wait is over. When the archive
/// is there the release is prepared (and installed, when that was asked) exactly once; after the
/// bound the wait ends with a fixed text and nothing is tried again on its behalf.
async fn look_for_archive(host: &Arc<Host>, env: &Environment) -> bool {
    let dir = directory(host);
    if load(&dir).pending.is_none() {
        return false;
    }
    // A failed look (the registry or the release host unreachable) is only a look: the bound holds.
    let _ = refresh(host, env).await;
    let saved = load(&dir);
    let Some(pending) = saved.pending.clone() else {
        return false;
    };
    let installed = core::inspect(&env.exe).ok().map(|i| i.version);
    if let Some(offer) = available(&saved, env.running, installed) {
        let _ = save(&dir, |saved| saved.pending = None);
        let reply = stage(host, env, &offer.to_string(), None).await;
        if !reply.ok {
            let message = reply.message.unwrap_or_default();
            let _ = save(&dir, |saved| {
                saved.stage = Some(failed_stage(offer, pending.since, Some(message.clone())));
            });
            audit(&dir, "update_stage", Some(offer), Some(message), None);
        } else if pending.install {
            follow(host.clone(), env.clone(), offer);
        }
        return false;
    }
    let Some(newest) = late(&saved, env.running, installed) else {
        // Nothing newer to wait for any more (installed otherwise, or withdrawn).
        let _ = save(&dir, |saved| saved.pending = None);
        return false;
    };
    if now() >= pending.until {
        let target = core::inspect(&env.exe)
            .ok()
            .and_then(|i| i.target)
            .unwrap_or_default();
        let text = never_text(&pending.version, &target);
        let version = Version::parse(&pending.version).unwrap_or(newest);
        let _ = save(&dir, |saved| {
            saved.pending = None;
            saved.stage = Some(failed_stage(version, pending.since, Some(text.clone())));
        });
        audit(&dir, "update_stage", Some(version), Some(text), None);
        return false;
    }
    if newest.to_string() != pending.version {
        // A newer release came out meanwhile: the wait follows it, within the same bound.
        let _ = save(&dir, |saved| {
            if let Some(pending) = &mut saved.pending {
                pending.version = newest.to_string();
            }
        });
    }
    true
}

/// Whether another device of the account may ask this installation to update itself: this host is
/// the local owner of a supported installation it can restart by itself.
pub(super) fn remote_capable(host: &Host) -> bool {
    let env = environment();
    is_local_owner(&host.state.authority, Transport::Local)
        && core::inspect(&env.exe).is_ok()
        && apply::unsupported(&env).is_none()
}

/// A request from another device of the account, received through the coordinator: the same
/// update to the newest release, only on a host that owns its installation.
pub(super) async fn remote_latest(host: &Arc<Host>) -> Result<Value> {
    ensure!(
        is_local_owner(&host.state.authority, Transport::Local),
        "{MANAGED}"
    );
    let reply = latest(host, &environment(), By::Remote).await;
    ensure!(
        reply.ok,
        "{}",
        reply
            .message
            .as_deref()
            .unwrap_or("The update could not start.")
    );
    Ok(reply.data)
}

/// The update to the newest release asked from this installation's own row of the machine list:
/// only its local owner (the local page or pta on this computer) may, like every update here.
pub(super) async fn own_latest(host: &Arc<Host>) -> Result<Value> {
    let invocation = authority::invocation(&host.state)?;
    ensure!(
        is_local_owner(&invocation.scope, invocation.transport),
        "{}",
        if invocation.scope.shared || !invocation.scope.owner {
            MANAGED
        } else {
            LOCAL_ONLY
        }
    );
    let by = if invocation.transport == Transport::Window {
        By::LocalPage
    } else {
        By::Cli
    };
    let reply = latest(host, &environment(), by).await;
    ensure!(
        reply.ok,
        "{}",
        reply
            .message
            .as_deref()
            .unwrap_or("The update could not start.")
    );
    Ok(reply.data)
}

/// Removes what earlier updates left behind, unless one is running.
fn tidy(host: &Host) {
    if let Ok(installation) = core::inspect(&environment().exe) {
        journal::sweep(&directory(host), &installation.root, installation.version);
    }
}

mod apply;
pub(crate) use apply::{HANDOVER, run_helper};

#[cfg(test)]
mod tests;
