//! Applying a prepared update: the request, the helper process that replaces the executables
//! while no host runs, the restart and health check, and the rollback.
//!
//! The host that receives the request only checks, records a journal and starts the helper. The
//! helper is the same package's host binary (`--maintenance-apply`), run from the prepared copy so
//! that it never holds the installed files: it asks the running host to quit (the ordinary quit
//! that stops the assistant and the tunnel), replaces both executables through the journaled
//! sequence in [`crate::maintenance::apply`], starts the new host the way the previous one was
//! started, restores the assistant if it was running and only then calls the update done. Any
//! failure puts the previous pair back and starts it. Nothing here retries a Graph send or a work
//! item write: those operations are not touched, and the update waits while one is in flight.
use super::*;
use crate::maintenance::apply::{
    self as journal, Apply, Health, Intent, Journal, Lock, Phase, Profile,
};

pub(super) const FLAG: &str = "--maintenance-apply";
/// Set by the helper on the host it starts, so that host is not mistaken for one started in the
/// middle of an update.
pub(crate) const HANDOVER: &str = "PTA_UPDATE_HANDOVER";
const SERVICE: &str = "The application runs as a service under systemd, which would start a second copy when the update restarts it. Update it through the service: install the new version with `cargo install personal-teams-assistant --locked --force` and restart the service.";

/// Time limits, shortened by tests.
#[derive(Clone, Copy)]
pub(super) struct Tune {
    pub health: Duration,
    pub poll: Duration,
}

impl Default for Tune {
    fn default() -> Self {
        Self {
            health: Duration::from_secs(180),
            poll: Duration::from_secs(1),
        }
    }
}

/// How a restart looks to the update.
pub(super) enum Outcome {
    Deferred,
    Committed,
    RolledBack,
    Failed,
}

/// What the helper needs from the machine; tests replace it with a scripted one.
pub(super) trait Platform {
    /// Asks the running host to quit and waits until it released the profile.
    async fn quit(&self) -> bool;
    /// Starts the installed host the way the previous one was started.
    async fn start(&self, launch: &[String]) -> Result<()>;
    /// What the running host reports, or nothing while none answers.
    async fn health(&self) -> Option<Health>;
    async fn start_assistant(&self) -> Result<()>;
    /// Refreshes what the new version carries outside the executables; returns notes for the person.
    fn follow_up(&self) -> Vec<String>;
}

// ---------------------------------------------------------------------------------------------
// The request.

/// A headless host started by systemd (a unit): the unit would start a second copy when the update
/// restarts it. A window started from a desktop session is not a unit's service.
pub(super) fn unsupported(env: &Environment) -> Option<&'static str> {
    (env.launch.iter().any(|flag| flag == "--headless")
        && env
            .variables
            .iter()
            .any(|(name, _)| name == "INVOCATION_ID"))
    .then_some(SERVICE)
}

/// The profiles a restart affects: this one and the isolated profiles of a portal under it.
pub(super) fn profiles(host: &Host) -> Vec<Profile> {
    let own = host.state.config_path.parent().unwrap().to_path_buf();
    let data = |dir: &Path| {
        read_config(&dir.join("config.toml"))
            .map(|config| config.server.data_dir)
            .unwrap_or_else(|_| dir.join("data"))
    };
    let mut found = vec![Profile {
        data: data(&own),
        dir: own.clone(),
    }];
    if let Ok(entries) = fs::read_dir(own.join("web").join("profiles")) {
        let mut others: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        others.sort();
        found.extend(others.into_iter().map(|dir| Profile {
            data: data(&dir),
            dir,
        }));
    }
    found
}

/// Why the assistant should not be restarted now, beyond what the databases say.
async fn in_memory(host: &Host) -> Vec<String> {
    let mut reasons = Vec::new();
    let profile = host.state.config_path.parent().unwrap();
    if super::super::migration::active(profile) {
        reasons.push("A migration is running.".to_owned());
    }
    if host.state.variables.waiting() {
        reasons.push("A protected variable entry is waiting for its value.".to_owned());
    }
    if super::super::activity::active(&host.state).await {
        reasons.push("An activity review is running.".to_owned());
    }
    reasons
}

