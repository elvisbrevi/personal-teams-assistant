//! `pta service` against the CI runner's real service manager: install, a start by the manager
//! standing in for a login, a second start that exits because the profile is owned, and
//! uninstall. The profile is a temporary directory (`--profile-dir`), never a real one, and the
//! entry is removed at the end whatever happens.
//!
//! It registers an entry with the operating system, so it runs only where CI asks for it
//! (`PTA_SERVICE_LIVE_TEST=1`); everywhere else it returns at once. The definitions and command
//! sequences themselves are tested on every platform in `src/app/service/tests.rs`.
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

fn live() -> bool {
    std::env::var_os("PTA_SERVICE_LIVE_TEST").is_some_and(|v| v == "1")
}

struct Profile {
    _root: tempfile::TempDir,
    dir: PathBuf,
}

impl Profile {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("profile");
        std::fs::create_dir_all(&dir).unwrap();
        Self { _root: root, dir }
    }

    fn pta(&self, args: &[&str]) -> (i32, Value) {
        let output = Command::new(env!("CARGO_BIN_EXE_pta"))
            .args(["--json", "--non-interactive"])
            .args(args)
            .env("PTA_PROFILE_DIR", &self.dir)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let reply = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "pta {args:?}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().unwrap_or(-1), reply)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, reply) = self.pta(args);
        assert!(
            code == 0 && reply["ok"] == true,
            "pta {args:?}: {code} {reply}"
        );
        reply["data"].clone()
    }

    /// Whether a host owns the profile, without starting one.
    fn host_running(&self) -> bool {
        let (_, reply) = self.pta(&["status"]);
        reply["data"]["host_running"] == true
    }

    fn wait(&self, running: bool, what: &str, limit: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < limit {
            if self.host_running() == running {
                return true;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        eprintln!("service live test: {what} did not happen within {limit:?}");
        false
    }

    fn quit(&self) {
        let _ = self.pta(&["app", "quit"]);
        self.wait(false, "the host quitting", Duration::from_secs(60));
    }
}

/// The operating system's own command for the entry.
fn manager(args: &[&str]) -> bool {
    let program = match args[0] {
        "launchctl" => "/bin/launchctl",
        other => other,
    };
    Command::new(program)
        .args(&args[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// Whether this runner has the manager the entry needs.
fn manager_available() -> bool {
    if cfg!(target_os = "macos") {
        manager(&["launchctl", "print", &format!("gui/{}", uid())])
    } else if cfg!(windows) {
        manager(&["schtasks", "/Query", "/FO", "CSV", "/NH"])
    } else {
        manager(&["systemctl", "--user", "show-environment"])
    }
}

/// What a login does: the manager starts the entry.
fn login(name: &str) -> bool {
    if cfg!(target_os = "macos") {
        manager(&["launchctl", "kickstart", &format!("gui/{}/{name}", uid())])
    } else if cfg!(windows) {
        manager(&["schtasks", "/Run", "/TN", name])
    } else {
        manager(&["systemctl", "--user", "start", name])
    }
}

/// Removes whatever is left of the entry, even after a failed assertion.
struct Cleanup<'a> {
    profile: &'a Profile,
    name: Option<String>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = self.profile.pta(&["service", "uninstall", "--confirmed"]);
        self.profile.quit();
        if let Some(name) = &self.name {
            if cfg!(target_os = "macos") {
                manager(&["launchctl", "bootout", &format!("gui/{}/{name}", uid())]);
            } else if cfg!(windows) {
                manager(&["schtasks", "/Delete", "/TN", name, "/F"]);
            } else {
                manager(&["systemctl", "--user", "stop", name]);
                manager(&["systemctl", "--user", "disable", name]);
            }
        }
    }
}

fn mentions(reply: &Value, path: &Path) -> bool {
    let text = reply.to_string();
    let path = path.to_string_lossy();
    text.contains(path.as_ref()) || text.contains(&path.replace('\\', "\\\\"))
}

#[test]
fn a_login_starts_the_host_through_the_service_manager_and_uninstall_removes_only_its_entry() {
    if !live() {
        return;
    }
    if !manager_available() {
        eprintln!("service live test: SKIPPED, this runner has no per-user service manager");
        return;
    }
    let profile = Profile::new();
    let mut cleanup = Cleanup {
        profile: &profile,
        name: None,
    };
    let before = profile.ok(&["service", "status"]);
    assert_eq!(before["supported"], true, "{before}");
    assert_eq!(before["installed"], false, "{before}");
    cleanup.name = before["name"].as_str().map(str::to_owned);
    let name = cleanup.name.clone().unwrap();

    // Installing needs an explicit confirmation.
    let (code, refused) = profile.pta(&["service", "install"]);
    assert_eq!((code, refused["code"].as_str()), (2, Some("invalid_input")));

    let installed = profile.ok(&["service", "install", "--confirmed"]);
    eprintln!(
        "service live test: installed {name} through {}",
        installed["manager"]
    );
    assert_eq!(installed["installed"], true, "{installed}");
    assert_eq!(installed["current"], true, "{installed}");
    assert_eq!(installed["changed"], true, "{installed}");
    // Repeating it changes nothing.
    let again = profile.ok(&["service", "install", "--confirmed"]);
    assert_eq!(again["changed"], false, "{again}");
    // Nothing that reveals the profile or the executable.
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_personal-teams-assistant"));
    for reply in [&before, &installed, &again] {
        assert!(
            !mentions(reply, &profile.dir) && !mentions(reply, &exe),
            "{reply}"
        );
    }

    // The host pta started is not the manager's: quit it, then log in.
    profile.quit();
    assert!(login(&name), "the service manager refused to start {name}");
    let started = profile.wait(true, "the manager's start", Duration::from_secs(60));
    if cfg!(windows) && !started {
        // A task at logon runs with the user's interactive session; a runner that is a service
        // without one cannot start it. The definition, idempotence and removal are still checked.
        eprintln!(
            "service live test: the scheduled task did not start (no interactive session on this runner)"
        );
    } else {
        assert!(
            started,
            "the host did not start through the service manager"
        );
        eprintln!("service live test: the manager started the host and it answers pta status");
        let running = profile.ok(&["service", "status"]);
        assert_eq!(running["running"], true, "{running}");
        assert_eq!(running["last_error"], Value::Null, "{running}");
        // Started by the entry, the host opened no page and did not start the assistant.
        let status = profile.ok(&["status"]);
        assert_eq!(status["running"], false, "{status}");

        // A second start exits cleanly: the profile is owned.
        let second = Command::new(&exe)
            .args(["--host", "--service", "--profile-dir"])
            .arg(&profile.dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let output = wait_child(second, Duration::from_secs(30));
        assert_eq!(output, Some(0), "a second start must exit cleanly");
        assert!(profile.host_running());
        eprintln!(
            "service live test: a second start exited with 0 while the first owns the profile"
        );
    }

    // Uninstall removes the entry; a host it runs keeps running until it quits.
    let removed = profile.ok(&["service", "uninstall", "--confirmed"]);
    assert_eq!(removed["installed"], false, "{removed}");
    assert_eq!(removed["changed"], true, "{removed}");
    let gone = profile.ok(&["service", "uninstall", "--confirmed"]);
    assert_eq!(gone["changed"], false, "{gone}");
    eprintln!("service live test: uninstalled {name}");
}

fn wait_child(mut child: std::process::Child, limit: Duration) -> Option<i32> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if let Ok(Some(status)) = child.try_wait() {
            return status.code();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    None
}
