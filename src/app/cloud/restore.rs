//! Restoring one of the backups the account store operations take before they change anything
//! (`backups/cloud-<time>/`, phase 5 of the cloud storage design). Nothing local is ever deleted:
//! the restore first keeps a copy of the current data in `backups/before-restore-<time>/`.
//!
//! A running host keeps its databases open, so the files are never replaced under it. The
//! operation (`cloud_restore`) checks the backup, refuses while work a restart would interrupt is
//! running, records the request in `backups/restore.json` and restarts the host; the next start
//! applies it before any store opens (`apply_pending`): the copy of the current data, then every
//! file of the backup back at the path its manifest names. This installation's own link to the
//! account store (`[cloud]`) stays as it is now, because its credentials are not in the backup.
use super::*;
use crate::maintenance::apply::{self as safety, Profile, Saved};

const PENDING: &str = "restore.json";
const RESULT: &str = "restore-result.json";
const BEFORE: &str = "before-restore-";
/// Set on the host a restore starts, so it waits for the previous one to release the profile.
pub(crate) const HANDOVER: &str = "PTA_RESTORE_HANDOVER";
/// The exit status that asks systemd to start the host again (`Restart=on-failure`).
const RESTART_STATUS: i32 = 75;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    id: String,
    backup: String,
    requested_at: i64,
    /// Whether the Teams assistant was running, so the restored host starts it again.
    assistant: bool,
}

/// The last restore's outcome, in `backups/restore-result.json`.
#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Outcome {
    id: String,
    backup: String,
    requested_at: i64,
    finished_at: i64,
    ok: bool,
    files: usize,
    /// The copy of the data as it was just before, under `backups/`.
    before: String,
    error: String,
}

/// A backup's `manifest.json`, as `migration::back_up_data` writes it.
#[derive(Debug, Deserialize)]
struct Manifest {
    created_at: i64,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    host_version: String,
    #[serde(default)]
    bytes: u64,
    files: Vec<Saved>,
}

fn root(profile: &Path) -> PathBuf {
    profile.join("backups")
}

/// `cloud-<unix seconds>[-n]`, nothing else.
fn valid_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("cloud-") else {
        return false;
    };
    let mut parts = rest.split('-');
    let seconds = parts.next().unwrap_or_default();
    let suffix = parts.next();
    parts.next().is_none()
        && (1..=12).contains(&seconds.len())
        && seconds.bytes().all(|b| b.is_ascii_digit())
        && suffix
            .is_none_or(|s| (1..=4).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()))
}

fn version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// The profile and data directory a restore may write to.
fn places(profile: &Path) -> Result<(PathBuf, PathBuf)> {
    let config = crate::store::settings::LocalSettings::at(profile).config()?;
    Ok((profile.to_path_buf(), config.server.data_dir))
}

/// A path a backup may put back: inside the profile or the data directory, without `..`, and of
/// the kinds a backup holds (databases, settings files, the machine registry).
fn allowed(from: &Path, sqlite: bool, places: &(PathBuf, PathBuf)) -> bool {
    use std::path::Component;
    let plain = from.is_absolute()
        && from
            .components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir));
    let inside = from.starts_with(&places.0) || from.starts_with(&places.1);
    let name = from
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let kind = if sqlite {
        name.ends_with(".db")
    } else {
        name.ends_with(".toml") || name == "registry.json"
    };
    plain && inside && kind
}