fn intent_of(env: &Environment, running: bool, subscriptions: u64, tunnel: bool) -> Intent {
    Intent {
        launch: env.launch.clone(),
        assistant: running,
        subscriptions,
        tunnel,
    }
}

pub(super) async fn request(host: &Arc<Host>, env: &Environment, requested: &str) -> Reply {
    let Some(version) = Version::parse(requested) else {
        return Reply::error("invalid_input", 2, "That is not a release version.");
    };
    let installation = match core::inspect(&env.exe) {
        Ok(installation) => installation,
        Err(diagnostic) => return Reply::error("not_ready", 3, &diagnostic),
    };
    if let Some(diagnostic) = unsupported(env) {
        return Reply::error("not_ready", 3, diagnostic);
    }
    let dir = directory(host);
    let saved = load(&dir);
    let offer = available(
        saved.latest.as_deref(),
        env.running,
        Some(installation.version),
    );
    if offer != Some(version) {
        return Reply::error(
            "state_conflict",
            6,
            &format!("Version {version} is not the available update. Check for updates again."),
        );
    }
    let Some(manifest) = staged(&dir, &saved, offer) else {
        return Reply::error(
            "state_conflict",
            6,
            &format!("Prepare version {version} first: no verified copy of it is ready."),
        );
    };
    let base = staged_dir(&dir, &version.to_string()).join("root");
    if !core::unchanged(&base, &manifest) {
        return Reply::error(
            "state_conflict",
            6,
            "The prepared files changed after they were verified. Prepare the update again.",
        );
    }
    if live().contains_key(dir.parent().unwrap()) {
        return Reply::error(
            "state_conflict",
            6,
            "An update is being prepared. Wait for it to finish.",
        );
    }
    // Block new profile operations while deciding, so work cannot start between the check and the
    // helper taking over.
    let operations = tokio::time::timeout(Duration::from_secs(2), host.state.operations.lock())
        .await
        .ok();
    let Ok(Some(lock)) = Lock::try_acquire(&dir) else {
        return Reply::error("state_conflict", 6, "An update is already being applied.");
    };
    match journal::recover(&dir, journal::now()) {
        Ok(journal::Recovery::Active) => {
            return Reply::error("state_conflict", 6, "An update is already being applied.");
        }
        Ok(journal::Recovery::Failed(message)) => {
            return Reply::error("operation_failed", 1, &message);
        }
        _ => {}
    }
    let profiles = profiles(host);
    let mut reasons = journal::busy(&profiles);
    for reason in in_memory(host).await {
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    if operations.is_none() {
        reasons.push("A change or request is being processed.".into());
    }
    if !reasons.is_empty() {
        let _ = save(&dir, |saved| {
            // The wait began with the first deferral, not with each look.
            let since = saved
                .deferred
                .as_ref()
                .filter(|d| d.version == version.to_string())
                .map_or_else(now, |d| d.at);
            saved.deferred = Some(Deferred {
                version: version.to_string(),
                at: since,
                reasons,
            });
        });
        return Reply::success(status(host, env));
    }
    if let Err(error) = journal::preflight(&dir, &installation.root, &manifest, &profiles) {
        return Reply::error("not_ready", 3, &error.to_string());
    }
    let (running, tunnel, subscriptions) = {
        let running = host.state.running.lock().await;
        let active = running.as_ref().is_some_and(|r| !r.task.is_finished());
        let data = profiles[0].data.join("assistant.db");
        (
            active,
            running.as_ref().is_some_and(|r| r.tunnel.is_some()),
            crate::state::Store::subscription_health(&data).map_or(0, |(count, _)| count as u64),
        )
    };
    let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
    // The pair being replaced is the installed one; a rollback restores and waits for exactly it,
    // even when the host still runs an older version (restart_required).
    let entry = Journal::new(
        &id,
        &installation.version.to_string(),
        manifest,
        &installation.root,
        &base,
        intent_of(env, running, subscriptions, tunnel),
        profiles,
    );
    if journal::write(&dir, &entry).is_err() {
        return Reply::error("operation_failed", 1, "The update could not be recorded.");
    }
    let _ = save(&dir, |saved| saved.deferred = None);
    if !env.manual
        && let Err(error) = spawn(&base)
    {
        let _ = fs::remove_file(dir.join(journal::JOURNAL));
        return Reply::error("operation_failed", 1, &error.to_string());
    }
    // The helper takes the lock as soon as it starts; the journal bridges the moment between.
    drop(lock);
    Reply::success(status(host, env))
}

/// Cancels a deferred request. An update that is already applying cannot be cancelled.
pub(super) fn cancel(host: &Host, env: &Environment) -> Reply {
    let dir = directory(host);
    if journal::read(&dir).is_some_and(|j| !j.phase.finished()) {
        return Reply::error(
            "state_conflict",
            6,
            "The update is already being applied and cannot be cancelled.",
        );
    }
    match save(&dir, |saved| saved.deferred = None) {
        Ok(()) => Reply::success(status(host, env)),
        Err(_) => Reply::error("operation_failed", 1, "The choice could not be saved."),
    }
}

fn spawn(staged: &Path) -> Result<()> {
    let exe = staged.join("bin").join(core::exe_name(core::CRATE));
    ensure!(exe.is_file(), "The prepared application is missing.");
    let mut command = std::process::Command::new(exe);
    command
        .arg(FLAG)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);
    command
        .spawn()
        .context("The update could not be started.")?;
    Ok(())
}

