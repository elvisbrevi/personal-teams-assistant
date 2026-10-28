//! Local installation maintenance: update discovery and staging.
//!
//! A check is a bounded, read-only look at the pinned public registry. Staging builds the newest
//! release with Cargo into a directory of this profile while the installed executables keep
//! running, then verifies the matched host/CLI pair; it replaces nothing and installs nothing.
//! Only the genuine local owner of the installation (the control endpoint's own token or the
//! application window, on a profile that is not a shared portal account) can check or stage; a
//! portal session, a machine credential or an ordinary account only sees that the server's
//! updates are managed by its operator. The shared server's own timer (`scripts/vps-update.py`)
//! is independent of everything here.
use super::{
    authority::{self, Transport},
    control::{self, Reply},
    *,
};
use crate::maintenance::{self as core, Installation, Manifest, Version};
use serde_json::{Value, json};
use std::{collections::BTreeMap, ffi::OsString, sync::Mutex as StdMutex, time::Duration};

const SCHEMA: u32 = 1;
const DIRECTORY: &str = "maintenance";
const AUTO_AFTER: u64 = 90;
const AUTO_EVERY: u64 = 6 * 60 * 60;
const AUTO_DUE: i64 = 12 * 60 * 60;
const MANAGED: &str =
    "Updates for this server are applied by its operator; there is nothing to install from here.";
const LOCAL_ONLY: &str = "Updating this installation needs the local owner of the computer: use the application window or pta on that computer.";

pub(super) fn handles(method: &str) -> bool {
    method.starts_with("crate_update_")
}

/// What the host needs from its surroundings; tests replace it with synthetic ones.
#[derive(Clone)]
pub(super) struct Environment {
    pub registry: String,
    pub exe: PathBuf,
    pub variables: Vec<(OsString, OsString)>,
    pub running: Version,
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
}

/// Progress of a build in this process, by profile.
static LIVE: StdMutex<BTreeMap<PathBuf, Stage>> = StdMutex::new(BTreeMap::new());
/// Serializes read-modify-write of a profile's saved state.
static SAVING: StdMutex<()> = StdMutex::new(());

fn live() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Stage>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

fn directory(host: &Host) -> PathBuf {
    host.state.config_path.parent().unwrap().join(DIRECTORY)
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

/// The newest release that is worth offering: newer than both the installed and the running one.
fn available(
    latest: Option<&str>,
    running: Version,
    installed: Option<Version>,
) -> Option<Version> {
    let baseline = installed.map_or(running, |i| i.max(running));
    latest.and_then(Version::parse).filter(|l| *l > baseline)
}

fn stage_json(stage: &Stage) -> Value {
    json!({
        "version": stage.version, "state": stage.state, "phase": stage.phase,
        "started_at": stage.started_at, "finished_at": stage.finished_at,
        "progress": {"crates": stage.crates, "current": stage.current},
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
    let saved = load(&dir);
    let installation = core::inspect(&env.exe);
    let installed = installation.as_ref().ok().map(|i| i.version);
    let offer = available(saved.latest.as_deref(), env.running, installed);
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
        (Some(_), ..) => "staging",
        (None, Some(_), _) => "staged",
        (None, None, Some("failed")) => "failed",
        _ => "idle",
    };
    let install = match &installation {
        Ok(i) => json!({"supported":true,"features":i.features,"all_features":i.all_features,
            "no_default_features":i.no_default_features,"target":i.target}),
        Err(diagnostic) => json!({"supported":false,"diagnostic":diagnostic}),
    };
    // A failure about a release that is no longer on offer is stale.
    let shown = recorded
        .filter(|s| s.state == "staging" || offer.is_some_and(|o| o.to_string() == s.version));
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
        "can_stage": installation.is_ok() && offer.is_some() && state != "staging",
        "stage": shown.as_ref().map(stage_json),
        "staged": manifest.map(|m| json!({
            "version": m.version, "staged_at": m.staged_at, "features": m.features,
            "no_default_features": m.no_default_features, "target": m.target,
            "binaries": m.binaries.keys().collect::<Vec<_>>(),
        })),
    })
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
            Ok(args) => stage(host, &env, &args.version).await,
            Err(_) => Reply::error(
                "invalid_input",
                2,
                "Give the version to prepare, as returned by the update check.",
            ),
        },
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