/// The backup named `name` in this profile, checked: its manifest, every file it lists (where it
/// goes back, that the copy exists and that each database copy is intact) and that this version
/// can read it. The reason in the user's words otherwise.
fn check(profile: &Path, name: &str) -> std::result::Result<(PathBuf, Manifest), String> {
    if !valid_name(name) {
        return Err("Give a backup's name as cloud status or cloud backups lists it.".into());
    }
    let base = root(profile).join(name);
    if !std::fs::symlink_metadata(&base).is_ok_and(|m| m.is_dir()) {
        return Err("There is no backup with that name on this computer.".into());
    }
    let manifest: Manifest = std::fs::read_to_string(base.join("manifest.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .ok_or("The backup's manifest is missing or damaged; nothing was restored.")?;
    let ours = version(env!("CARGO_PKG_VERSION"));
    if version(&manifest.host_version).is_none_or(|made| Some(made) > ours) {
        return Err(
            "The backup was made by a newer version of the application; update this computer first."
                .into(),
        );
    }
    let places = places(profile).map_err(|_| "The configuration could not be read.".to_owned())?;
    if manifest.files.is_empty() {
        return Err("The backup lists no files; nothing was restored.".into());
    }
    for saved in &manifest.files {
        let source = base.join(&saved.file);
        let named = saved.file.len() == 4 && saved.file.bytes().all(|b| b.is_ascii_digit());
        if !named
            || !std::fs::symlink_metadata(&source).is_ok_and(|m| m.is_file())
            || !allowed(&saved.from, saved.sqlite, &places)
        {
            return Err(
                "The backup's manifest does not match its files; nothing was restored.".into(),
            );
        }
        if saved.sqlite && !intact(&source) {
            return Err("A database in the backup is damaged; nothing was restored.".into());
        }
    }
    Ok((base, manifest))
}

fn intact(path: &Path) -> bool {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|db| db.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)))
        .is_ok_and(|result| result == "ok")
}

fn last(profile: &Path) -> Option<Outcome> {
    std::fs::read_to_string(root(profile).join(RESULT))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

/// `cloud_backups`: this computer's backups, newest first, whether each can be restored, the
/// copies kept before earlier restores and the last restore's outcome. Read-only.
pub(super) fn operate_backups(host: &Host) -> Reply {
    let profile = host.state.settings.profile();
    let held = migration::held(&root(profile));
    let backups: Vec<Value> = held
        .iter()
        .rev()
        .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
        .map(|name| match check(profile, &name) {
            Ok((_, manifest)) => json!({
                "name": name, "created_at": manifest.created_at, "reason": manifest.reason,
                "host_version": manifest.host_version, "bytes": manifest.bytes,
                "files": manifest.files.len(), "restorable": true,
            }),
            Err(problem) => json!({"name": name, "restorable": false, "problem": problem}),
        })
        .collect();
    let mut before: Vec<String> = std::fs::read_dir(root(profile))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                .filter(|n| n.starts_with(BEFORE))
                .collect()
        })
        .unwrap_or_default();
    before.sort();
    before.reverse();
    Reply::success(json!({
        "backups": backups,
        "before_restore": before,
        "pending": root(profile).join(PENDING).exists(),
        "last_restore": last(profile),
        "restores_here": restores_here(host),
    }))
}