fn detach(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The helper outlives this host and its terminal process group.
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        command.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
    }
}

// ---------------------------------------------------------------------------------------------
// The helper.

/// Entry of `personal-teams-assistant --maintenance-apply`. Returns the process exit status:
/// 0 when the update was applied, rolled back or deferred, 1 when a person has to look.
pub(crate) fn run_helper() -> i32 {
    let Ok(profile) = control::profile_dir() else {
        return 2;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        return 2;
    };
    let dir = profile.join(DIRECTORY);
    let Some(entry) = journal::read(&dir).filter(|j| j.phase == Phase::Requested) else {
        return 2;
    };
    let platform = Local {
        profile,
        root: entry.root.clone(),
        id: entry.id.clone(),
    };
    match runtime.block_on(drive(&dir, &platform, Tune::default())) {
        Outcome::Failed => 1,
        _ => 0,
    }
}

/// The whole update after the request, as one journaled sequence.
pub(super) async fn drive(dir: &Path, platform: &impl Platform, tune: Tune) -> Outcome {
    let dir_owned = dir.to_owned();
    let Ok(Ok(_lock)) =
        tokio::task::spawn_blocking(move || Lock::wait(&dir_owned, Duration::from_secs(30))).await
    else {
        return Outcome::Failed;
    };
    let Some(entry) = journal::read(dir).filter(|j| j.phase == Phase::Requested) else {
        return Outcome::Failed;
    };
    let mut apply = Apply::new(dir, entry);
    drive_with(&mut apply, platform, tune).await
}