/// Reads the registry and remembers what it listed. Never installs or stages anything.
async fn refresh(
    host: &Host,
    env: &Environment,
) -> std::result::Result<Option<core::Release>, String> {
    let result = async {
        let body = core::fetch_index(&env.registry).await?;
        core::newest_stable(&body)
    }
    .await;
    let dir = directory(host);
    let at = now();
    let outcome = match &result {
        Ok(release) => Ok(release.as_ref().map(|r| r.version.to_string())),
        Err(error) => Err(error.to_string()),
    };
    let _ = save(&dir, |saved| {
        saved.checked_at = Some(at);
        match &outcome {
            Ok(latest) => {
                saved.latest = latest.clone();
                saved.check_error = None;
            }
            Err(message) => saved.check_error = Some(message.clone()),
        }
    });
    result.map_err(|e| e.to_string())
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

/// Where Cargo is: the build's own `PATH` first, then the places `rustup` installs it.
fn find_cargo(variables: &[(OsString, OsString)]) -> Option<PathBuf> {
    let get = |name: &str| {
        variables
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    core::find_program("cargo", get("PATH").as_deref()).or_else(|| {
        let home = get("CARGO_HOME").map(PathBuf::from).or_else(|| {
            get("HOME")
                .or_else(|| get("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".cargo"))
        })?;
        core::find_program("cargo", Some(home.join("bin").as_os_str()))
    })
}

/// Removes this profile's entry from `LIVE` however the build ends.
struct Building(PathBuf);
impl Drop for Building {
    fn drop(&mut self) {
        live().remove(&self.0);
    }
}

async fn stage(host: &Host, env: &Environment, requested: &str) -> Reply {
    let Some(version) = Version::parse(requested) else {
        return Reply::error("invalid_input", 2, "That is not a release version.");
    };
    let installation = match core::inspect(&env.exe) {
        Ok(installation) => installation,
        Err(diagnostic) => return Reply::error("not_ready", 3, &diagnostic),
    };
    let dir = directory(host);
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
    let Some(cargo) = find_cargo(&env.variables) else {
        return Reply::error(
            "not_ready",
            3,
            "Cargo was not found. Install the Rust toolchain from https://rustup.rs and try again.",
        );
    };
    let build_env = core::build_environment(env.variables.clone());
    match core::cargo_version(&cargo, &build_env).await {
        Err(message) => return Reply::error("not_ready", 3, &message),
        Ok(have) => {
            if let Some(need) = release.rust_version.filter(|need| have < *need) {
                return Reply::error(
                    "not_ready",
                    3,
                    &format!(
                        "Version {version} needs Rust {need} or newer; this computer has {have}. Run `rustup update` and try again."
                    ),
                );
            }
        }
    }
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
        stage.phase = "building".into();
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
        build_env,
        partial,
        started,
        _building: building,
    };
    tokio::spawn(work.run());
    Reply::success(status(host, env))
}

/// One staging build, run in the background.
struct Work {
    dir: PathBuf,
    profile: PathBuf,
    version: Version,
    installation: Installation,
    cargo: PathBuf,
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
        let root = self.partial.join("root");
        let args = core::install_args(&self.installation, &self.version, &root);
        let built = core::build(
            &self.cargo,
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
        let outcome = match built {
            Err(failure) => Err((failure.reason, failure.tail)),
            Ok(()) => {
                self.update(|stage| stage.phase = "verifying".into());
                self.seal(&root)
                    .await
                    .map_err(|e| (e.to_string(), Vec::new()))
            }
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
                current: progress.and_then(|p| p.current),
                error,
                tail,
            });
        });
    }

    /// Verifies the pair Cargo built and moves it to its final place.
    async fn seal(&self, root: &Path) -> Result<()> {
        let manifest = core::verify(
            root,
            &self.version,
            &self.installation,
            &self.build_env,
            now(),
        )
        .await?;
        write_private(
            &self.partial.join("manifest.json"),
            &serde_json::to_string(&manifest)?,
        )?;
        let finished = staged_dir(&self.dir, &self.version.to_string());
        fs::rename(&self.partial, &finished)
            .context("The staged pair could not be put in place.")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The daily check.

/// Looks for a newer release now and then on the owner's own installation. Read-only: nothing is
/// ever downloaded, built or installed without an explicit request.
pub(super) async fn schedule(host: Arc<Host>) {
    if !is_local_owner(&host.state.authority, Transport::Local) {
        return;
    }
    tokio::time::sleep(Duration::from_secs(AUTO_AFTER)).await;
    loop {
        let env = environment();
        let due = load(&directory(&host))
            .checked_at
            .is_none_or(|at| now() - at > AUTO_DUE);
        if due && core::inspect(&env.exe).is_ok() {
            let _ = refresh(&host, &env).await;
        }
        tokio::time::sleep(Duration::from_secs(AUTO_EVERY)).await;
    }
}

#[cfg(test)]
mod tests;
