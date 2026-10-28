//! Replacement, undo, recovery and work checks on real files in temporary directories, with
//! synthetic executables (plain bytes: nothing here runs a program) and injected failures.
use super::*;
use serde_json::json;

const OLD_HOST: &[u8] = b"previous host executable";
const OLD_CLI: &[u8] = b"previous pta executable";
const NEW_HOST: &[u8] = b"new host executable, a little longer";
const NEW_CLI: &[u8] = b"new pta executable, also longer";

fn crates_io(version: &str) -> String {
    format!("{CRATE} {version} (registry+https://github.com/rust-lang/crates.io-index)")
}

struct Fixture {
    _tmp: tempfile::TempDir,
    maintenance: PathBuf,
    root: PathBuf,
    staged: PathBuf,
    profile: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("install");
        let staged = tmp.path().join("stage/root");
        let profile = tmp.path().join("profile");
        let data = tmp.path().join("data");
        let maintenance = profile.join("maintenance");
        for dir in [
            root.join("bin"),
            staged.join("bin"),
            data.clone(),
            maintenance.clone(),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(root.join("bin").join(exe_name(CRATE)), OLD_HOST).unwrap();
        fs::write(root.join("bin").join(exe_name(CLI)), OLD_CLI).unwrap();
        fs::write(staged.join("bin").join(exe_name(CRATE)), NEW_HOST).unwrap();
        fs::write(staged.join("bin").join(exe_name(CLI)), NEW_CLI).unwrap();
        let record = json!({"bins":["personal-teams-assistant","pta"],"features":[],
            "all_features":false,"no_default_features":true,"profile":"release"});
        fs::write(
            root.join(".crates2.json"),
            json!({"installs":{crates_io("0.9.13"): record, "other 1.0.0 (registry+x)": {"bins":["other"]}}})
                .to_string(),
        )
        .unwrap();
        fs::write(
            root.join(".crates.toml"),
            format!("[v1]\n\"{}\" = [\"personal-teams-assistant\", \"pta\"]\n\"other 1.0.0 (registry+x)\" = [\"other\"]\n", crates_io("0.9.13")),
        )
        .unwrap();
        fs::write(
            staged.join(".crates2.json"),
            json!({"installs":{crates_io("0.9.14"): record}}).to_string(),
        )
        .unwrap();
        fs::write(
            staged.join(".crates.toml"),
            format!(
                "[v1]\n\"{}\" = [\"personal-teams-assistant\", \"pta\"]\n",
                crates_io("0.9.14")
            ),
        )
        .unwrap();
        fs::write(
            profile.join("config.toml"),
            "[server]\nbind = \"127.0.0.1:1\"\n",
        )
        .unwrap();
        Self {
            _tmp: tmp,
            maintenance,
            root,
            staged,
            profile,
            data,
        }
    }

    fn profiles(&self) -> Vec<Profile> {
        vec![Profile {
            dir: self.profile.clone(),
            data: self.data.clone(),
        }]
    }

    fn manifest(&self) -> Manifest {
        let binaries = [CRATE, CLI]
            .iter()
            .map(|n| {
                (
                    n.to_string(),
                    digest(&self.staged.join("bin").join(exe_name(n))).unwrap(),
                )
            })
            .collect();
        Manifest {
            schema: 1,
            version: "0.9.14".into(),
            staged_at: 0,
            features: Vec::new(),
            all_features: false,
            no_default_features: true,
            target: None,
            binaries,
        }
    }

    fn journal(&self) -> Journal {
        Journal::new(
            "job1",
            "0.9.13",
            self.manifest(),
            &self.root,
            &self.staged,
            Intent {
                launch: vec!["--headless".into()],
                assistant: true,
                subscriptions: 1,
                tunnel: false,
            },
            self.profiles(),
        )
    }

    fn apply(&self) -> Apply {
        Apply::new(&self.maintenance, self.journal()).without_pause()
    }

    fn bin(&self, name: &str) -> Vec<u8> {
        fs::read(self.root.join("bin").join(exe_name(name))).unwrap()
    }