pub(super) async fn drive_with(apply: &mut Apply, platform: &impl Platform, tune: Tune) -> Outcome {
    let dir = apply.dir.clone();
    // Work that started after the request: look again right before anything is stopped.
    let reasons = journal::busy(&apply.journal.profiles);
    if !reasons.is_empty() {
        let (version, at) = (apply.journal.to.clone(), now());
        let _ = fs::remove_file(dir.join(journal::JOURNAL));
        let _ = save(&dir, |saved| {
            saved.deferred = Some(Deferred {
                version,
                at,
                reasons,
            })
        });
        return Outcome::Deferred;
    }
    if apply.phase(Phase::Stopping).is_err() {
        return Outcome::Failed;
    }
    if !platform.quit().await {
        // Nothing was touched and the host still owns the profile.
        let _ = apply.finish(
            Phase::RolledBack,
            Some("The running application did not stop, so nothing was changed.".into()),
        );
        return Outcome::RolledBack;
    }
    let launch = apply.journal.intent.launch.clone();
    let replaced = apply.back_up().and_then(|()| apply.replace());
    if let Err(error) = replaced {
        return roll_back(apply, platform, tune, &error.to_string()).await;
    }
    if apply.phase(Phase::Restarting).is_err() {
        return roll_back(apply, platform, tune, "The update could not be recorded.").await;
    }
    if let Err(error) = platform.start(&launch).await {
        return roll_back(apply, platform, tune, &error.to_string()).await;
    }
    let _ = apply.phase(Phase::Verifying);
    let (to, intent) = (apply.journal.to.clone(), apply.journal.intent.clone());
    if let Err(reason) = wait_healthy(platform, &intent, &to, tune, true).await {
        return roll_back(apply, platform, tune, &reason).await;
    }
    apply.journal.notes = platform.follow_up();
    if apply.commit().is_err() {
        return roll_back(apply, platform, tune, "The update could not be recorded.").await;
    }
    Outcome::Committed
}

fn healthy(now: &Health, intent: &Intent, version: &str) -> bool {
    now.version == version
        && (!intent.assistant || now.assistant)
        && (intent.subscriptions == 0 || now.subscriptions > 0)
        && (now.tunnel || !intent.tunnel)
}

/// Waits until the host runs `version` and is as healthy as before. With `restore`, asks it once
/// to start the assistant again when the previous host's assistant was running.
async fn wait_healthy(
    platform: &impl Platform,
    intent: &Intent,
    version: &str,
    tune: Tune,
    restore: bool,
) -> std::result::Result<(), String> {
    let deadline = std::time::Instant::now() + tune.health;
    let mut asked = !restore;
    loop {
        if let Some(health) = platform.health().await
            && health.version == version
        {
            if !asked && intent.assistant && !health.assistant {
                asked = true;
                if platform.start_assistant().await.is_err() {
                    return Err(format!("The assistant did not start on version {version}."));
                }
            }
            if healthy(&health, intent, version) {
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("Version {version} did not become healthy in time."));
        }
        tokio::time::sleep(tune.poll).await;
    }
}

/// Puts the previous pair back and starts it again, escalating to the data copy only if the
/// previous version cannot start on what the new one left.
async fn roll_back(
    apply: &mut Apply,
    platform: &impl Platform,
    tune: Tune,
    reason: &str,
) -> Outcome {
    apply.journal.error = Some(reason.to_owned());
    let _ = apply.phase(Phase::RollingBack);
    let fail = |apply: &mut Apply, why: String| {
        let _ = apply.finish(Phase::Failed, Some(format!("{reason} {why}")));
        Outcome::Failed
    };
    if !platform.quit().await {
        return fail(
            apply,
            "The new version could not be stopped, so the previous one was not restored. Quit the application and update again.".into(),
        );
    }
    if let Err(error) = apply.roll_back() {
        return fail(
            apply,
            format!("The previous version could not be restored: {error}"),
        );
    }
    let (from, intent, launch) = (
        apply.journal.from.clone(),
        apply.journal.intent.clone(),
        apply.journal.intent.launch.clone(),
    );
    let quick = Tune {
        health: tune.health.min(Duration::from_secs(60)),
        ..tune
    };
    let mut started = platform.start(&launch).await.is_ok()
        && wait_healthy(
            platform,
            &Intent {
                assistant: false,
                subscriptions: 0,
                tunnel: false,
                ..intent.clone()
            },
            &from,
            quick,
            false,
        )
        .await
        .is_ok();
    if !started {
        // The previous version cannot start on the data the new one touched: put the copy back.
        let _ = platform.quit().await;
        if let Err(error) = apply.restore_data() {
            return fail(
                apply,
                format!(
                    "The previous version does not start and your data could not be restored: {error}"
                ),
            );
        }
        started = platform.start(&launch).await.is_ok()
            && wait_healthy(
                platform,
                &Intent {
                    assistant: false,
                    subscriptions: 0,
                    tunnel: false,
                    ..intent.clone()
                },
                &from,
                quick,
                false,
            )
            .await
            .is_ok();
    }
    if !started {
        return fail(
            apply,
            format!(
                "The previous version {from} does not start. Your data and credentials were not removed; the safety copy is in the profile's maintenance folder."
            ),
        );
    }
    if intent.assistant && platform.start_assistant().await.is_err() {
        apply
            .journal
            .notes
            .push("The assistant was running before the update and could not be started again: start it from the application.".into());
    }
    let _ = apply.finish(Phase::RolledBack, Some(reason.to_owned()));
    // The previous version runs on the live data again: the copy is not needed any more.
    apply.discard_backup();
    Outcome::RolledBack
}