/// Restoring restarts the host: only its owner, at this machine's window or local CLI.
fn restores_here(host: &Host) -> bool {
    authority::invocation(&host.state).is_ok_and(|invocation| {
        super::super::maintenance::is_local_owner(&invocation.scope, invocation.transport)
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreArgs {
    backup: String,
}

/// `cloud_restore`: check the backup, make sure nothing a restart would interrupt is running,
/// record the request and restart the host, which applies it before anything opens.
pub(super) async fn operate_restore(host: &Arc<Host>, args: RestoreArgs) -> Reply {
    if !restores_here(host) {
        return Reply::error(
            "authority_denied",
            6,
            "Restoring a backup restarts the application: do it from this computer's local page or its local CLI, as its owner.",
        );
    }
    let profile = host.state.settings.profile().to_path_buf();
    if let Err(problem) = check(&profile, &args.backup) {
        return Reply::error("invalid_input", 2, &problem);
    }
    let _operations = host.state.operations.lock().await;
    let _gate = mirror::hold(host).await;
    let mut reasons = Vec::new();
    if let Ok(config) = host.state.settings.config() {
        reasons = safety::busy(&[Profile {
            dir: profile.clone(),
            data: config.server.data_dir,
        }]);
    }
    if super::super::migration::active(&profile) {
        reasons.push("A migration is running.".into());
    }
    if host.state.variables.waiting() {
        reasons.push("A protected variable entry is waiting for its value.".into());
    }
    if super::super::activity::active(&host.state).await {
        reasons.push("An activity review is running.".into());
    }
    if !reasons.is_empty() {
        return Reply {
            data: json!({"reasons": reasons}),
            ..Reply::error(
                "state_conflict",
                6,
                "Work a restart would interrupt is running; restore when it has finished.",
            )
        };
    }
    let pending = Pending {
        id: uuid::Uuid::new_v4().to_string(),
        backup: args.backup.clone(),
        requested_at: now(),
        assistant: host.state.running.lock().await.is_some(),
    };
    if security::private_dir(&root(&profile))
        .and_then(|()| {
            write_private(
                &root(&profile).join(PENDING),
                &serde_json::to_string(&pending)?,
            )
        })
        .is_err()
    {
        return Reply::error(
            "operation_failed",
            1,
            "The restore could not be recorded; nothing was changed.",
        );
    }
    restart(host.clone());
    Reply::success(json!({
        "restarting": true,
        "backup": args.backup,
        "assistant": pending.assistant,
        "next": "The application restarts now: it keeps a copy of the current data in backups/before-restore-<time>/, puts the backup back and starts again. Check the outcome with pta cloud backups.",
    }))
}

/// Whether this process is the main process of a systemd unit, which starts it again only when
/// it exits with a failure status.
fn systemd_main() -> bool {
    std::env::var("SYSTEMD_EXEC_PID")
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
        == Some(std::process::id())
}

/// Stop as `app_quit` does and start again: systemd's main process exits with a status that
/// restarts it; any other host first starts its successor, which waits for the profile.
fn restart(host: Arc<Host>) {
    tokio::spawn(async move {
        // Let the reply reach the client first.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let systemd = systemd_main();
        if !systemd && let Ok(exe) = std::env::current_exe() {
            let mut command = std::process::Command::new(exe);
            command
                .args(std::env::args_os().skip(1))
                .env(HANDOVER, "1")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            detach(&mut command);
            let _ = command.spawn();
        }
        super::super::activity::shutdown(&host.state).await;
        let _ = super::super::stop(&host.state).await;
        if systemd {
            std::process::exit(RESTART_STATUS);
        }
        host.shell.exit();
    });
}

fn detach(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        command.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
    }
}

/// On a host's start, before any store opens: a successor started by a restore waits (up to a
/// minute) for the previous host to release the profile.
pub(crate) fn wait_for_previous() {
    if std::env::var_os(HANDOVER).is_none() {
        return;
    }
    for _ in 0..120 {
        if control::existing_host().is_err() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// On a host's start, holding the profile and before any store opens: apply a recorded restore.
/// Returns whether the Teams assistant was running when it was requested; `None` without one.
/// The request is consumed either way and its outcome kept in `backups/restore-result.json`.
pub(crate) fn apply_pending(profile: &Path) -> Option<bool> {
    let path = root(profile).join(PENDING);
    let text = std::fs::read_to_string(&path).ok()?;
    let Ok(pending) = serde_json::from_str::<Pending>(&text) else {
        let _ = std::fs::remove_file(&path);
        return None;
    };
    let mut outcome = Outcome {
        id: pending.id.clone(),
        backup: pending.backup.clone(),
        requested_at: pending.requested_at,
        ..Outcome::default()
    };
    match apply(profile, &pending, &mut outcome) {
        Ok(files) => {
            outcome.ok = true;
            outcome.files = files;
        }
        Err(error) => outcome.error = error,
    }
    outcome.finished_at = now();
    let _ = write_private(
        &root(profile).join(RESULT),
        &serde_json::to_string(&outcome).unwrap_or_default(),
    );
    let _ = std::fs::remove_file(&path);
    Some(pending.assistant)
}

fn apply(
    profile: &Path,
    pending: &Pending,
    outcome: &mut Outcome,
) -> std::result::Result<usize, String> {
    let (base, manifest) = check(profile, &pending.backup)?;
    let (dir, data) =
        places(profile).map_err(|_| "The configuration could not be read.".to_owned())?;
    let link = crate::store::settings::LocalSettings::at(profile)
        .config()
        .map(|config| config.cloud)
        .map_err(|_| "The configuration could not be read.".to_owned())?;
    // The data as it is now, first; nothing is put back without it.
    let at = now();
    let mut name = format!("{BEFORE}{at}");
    let mut n = 1;
    while root(profile).join(&name).exists() {
        name = format!("{BEFORE}{at}-{n}");
        n += 1;
    }
    let before = root(profile).join(&name);
    let kept = security::private_dir(&before)
        .and_then(|()| {
            safety::copy_data(
                safety::protected_files(&[Profile { dir, data }]),
                &before,
                || Ok(()),
            )
        })
        .and_then(|saved| {
            write_private(
                &before.join("manifest.json"),
                &serde_json::to_string_pretty(&json!({
                    "created_at": at, "reason": "before_restore", "restoring": pending.backup,
                    "host_version": env!("CARGO_PKG_VERSION"), "files": saved,
                }))?,
            )
        });
    if kept.is_err() {
        let _ = std::fs::remove_dir_all(&before);
        return Err("The current data could not be copied first; nothing was restored.".into());
    }
    outcome.before = name;
    for saved in &manifest.files {
        put_back(&base.join(&saved.file), saved, &pending.id).map_err(|_| {
            "A file could not be put back; the copy taken first holds the data as it was."
                .to_owned()
        })?;
    }
    // The link to the account store is this installation's now: its credentials were not restored.
    let settings = crate::store::settings::LocalSettings::at(profile);
    if let Ok(mut config) = settings.config()
        && config.cloud != link
    {
        config.cloud = link;
        settings
            .save_config(&config)
            .map_err(|_| "The account store link could not be kept.".to_owned())?;
    }
    Ok(manifest.files.len())
}

fn put_back(source: &Path, saved: &Saved, id: &str) -> Result<()> {
    if let Some(parent) = saved.from.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = saved.from.with_extension(format!("restore-{id}"));
    std::fs::copy(source, &temporary)?;
    if saved.sqlite {
        for suffix in ["-wal", "-shm"] {
            let mut name = saved.from.as_os_str().to_owned();
            name.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(name));
        }
    }
    security::replace_private_file(&temporary, &saved.from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_backup_name_is_accepted() {
        assert!(valid_name("cloud-1793748961"));
        assert!(valid_name("cloud-1793748961-2"));
        for name in [
            "cloud-",
            "cloud-x",
            "../cloud-1",
            "cloud-1/..",
            "cloud-1-2-3",
            "before-restore-1",
            "cloud-1-abc",
        ] {
            assert!(!valid_name(name), "{name}");
        }
    }

    #[test]
    fn a_backup_puts_back_only_paths_of_this_profile() {
        let places = (PathBuf::from("/p/profile"), PathBuf::from("/p/data"));
        assert!(allowed(Path::new("/p/data/assistant.db"), true, &places));
        assert!(allowed(Path::new("/p/profile/config.toml"), false, &places));
        assert!(allowed(
            Path::new("/p/profile/implementation/registry.json"),
            false,
            &places
        ));
        assert!(!allowed(Path::new("/p/profile/../etc/x.db"), true, &places));
        assert!(!allowed(Path::new("/etc/passwd"), false, &places));
        assert!(!allowed(
            Path::new("/p/profile/keys/cloud-content.json"),
            false,
            &places
        ));
        assert!(!allowed(Path::new("/p/data/assistant.db"), false, &places));
        assert!(!allowed(Path::new("relative.db"), true, &places));
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(version("0.10.40") > version("0.10.9"));
        assert!(version("x").is_none());
    }

    #[tokio::test]
    async fn a_restore_keeps_the_current_data_first_and_the_link_as_it_is() {
        let root_dir = tempfile::tempdir().unwrap();
        let host = super::super::tests::host(root_dir.path());
        let profile = host.state.settings.profile().to_path_buf();
        let mut config = host.state.settings.config().unwrap();
        config.llm.language = crate::config::Language::Es;
        host.state.settings.save_config(&config).unwrap();
        let backup = migration::back_up_data(&host, "migrate").unwrap();
        let name = backup["path"]
            .as_str()
            .and_then(|p| Path::new(p).file_name()?.to_str().map(str::to_owned))
            .unwrap();
        // Changed since the backup: the language, and the link to the account store.
        let mut config = host.state.settings.config().unwrap();
        config.llm.language = crate::config::Language::En;
        config.cloud = Cloud {
            endpoint: "https://store.example.invalid".into(),
            installation: "00000000-0000-4000-8000-000000000001".into(),
            mode: CloudMode::Mirror,
            token_expires_at: 0,
        };
        host.state.settings.save_config(&config).unwrap();
        assert!(check(&profile, "cloud-1").is_err());
        assert!(check(&profile, &name).is_ok());
        write_private(
            &root(&profile).join(PENDING),
            &serde_json::to_string(&Pending {
                id: "synthetic".into(),
                backup: name.clone(),
                requested_at: 1,
                assistant: true,
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(apply_pending(&profile), Some(true));
        assert!(!root(&profile).join(PENDING).exists());
        let outcome = last(&profile).unwrap();
        assert!(outcome.ok, "{}", outcome.error);
        assert!(outcome.files > 0);
        let restored = crate::store::settings::LocalSettings::at(&profile)
            .config()
            .unwrap();
        assert_eq!(restored.llm.language, crate::config::Language::Es);
        assert_eq!(restored.cloud.mode, CloudMode::Mirror);
        // The data as it was just before is kept, with the language it had.
        let before = root(&profile).join(&outcome.before);
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(before.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["restoring"], json!(name));
        let toml = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| {
                f["from"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("config.toml"))
            })
            .unwrap();
        let kept = std::fs::read_to_string(before.join(toml["file"].as_str().unwrap())).unwrap();
        assert!(kept.contains("store.example.invalid"));
        // Nothing pending: a start applies nothing.
        assert_eq!(apply_pending(&profile), None);
    }

    #[test]
    fn a_tampered_backup_is_refused() {
        let root_dir = tempfile::tempdir().unwrap();
        let host = super::super::tests::host(root_dir.path());
        let profile = host.state.settings.profile().to_path_buf();
        let backup = migration::back_up_data(&host, "promote").unwrap();
        let base = PathBuf::from(backup["path"].as_str().unwrap());
        let name = base.file_name().unwrap().to_str().unwrap().to_owned();
        let manifest_path = base.join("manifest.json");
        let original = std::fs::read_to_string(&manifest_path).unwrap();
        let mut manifest: Value = serde_json::from_str(&original).unwrap();
        manifest["files"][0]["from"] = json!("/etc/synthetic.db");
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();
        assert!(
            check(&profile, &name)
                .unwrap_err()
                .contains("does not match")
        );
        let mut newer: Value = serde_json::from_str(&original).unwrap();
        newer["host_version"] = json!("99.0.0");
        std::fs::write(&manifest_path, newer.to_string()).unwrap();
        assert!(
            check(&profile, &name)
                .unwrap_err()
                .contains("newer version")
        );
        std::fs::write(&manifest_path, &original).unwrap();
        let database = serde_json::from_str::<Manifest>(&original)
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.sqlite)
            .unwrap();
        std::fs::write(base.join(&database.file), b"not a database").unwrap();
        assert!(check(&profile, &name).unwrap_err().contains("damaged"));
    }
}