    fn assert_previous(&self) {
        assert_eq!(self.bin(CRATE), OLD_HOST);
        assert_eq!(self.bin(CLI), OLD_CLI);
        let records = fs::read_to_string(self.root.join(".crates2.json")).unwrap();
        assert!(
            records.contains("0.9.13") && !records.contains("0.9.14"),
            "{records}"
        );
        let toml = fs::read_to_string(self.root.join(".crates.toml")).unwrap();
        assert!(
            toml.contains("0.9.13") && !toml.contains("0.9.14"),
            "{toml}"
        );
    }

    fn leftovers(&self) -> Vec<String> {
        fs::read_dir(self.root.join("bin"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect()
    }
}

fn database(path: &Path, statements: &str) {
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(statements).unwrap();
}

#[test]
fn a_matched_pair_replaces_both_executables_and_cargos_records() {
    let f = Fixture::new();
    let mut apply = f.apply();
    apply.back_up().unwrap();
    apply.replace().unwrap();
    assert_eq!(f.bin(CRATE), NEW_HOST);
    assert_eq!(f.bin(CLI), NEW_CLI);
    assert!(apply.installed_is_new());
    assert!(!apply.installed_is_previous());
    let records: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(f.root.join(".crates2.json")).unwrap()).unwrap();
    let keys: Vec<&String> = records["installs"].as_object().unwrap().keys().collect();
    assert!(keys.iter().any(|k| k.contains("0.9.14")), "{keys:?}");
    assert!(!keys.iter().any(|k| k.contains("0.9.13")), "{keys:?}");
    assert!(
        keys.iter().any(|k| k.starts_with("other ")),
        "other packages are untouched"
    );
    let toml = fs::read_to_string(f.root.join(".crates.toml")).unwrap();
    assert!(
        toml.contains("0.9.14") && !toml.contains("0.9.13") && toml.contains("other 1.0.0"),
        "{toml}"
    );
    // The previous pair is kept until the new one is found healthy.
    assert_eq!(f.leftovers().len(), 2, "{:?}", f.leftovers());
    apply.commit().unwrap();
    assert!(f.leftovers().is_empty(), "{:?}", f.leftovers());
    assert_eq!(read(&f.maintenance).unwrap().phase, Phase::Committed);
}

#[test]
fn a_failure_at_any_point_restores_both_previous_executables_and_records() {
    for point in [
        "backup",
        "copy:personal-teams-assistant",
        "retire:personal-teams-assistant",
        "install:personal-teams-assistant",
        "copy:pta",
        "retire:pta",
        "install:pta",
        "records",
    ] {
        let f = Fixture::new();
        let target = point.to_owned();
        let mut apply = f.apply().with_hook(move |at| {
            if at == target {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        });
        let result = apply.back_up().and_then(|()| apply.replace());
        assert!(result.is_err(), "{point} must fail");
        // The hook is for the forward path; the rollback runs clean, like production.
        let mut undo = Apply::new(&f.maintenance, read(&f.maintenance).unwrap()).without_pause();
        if point == "backup" {
            // Nothing was installed yet, so there is nothing to restore.
            f.assert_previous();
        } else {
            undo.roll_back().unwrap();
        }
        f.assert_previous();
        assert!(
            f.leftovers().iter().all(|n| n.ends_with(".failed")),
            "{point}: {:?}",
            f.leftovers()
        );
        // Running it again changes nothing.
        let mut again = Apply::new(&f.maintenance, read(&f.maintenance).unwrap()).without_pause();
        again.roll_back().unwrap();
        f.assert_previous();
    }
}

#[test]
fn a_crash_midway_is_undone_by_recovery_once_its_driver_is_gone() {
    let f = Fixture::new();
    let mut apply = f.apply().with_hook(|at| {
        if at == "install:pta" {
            Err(io::Error::other("the process died"))
        } else {
            Ok(())
        }
    });
    apply.back_up().unwrap();
    assert!(apply.replace().is_err());
    // Half replaced: the host is new, pta is missing from its name.
    assert_eq!(f.bin(CRATE), NEW_HOST);
    assert!(!f.root.join("bin").join(exe_name(CLI)).exists());
    // A fresh hand-over is not an interrupted update.
    assert_eq!(recover(&f.maintenance, now()).unwrap(), Recovery::Active);
    f.assert_previous_pending();
    assert_eq!(
        recover(&f.maintenance, now() + GRACE_SECONDS + 1).unwrap(),
        Recovery::RolledBack
    );
    f.assert_previous();
    let journal = read(&f.maintenance).unwrap();
    assert_eq!(journal.phase, Phase::RolledBack);
    assert!(journal.error.unwrap().contains("interrupted"));
    // Finished journals are left alone.
    assert_eq!(
        recover(&f.maintenance, now() + 1000).unwrap(),
        Recovery::Idle
    );
}

impl Fixture {
    fn assert_previous_pending(&self) {
        assert_eq!(
            self.bin(CRATE),
            NEW_HOST,
            "recovery must wait for the grace period"
        );
    }
}

#[test]
fn a_live_driver_blocks_recovery_and_a_second_update() {
    let f = Fixture::new();
    let mut apply = f.apply();
    apply.phase(Phase::Replacing).unwrap();
    let lock = Lock::try_acquire(&f.maintenance)
        .unwrap()
        .expect("first driver");
    assert!(
        Lock::try_acquire(&f.maintenance).unwrap().is_none(),
        "one driver at a time"
    );
    assert!(Lock::wait(&f.maintenance, Duration::from_millis(150)).is_err());
    assert_eq!(
        recover(&f.maintenance, now() + 10_000).unwrap(),
        Recovery::Active
    );
    drop(lock);
    assert!(
        Lock::try_acquire(&f.maintenance).unwrap().is_some(),
        "a dead driver releases it"
    );
}

#[test]
fn a_staged_file_changed_after_verification_is_never_installed() {
    let f = Fixture::new();
    let mut apply = f.apply();
    fs::write(f.staged.join("bin").join(exe_name(CLI)), b"tampered").unwrap();
    apply.back_up().unwrap();
    let error = apply.replace().unwrap_err().to_string();
    assert!(error.contains("changed after it was verified"), "{error}");
    Apply::new(&f.maintenance, read(&f.maintenance).unwrap())
        .without_pause()
        .roll_back()
        .unwrap();
    f.assert_previous();
}

#[test]
fn a_rollback_that_does_not_find_the_previous_pair_says_so() {
    let f = Fixture::new();
    let mut apply = f.apply();
    apply.back_up().unwrap();
    apply.replace().unwrap();
    // Someone removed the renamed-aside original.
    fs::remove_file(apply.path(CLI, "previous")).unwrap();
    let error = apply.roll_back().unwrap_err().to_string();
    assert!(
        error.contains("not the one that was installed before") || error.contains("restored"),
        "{error}"
    );
}

#[test]
fn retrying_stops_after_a_bounded_number_of_attempts() {
    let mut calls = 0;
    let result: io::Result<()> = retry(Duration::ZERO, || {
        calls += 1;
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    });
    assert!(result.is_err());
    assert_eq!(calls, RETRIES);
    let mut calls = 0;
    let result = retry(Duration::ZERO, || {
        calls += 1;
        if calls < 3 {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        } else {
            Ok(calls)
        }
    });
    assert_eq!(result.unwrap(), 3, "a lock that clears is waited out");
}

/// A running program keeps its file open without the right to delete or overwrite it; Windows still
/// lets it be renamed. A file held without that sharing cannot be replaced at all, and must fail
/// cleanly with the previous pair intact.
#[cfg(windows)]
#[test]
fn windows_running_and_locked_executables() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::new();
    let host = f.root.join("bin").join(exe_name(CRATE));
    // As a running image: readable, deletable-by-rename.
    let running = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .open(&host)
        .unwrap();
    let mut apply = f.apply();
    apply.back_up().unwrap();
    apply.replace().unwrap();
    assert!(apply.installed_is_new());
    drop(running);
    apply.roll_back().unwrap();
    f.assert_previous();
    // Held with no sharing at all (an aggressive scanner): the replacement fails and nothing changes.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(f.root.join("bin").join(exe_name(CLI)))
        .unwrap();
    let mut apply = f.apply().without_pause();
    apply.journal.id = "job2".into();
    apply.back_up().unwrap_err();
    drop(held);
}

fn assistant_db(data: &Path, status: &str) {
    database(
        &data.join("assistant.db"),
        &format!(
            "CREATE TABLE jobs (id INTEGER, status TEXT); INSERT INTO jobs VALUES (1, '{status}');"
        ),
    );
}

#[test]
fn work_in_any_affected_profile_defers_the_update() {
    let f = Fixture::new();
    assert!(busy(&f.profiles()).is_empty());
    assistant_db(&f.data, "done");
    assert!(busy(&f.profiles()).is_empty(), "finished jobs are not work");
    for status in ["processing", "sending", "refining"] {
        fs::remove_file(f.data.join("assistant.db")).unwrap();
        assistant_db(&f.data, status);
        assert_eq!(
            busy(&f.profiles()),
            ["A Teams message is being answered or sent."],
            "{status}"
        );
    }
    // A second affected profile counts as well.
    let other = f._tmp.path().join("other");
    fs::create_dir_all(other.join("implementation")).unwrap();
    fs::write(
        other.join("implementation/registry.json"),
        json!({"runs":[{"state":"running"}],"creations":{"a":{"state":"sending"}}}).to_string(),
    )
    .unwrap();
    database(
        &other.join("implementation/runs.db"),
        "CREATE TABLE agent_runs (state TEXT); INSERT INTO agent_runs VALUES ('planning');
         CREATE TABLE request_runs (state TEXT); INSERT INTO request_runs VALUES ('generating');",
    );
    database(
        &other.join("activity-registration.db"),
        "CREATE TABLE runs (status TEXT); INSERT INTO runs VALUES ('running');
         CREATE TABLE entries (state TEXT); INSERT INTO entries VALUES ('refining');",
    );
    let mut profiles = f.profiles();
    profiles.push(Profile {
        dir: other.clone(),
        data: other,
    });
    let reasons = busy(&profiles);
    for expected in [
        "A Teams message is being answered or sent.",
        "A work item implementation is in progress.",
        "Work items are being created.",
        "A coding agent is running.",
        "A request agent or creation is running.",
        "An activity review is running.",
        "An activity task is being written.",
    ] {
        assert!(
            reasons.iter().any(|r| r == expected),
            "{expected}: {reasons:?}"
        );
    }
    assert!(
        reasons
            .iter()
            .all(|r| !r.contains('/') && !r.contains('\\')),
        "no paths: {reasons:?}"
    );
}

#[test]
fn unreadable_state_counts_as_work_rather_than_a_guess() {
    let f = Fixture::new();
    fs::write(
        f.data.join("assistant.db"),
        b"this is not a database at all, just text",
    )
    .unwrap();
    fs::create_dir_all(f.profile.join("implementation")).unwrap();
    fs::write(f.profile.join("implementation/registry.json"), b"{broken").unwrap();
    let reasons = busy(&f.profiles());
    assert!(
        reasons.iter().any(|r| r.contains("could not be read")),
        "{reasons:?}"
    );
    assert_eq!(reasons.len(), 2, "{reasons:?}");
}

#[test]
fn the_data_is_copied_before_replacement_and_can_be_put_back() {
    let f = Fixture::new();
    database(
        &f.data.join("assistant.db"),
        "PRAGMA journal_mode=WAL; CREATE TABLE jobs (id INTEGER, status TEXT); INSERT INTO jobs VALUES (1,'done'); CREATE TABLE note (v TEXT); INSERT INTO note VALUES ('before');",
    );
    let mut apply = f.apply();
    apply.back_up().unwrap();
    assert!(
        apply
            .journal
            .saved
            .iter()
            .any(|s| s.sqlite && s.from.ends_with("assistant.db"))
    );
    assert!(
        apply
            .journal
            .saved
            .iter()
            .any(|s| !s.sqlite && s.from.ends_with("config.toml"))
    );
    apply.replace().unwrap();
    // The new version migrates the data...
    database(
        &f.data.join("assistant.db"),
        "ALTER TABLE note RENAME TO note_v2; INSERT INTO jobs VALUES (2,'processing');",
    );
    fs::write(f.profile.join("config.toml"), "changed").unwrap();
    // ...and the previous one cannot read it, so the copy goes back.
    apply.roll_back().unwrap();
    apply.restore_data().unwrap();
    let db = rusqlite::Connection::open(f.data.join("assistant.db")).unwrap();
    let before: String = db
        .query_row("SELECT v FROM note", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, "before");
    let jobs: i64 = db
        .query_row("SELECT count(*) FROM jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(jobs, 1);
    assert!(
        fs::read_to_string(f.profile.join("config.toml"))
            .unwrap()
            .contains("bind")
    );
    assert!(apply.journal.data_restored);
    f.assert_previous();
}

#[test]
fn only_the_latest_safety_copy_is_kept() {
    let f = Fixture::new();
    let mut first = f.apply();
    first.back_up().unwrap();
    let mut second = Apply::new(&f.maintenance, {
        let mut journal = f.journal();
        journal.id = "job2".into();
        journal
    })
    .without_pause();
    second.back_up().unwrap();
    assert!(!first.backup_location().exists());
    assert!(second.backup_location().exists());
}

#[test]
fn an_update_without_room_for_its_safety_copy_is_refused_before_anything_changes() {
    let f = Fixture::new();
    let mut manifest = f.manifest();
    manifest.binaries.get_mut(CRATE).unwrap().size = 1 << 60;
    let error = preflight(&f.maintenance, &f.root, &manifest, &f.profiles())
        .unwrap_err()
        .to_string();
    assert!(error.contains("not enough free disk space"), "{error}");
    preflight(&f.maintenance, &f.root, &f.manifest(), &f.profiles()).unwrap();
}

#[test]
fn leftovers_are_swept_only_when_no_update_is_running() {
    let f = Fixture::new();
    let mut apply = f.apply();
    apply.back_up().unwrap();
    apply.replace().unwrap();
    apply.commit().unwrap();
    // A program still held the old files, so they stayed.
    for kind in ["previous", "failed", "new"] {
        fs::write(apply.path(CLI, kind), b"x").unwrap();
    }
    fs::create_dir_all(f.maintenance.join("backup/job1")).unwrap();
    fs::create_dir_all(f.maintenance.join("staged/0.9.14")).unwrap();
    fs::create_dir_all(f.maintenance.join("staged/0.9.15")).unwrap();
    let lock = Lock::try_acquire(&f.maintenance).unwrap().unwrap();
    sweep(&f.maintenance, &f.root, super::super::Version(0, 9, 14));
    assert!(
        f.maintenance.join("backup/job1").exists(),
        "nothing is swept while an update runs"
    );
    drop(lock);
    sweep(&f.maintenance, &f.root, super::super::Version(0, 9, 14));
    assert!(f.leftovers().is_empty(), "{:?}", f.leftovers());
    assert!(!f.maintenance.join("backup").exists());
    assert!(
        !f.maintenance.join("staged/0.9.14").exists(),
        "no longer newer"
    );
    assert!(f.maintenance.join("staged/0.9.15").exists(), "still newer");
    assert_eq!(f.bin(CRATE), NEW_HOST);
}

#[test]
fn the_description_reports_success_only_for_a_committed_update() {
    let f = Fixture::new();
    let mut apply = f.apply();
    for (phase, state) in [
        (Phase::Requested, "applying"),
        (Phase::Stopping, "applying"),
        (Phase::BackingUp, "applying"),
        (Phase::Replacing, "applying"),
        (Phase::Restarting, "restarting"),
        (Phase::Verifying, "verifying"),
        (Phase::RollingBack, "rolling_back"),
        (Phase::RolledBack, "rolled_back"),
        (Phase::Failed, "failed"),
        (Phase::Committed, "success"),
    ] {
        apply.phase(phase).unwrap();
        let text = describe(&apply.journal);
        assert_eq!(text["state"], state);
        let rendered = text.to_string();
        assert!(
            !rendered.contains(f.root.to_str().unwrap()) && !rendered.contains("tmp"),
            "no path: {rendered}"
        );
    }
}

#[test]
fn startup_is_blocked_while_an_update_runs_and_unblocked_after_recovery() {
    let f = Fixture::new();
    assert_eq!(startup_block(&f.maintenance), None);
    let mut apply = f.apply();
    apply.phase(Phase::Replacing).unwrap();
    assert!(
        startup_block(&f.maintenance)
            .unwrap()
            .contains("being applied")
    );
    let mut journal = read(&f.maintenance).unwrap();
    journal.updated_at -= GRACE_SECONDS + 5;
    write(&f.maintenance, &journal).unwrap();
    assert_eq!(
        startup_block(&f.maintenance),
        None,
        "interrupted before any change: undone"
    );
    assert_eq!(read(&f.maintenance).unwrap().phase, Phase::RolledBack);
}