// ---------------------------------------------------------------------------------------------
// The real machine.

struct Local {
    profile: PathBuf,
    root: PathBuf,
    id: String,
}

impl Local {
    async fn stopped(&self) -> bool {
        control::existing_host_at(&self.profile).is_err()
    }
}

impl Platform for Local {
    async fn quit(&self) -> bool {
        if self.stopped().await {
            return true;
        }
        // The host exits right after replying; a lost reply is fine, the lock tells the outcome.
        let _ = control::retirement_request(control::Request::new("app_quit"), &self.profile).await;
        for _ in 0..600 {
            if self.stopped().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    async fn start(&self, launch: &[String]) -> Result<()> {
        let exe = self.root.join("bin").join(core::exe_name(core::CRATE));
        let mut command = std::process::Command::new(exe);
        for flag in launch
            .iter()
            .filter(|f| matches!(f.as_str(), "--headless" | "--host"))
        {
            command.arg(flag);
        }
        command
            .env(HANDOVER, &self.id)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        detach(&mut command);
        command
            .spawn()
            .context("The application could not be started.")?;
        Ok(())
    }

    async fn health(&self) -> Option<Health> {
        if self.stopped().await {
            return None;
        }
        let reply = control::retirement_request(control::Request::new("snapshot"), &self.profile)
            .await
            .ok()
            .filter(|reply| reply.ok)?;
        Some(Health {
            version: reply.data["host_version"].as_str()?.to_owned(),
            assistant: reply.data["running"].as_bool() == Some(true),
            subscriptions: reply.data["active_subscriptions"].as_u64().unwrap_or(0),
            tunnel: reply.data["tunnel_running"].as_bool() == Some(true),
        })
    }

    async fn start_assistant(&self) -> Result<()> {
        let reply =
            control::client_at(control::Request::new("start_assistant"), &self.profile).await?;
        ensure!(reply.ok, "The assistant did not start.");
        Ok(())
    }

    fn follow_up(&self) -> Vec<String> {
        super::super::skill::refresh(&self.profile)
    }
}

// ---------------------------------------------------------------------------------------------
// Waiting requests.

/// Retries a request that was deferred because work was in progress, once the work is gone. The
/// person asked for the update; nothing here starts one nobody asked for.
pub(super) async fn retry_deferred(host: Arc<Host>) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let env = environment();
        let dir = directory(&host);
        let Some(deferred) = load(&dir).deferred else {
            continue;
        };
        let reply = request(&host, &env, &deferred.version).await;
        // A request that can no longer be honored (a newer release, files gone) is dropped.
        if !reply.ok && matches!(reply.code.as_str(), "state_conflict" | "not_ready") {
            let _ = save(&dir, |saved| saved.deferred = None);
        }
    }
}
