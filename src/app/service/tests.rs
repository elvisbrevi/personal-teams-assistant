//! The three definitions, the managers' command sequences through a scripted [`Runner`], the
//! authority of the operations and the assistant's intent across a login. Nothing here asks a real
//! service manager; `tests/service.rs` does, on the CI runners only.
use super::*;
use crate::app::variables::tests::{call, host};
use authority::Transport::{Agent, Browser, Local, Machine};
use std::cell::RefCell;

/// Answers each command by the first scripted prefix it starts with; anything else succeeds empty.
#[derive(Default)]
struct Script {
    answers: Vec<(String, Ran)>,
    log: RefCell<Vec<String>>,
}

impl Script {
    fn answer(mut self, prefix: &str, ok: bool, stdout: &str) -> Self {
        self.answers.push((
            prefix.to_owned(),
            Ran {
                ok,
                stdout: stdout.to_owned(),
            },
        ));
        self
    }
    fn commands(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
    /// The commands that change something (status reads are left out).
    fn changes(&self) -> Vec<String> {
        self.commands()
            .into_iter()
            .filter(|c| {
                !c.starts_with("launchctl print")
                    && !c.starts_with("systemctl --user show")
                    && !c.starts_with("loginctl")
                    && !c.starts_with("schtasks /Query")
            })
            .collect()
    }
}

impl Runner for Script {
    fn run(&self, program: &str, args: &[String]) -> Ran {
        let line = format!("{program} {}", args.join(" "));
        self.log.borrow_mut().push(line.clone());
        self.answers
            .iter()
            .find(|(prefix, _)| line.starts_with(prefix.as_str()))
            .map(|(_, ran)| ran.clone())
            .unwrap_or(Ran {
                ok: true,
                stdout: String::new(),
            })
    }
}

fn target(manager: Manager, root: &Path, isolated: bool) -> Target {
    let profile = root.join("profile with space");
    fs::create_dir_all(&profile).unwrap();
    Target {
        manager,
        exe: root.join("bin").join("personal-teams-assistant"),
        id: id(&profile),
        profile,
        isolated,
        home: root.join("home"),
        config_home: root.join("home/.config"),
        uid: Some(501),
        user: "WORKSTATION\\synthetic".into(),
    }
}

const SHOW_STOPPED: &str = "LoadState=loaded\nActiveState=inactive\nSubState=dead\nResult=success\nExecMainStatus=0\nUnitFileState=enabled\n";

#[test]
fn one_entry_per_profile_and_its_name_reveals_no_path() {
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (root.path().join("a"), root.path().join("b"));
    assert_eq!(id(&a), id(&a));
    assert_ne!(id(&a), id(&b));
    for manager in [Manager::Launchd, Manager::Systemd, Manager::TaskScheduler] {
        let t = target(manager, root.path(), false);
        let name = t.name();
        assert!(name.contains(&t.id));
        assert!(!name.contains('/') && !name.contains('\\') && !name.contains("profile"));
    }
}

#[test]
fn the_launch_agent_starts_the_host_in_the_background_with_a_minimal_path() {
    let root = tempfile::tempdir().unwrap();
    let mut t = target(Manager::Launchd, root.path(), true);
    // A Mac's paths, whatever the test runs on (a Windows drive letter is no PATH entry).
    t.exe = PathBuf::from("/Users/synthetic/.cargo/bin/personal-teams-assistant");
    t.home = PathBuf::from("/Users/synthetic");
    t.profile = PathBuf::from("/Users/synthetic/Profiles/work & play");
    let plist = t.definition();
    assert!(plist.contains(MARKER));
    assert!(plist.contains(&format!("<string>{}</string>", t.name())));
    let exe = format!("<string>{}</string>", xml(&t.exe.to_string_lossy()));
    assert!(plist.contains(&exe));
    for flag in ["--host", FLAG, PROFILE_FLAG] {
        assert!(
            plist.contains(&format!("<string>{flag}</string>")),
            "{flag}"
        );
    }
    assert!(plist.contains(&xml(&t.profile.to_string_lossy())));
    // A clean exit (a quit, a second start) is not restarted; a crash is.
    assert!(plist.contains("<key>SuccessfulExit</key>\n    <false/>"));
    assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
    // The update's helper is in its own process group and survives the host it replaces.
    assert!(plist.contains("<key>AbandonProcessGroup</key>\n  <true/>"));
    assert_eq!(
        plist.matches("<dict>").count(),
        plist.matches("</dict>").count()
    );
    // The only variable is PATH, which starts with the executable's directory.
    assert_eq!(plist.matches("<key>").count(), 9);
    let path = t.path();
    assert!(path.starts_with(&t.exe.parent().unwrap().to_string_lossy().into_owned()));
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        assert!(path.split(':').any(|d| d == dir), "{dir}");
    }
    // The standard profile is not named.
    let standard = target(Manager::Launchd, root.path(), false);
    assert!(!standard.definition().contains(PROFILE_FLAG));
}

#[test]
fn the_systemd_unit_quotes_its_command_and_keeps_the_update_helper_alive() {
    let root = tempfile::tempdir().unwrap();
    let mut t = target(Manager::Systemd, root.path(), true);
    t.exe = root
        .path()
        .join("odd \"dir\" 100%")
        .join("personal-teams-assistant");
    let unit = t.definition();
    assert!(unit.starts_with(&format!("# {MARKER}")));
    let exec = unit
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .unwrap();
    assert!(exec.starts_with('"'));
    assert!(exec.contains("\\\"dir\\\" 100%%"), "{exec}");
    assert!(exec.contains(&format!("\"--host\" \"{FLAG}\" \"{PROFILE_FLAG}\"")));
    for line in [
        "Type=simple",
        "Restart=on-failure",
        "KillMode=process",
        "WantedBy=default.target",
    ] {
        assert!(unit.lines().any(|l| l == line), "{line}");
    }
    let environment: Vec<&str> = unit
        .lines()
        .filter(|l| l.starts_with("Environment"))
        .collect();
    assert_eq!(environment.len(), 1);
    assert!(environment[0].starts_with("Environment=\"PATH="));
    assert!(!t.path().contains("homebrew"));
}

#[test]
fn the_scheduled_task_runs_at_this_users_logon_without_limits_or_elevation() {
    let root = tempfile::tempdir().unwrap();
    let mut t = target(Manager::TaskScheduler, root.path(), true);
    t.exe = PathBuf::from(r"C:\Users\Synthetic User\.cargo\bin\personal-teams-assistant.exe");
    t.profile = PathBuf::from(r"C:\Users\Synthetic User\Profiles\work\");
    let task = t.definition();
    assert!(task.contains(MARKER));
    assert!(task.contains("<LogonTrigger>"));
    assert_eq!(
        task.matches("<UserId>WORKSTATION\\synthetic</UserId>")
            .count(),
        2
    );
    for element in [
        "<LogonType>InteractiveToken</LogonType>",
        "<RunLevel>LeastPrivilege</RunLevel>",
        "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
        "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
        "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
        r"<Command>C:\Users\Synthetic User\.cargo\bin\personal-teams-assistant.exe</Command>",
    ] {
        assert!(task.contains(element), "{element}");
    }
    // The trailing backslash is doubled so the closing quote stays a quote.
    assert!(task.contains(&format!(
        r#"<Arguments>--host {FLAG} {PROFILE_FLAG} &quot;C:\Users\Synthetic User\Profiles\work\\&quot;</Arguments>"#
    )));
    // No variable can be set by a task: the profile is an argument, nothing else is passed.
    assert!(!task.contains("PATH"));
    let bytes = utf16(&task);
    assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
    assert_eq!(bytes.len(), 2 + task.encode_utf16().count() * 2);
}

#[test]
fn windows_arguments_survive_the_command_line() {
    for (word, quoted) in [
        ("plain", "plain"),
        ("", "\"\""),
        ("with space", "\"with space\""),
        (r"C:\a b\", r#""C:\a b\\""#),
        (r#"say "hi""#, r#""say \"hi\"""#),
        (r#"C:\x\"y"#, r#""C:\x\\\"y""#),
    ] {
        assert_eq!(windows_word(word), quoted, "{word}");
    }
}

#[test]
fn definitions_hold_no_account_variable_or_credential() {
    let root = tempfile::tempdir().unwrap();
    for manager in [Manager::Launchd, Manager::Systemd, Manager::TaskScheduler] {
        let definition = target(manager, root.path(), true).definition();
        for name in [
            "TOKEN",
            "SECRET",
            "STATE_ENCRYPTION_KEY",
            "_FILE",
            "PTA_PROFILE_DIR",
            "PTA_UPDATE_HANDOVER",
        ] {
            assert!(!definition.contains(name), "{manager:?} {name}");
        }
    }
}

#[test]
fn launchd_install_is_idempotent_and_uninstall_removes_only_its_own_entry() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Launchd, root.path(), false);
    let service = format!("gui/501/{}", t.name());
    // Not loaded yet.
    let first = Script::default().answer("launchctl print", false, "");
    assert_eq!(install(&t, &first), Ok(true));
    let file = t.file().unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), t.definition());
    assert_eq!(
        first.changes(),
        [format!("launchctl bootstrap gui/501 {}", file.display())]
    );
    // Loaded, same definition: nothing to do.
    let again = Script::default().answer("launchctl print", true, "\tstate = not running\n");
    assert_eq!(install(&t, &again), Ok(false));
    assert!(again.changes().is_empty());
    // Running with another executable: refused, the file is untouched.
    let mut moved = t.clone();
    moved.exe = root.path().join("elsewhere/personal-teams-assistant");
    let running = Script::default().answer("launchctl print", true, "\tstate = running\n");
    assert_eq!(install(&moved, &running).unwrap_err().0, "state_conflict");
    assert_eq!(fs::read_to_string(&file).unwrap(), t.definition());
    // Stopped with another executable: replaced and loaded again.
    let stopped = Script::default().answer("launchctl print", true, "\tstate = not running\n");
    assert_eq!(install(&moved, &stopped), Ok(true));
    assert_eq!(
        stopped.changes(),
        [
            format!("launchctl bootout {service}"),
            format!("launchctl bootstrap gui/501 {}", file.display())
        ]
    );
    // Uninstall while the job runs this host: the file goes, the running host is left alone.
    let busy = Script::default().answer("launchctl print", true, "\tstate = running\n");
    assert_eq!(uninstall(&moved, &busy), Ok(true));
    assert!(busy.changes().is_empty());
    assert!(!file.exists());
    assert_eq!(
        uninstall(
            &moved,
            &Script::default().answer("launchctl print", false, "")
        ),
        Ok(false)
    );
    // A file with this name that pta did not write is never changed or removed.
    fs::write(&file, "<plist>someone else's</plist>").unwrap();
    let foreign = Script::default().answer("launchctl print", false, "");
    assert_eq!(install(&t, &foreign).unwrap_err().0, "state_conflict");
    assert_eq!(uninstall(&t, &foreign).unwrap_err().0, "state_conflict");
    assert!(foreign.changes().is_empty());
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "<plist>someone else's</plist>"
    );
}

#[test]
fn systemd_install_enables_the_user_unit_and_says_when_lingering_is_needed() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Systemd, root.path(), false);
    let unit = t.name();
    let fresh = Script::default()
        .answer(
            "systemctl --user show",
            true,
            "LoadState=not-found\nActiveState=inactive\nUnitFileState=\n",
        )
        .answer("loginctl show-user 501", true, "no\n");
    assert_eq!(install(&t, &fresh), Ok(true));
    assert_eq!(
        fresh.changes(),
        [
            "systemctl --user daemon-reload".to_owned(),
            format!("systemctl --user enable {unit}")
        ]
    );
    let file = t.file().unwrap();
    assert!(file.ends_with(Path::new("systemd/user").join(&unit)));
    assert_eq!(fs::read_to_string(&file).unwrap(), t.definition());
    let enabled = Script::default()
        .answer("systemctl --user show", true, SHOW_STOPPED)
        .answer("loginctl show-user 501", true, "no\n");
    assert_eq!(install(&t, &enabled), Ok(false));
    assert!(enabled.changes().is_empty());
    let found = status(&t, &enabled);
    assert!(found.installed && found.current && found.loaded && !found.running);
    assert_eq!(found.linger, Some(false));
    let shown = view(&t, &found);
    assert!(
        shown["notes"][0]
            .as_str()
            .unwrap()
            .contains("loginctl enable-linger")
    );
    // Without a user manager nothing is written.
    fs::remove_file(&file).unwrap();
    let absent = Script::default().answer("systemctl", false, "");
    assert_eq!(install(&t, &absent).unwrap_err().0, "not_ready");
    assert!(!file.exists());
    // Uninstall disables, removes and reloads.
    assert_eq!(install(&t, &fresh), Ok(true));
    let removing = Script::default().answer("systemctl --user show", true, SHOW_STOPPED);
    assert_eq!(uninstall(&t, &removing), Ok(true));
    assert_eq!(
        removing.changes(),
        [
            format!("systemctl --user disable {unit}"),
            "systemctl --user daemon-reload".to_owned()
        ]
    );
    assert!(!file.exists());
}

#[test]
fn the_scheduled_task_is_created_from_a_temporary_definition_and_read_back_by_its_result() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::TaskScheduler, root.path(), true);
    let name = t.name();
    let absent = Script::default().answer("schtasks /Query", false, "");
    assert_eq!(install(&t, &absent), Ok(true));
    let changes = absent.changes();
    assert_eq!(changes.len(), 1);
    assert!(changes[0].starts_with(&format!("schtasks /Create /TN {name} /XML ")));
    assert!(changes[0].ends_with(" /F"));
    // The definition was handed over in a file that is gone again.
    let scratch = t.profile.join("service");
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
    // schtasks reformats what it stores: compare the command and the arguments.
    let stored = t
        .definition()
        .replace("  ", "    ")
        .replace("<Exec>", "<Exec>\r\n");
    let csv = |result: &str| {
        format!(
            "\"HOST\",\"\\{name}\",\"N/A\",\"Running\",\"Interactive only\",\"11/6/2026 9:00:00 AM\",\"{result}\",\"WORKSTATION\\synthetic\",\"x\"\r\n"
        )
    };
    let installed = Script::default()
        .answer(&format!("schtasks /Query /TN {name} /XML"), true, &stored)
        .answer(
            &format!("schtasks /Query /TN {name} /FO CSV"),
            true,
            &csv(TASK_RUNNING),
        );
    assert_eq!(install(&t, &installed), Ok(false));
    let found = status(&t, &installed);
    assert!(found.installed && found.current && found.running);
    assert_eq!(found.last_error, None);
    let failing = Script::default()
        .answer(&format!("schtasks /Query /TN {name} /XML"), true, &stored)
        .answer(
            &format!("schtasks /Query /TN {name} /FO CSV"),
            true,
            &csv("2147942402"),
        );
    let found = status(&t, &failing);
    assert!(!found.running);
    assert_eq!(
        found.last_error.as_deref(),
        Some("The service manager reports result 2147942402.")
    );
    // Another executable while it runs: refused.
    let mut moved = t.clone();
    moved.exe = root.path().join("elsewhere.exe");
    assert_eq!(install(&moved, &installed).unwrap_err().0, "state_conflict");
    let removing =
        Script::default().answer(&format!("schtasks /Query /TN {name} /XML"), true, &stored);
    assert_eq!(uninstall(&t, &removing), Ok(true));
    assert_eq!(
        removing.changes(),
        [format!("schtasks /Delete /TN {name} /F")]
    );
    // A task with this name that pta did not create is left alone.
    let foreign = Script::default().answer(
        "schtasks /Query",
        true,
        "<Task><Description>mine</Description></Task>",
    );
    assert_eq!(install(&t, &foreign).unwrap_err().0, "state_conflict");
    assert_eq!(uninstall(&t, &foreign).unwrap_err().0, "state_conflict");
    assert!(foreign.changes().is_empty());
}

#[test]
fn an_update_starts_and_stops_the_host_through_each_manager() {
    let root = tempfile::tempdir().unwrap();
    let launchd = target(Manager::Launchd, root.path(), false);
    let file = launchd.file().unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, launchd.definition()).unwrap();
    let service = format!("gui/501/{}", launchd.name());
    let loaded = Script::default().answer("launchctl print", true, "\tstate = not running\n");
    start(&launchd, &loaded).unwrap();
    stop(&launchd, &loaded);
    assert_eq!(
        loaded.changes(),
        [
            format!("launchctl kickstart {service}"),
            format!("launchctl kill SIGTERM {service}")
        ]
    );
    let unloaded = Script::default().answer("launchctl print", false, "");
    start(&launchd, &unloaded).unwrap();
    assert_eq!(
        unloaded.changes(),
        [format!("launchctl bootstrap gui/501 {}", file.display())]
    );

    let systemd = target(Manager::Systemd, root.path(), false);
    let file = systemd.file().unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, systemd.definition()).unwrap();
    let unit = systemd.name();
    let script = Script::default().answer("systemctl --user show", true, SHOW_STOPPED);
    start(&systemd, &script).unwrap();
    stop(&systemd, &script);
    assert_eq!(
        script.changes(),
        [
            format!("systemctl --user reset-failed {unit}"),
            format!("systemctl --user start {unit}"),
            format!("systemctl --user stop {unit}")
        ]
    );
    let refused = Script::default()
        .answer("systemctl --user show", true, SHOW_STOPPED)
        .answer("systemctl --user start", false, "");
    assert!(start(&systemd, &refused).is_err());

    let task = target(Manager::TaskScheduler, root.path(), false);
    let name = task.name();
    let script = Script::default().answer("schtasks /Query", true, &task.definition());
    start(&task, &script).unwrap();
    stop(&task, &script);
    assert_eq!(
        script.changes(),
        [
            format!("schtasks /Run /TN {name}"),
            format!("schtasks /End /TN {name}")
        ]
    );
    // An entry removed meanwhile is not started.
    let gone = Script::default().answer("schtasks /Query", false, "");
    assert!(start(&task, &gone).is_err());
    assert!(gone.changes().is_empty());
}

#[test]
fn status_reports_the_managers_last_error_without_paths() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Launchd, root.path(), true);
    let file = t.file().unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, t.definition()).unwrap();
    let crashed = Script::default().answer(
        "launchctl print",
        true,
        "\tstate = not running\n\tlast exit code = 78: EX_CONFIG\n",
    );
    let found = status(&t, &crashed);
    assert!(found.installed && found.current && found.loaded && !found.running);
    assert_eq!(
        found.last_error.as_deref(),
        Some("The service manager reports exit code 78.")
    );
    let systemd = target(Manager::Systemd, root.path(), true);
    let failed = Script::default().answer(
        "systemctl --user show",
        true,
        "ActiveState=failed\nResult=exit-code\nExecMainStatus=1\nUnitFileState=enabled\n",
    );
    assert_eq!(
        status(&systemd, &failed).last_error.as_deref(),
        Some("The service manager reports exit-code (exit status 1).")
    );
    let shown = view(&t, &found).to_string();
    for private in [
        t.profile.to_string_lossy(),
        t.home.to_string_lossy(),
        t.exe.to_string_lossy(),
    ] {
        assert!(!shown.contains(private.as_ref()), "{private}");
    }
}

#[test]
fn a_stored_task_is_read_back_whatever_its_encoding_and_escaping() {
    let root = tempfile::tempdir().unwrap();
    let mut t = target(Manager::TaskScheduler, root.path(), true);
    t.profile = PathBuf::from(r"C:\Users\Synthetic User\profile");
    // As schtasks writes it to a pipe: UTF-16, its own indentation, quotes not escaped.
    let stored = t
        .definition()
        .replace("&quot;", "\"")
        .replace("    <", "\t<");
    let name = t.name();
    let decoded = decode(&utf16(&stored));
    assert_eq!(decoded, stored);
    let script =
        Script::default().answer(&format!("schtasks /Query /TN {name} /XML"), true, &decoded);
    let found = status(&t, &script);
    assert!(found.installed && found.current && !found.foreign);
    assert_eq!(decode(b"plain"), "plain");
}

#[test]
fn csv_fields_follow_schtasks_quoting() {
    assert_eq!(
        csv_fields("\"a\",\"b, c\",\"d \"\"e\"\"\",\"0\""),
        ["a", "b, c", "d \"e\"", "0"]
    );
}

#[test]
fn the_profile_argument_is_split_off() {
    let (rest, profile) = profile_argument(
        ["--host", FLAG, PROFILE_FLAG, "/tmp/p q"]
            .map(String::from)
            .to_vec(),
    );
    assert_eq!(rest, ["--host", FLAG]);
    assert_eq!(profile, Some(PathBuf::from("/tmp/p q")));
    let (rest, profile) = profile_argument(vec!["--host".into()]);
    assert_eq!((rest.len(), profile), (1, None));
}

#[test]
fn a_restore_waits_only_while_the_lease_is_unknown() {
    assert_eq!(decide(&Ok(true)), Restore::Start);
    assert_eq!(decide(&Ok(false)), Restore::Stay);
    assert_eq!(decide(&Err(anyhow::anyhow!("offline"))), Restore::Wait);
}

#[tokio::test]
async fn only_the_owner_at_this_computer_manages_the_entry() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    for transport in [Browser, Agent, Machine] {
        for method in ["service_status", "service_install", "service_uninstall"] {
            let reply = call(&host, transport, method, json!({})).await;
            assert_eq!(reply.code, "authority_denied", "{transport:?} {method}");
        }
    }
    // No argument chooses anything: not an executable, a profile or a name.
    for args in [json!({"exe":"/bin/sh"}), json!({"name":"other"})] {
        let reply = call(&host, Local, "service_install", args).await;
        assert_eq!(reply.code, "invalid_input");
    }
}

#[tokio::test]
async fn the_persons_start_and_stop_are_the_intent_a_login_restores() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let profile = host.state.settings.profile().to_path_buf();
    assert!(!intended(&profile));
    remember(&profile, true);
    assert!(intended(&profile));
    // A start that fails records nothing.
    let reply = call(&host, Local, "start_assistant", json!({})).await;
    assert!(!reply.ok);
    assert!(intended(&profile));
    let reply = call(&host, Local, "stop_assistant", json!({})).await;
    assert!(reply.ok, "{:?}", reply.message);
    assert!(!intended(&profile));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(profile.join(INTENT))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
}

#[tokio::test]
async fn a_login_never_starts_the_assistant_without_the_intent_or_the_teams_lease() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let profile = host.state.settings.profile().to_path_buf();
    // Left stopped: nothing is asked.
    restore(host.clone(), Duration::ZERO).await;
    assert_eq!(*host.state.phase.borrow(), AssistantPhase::Stopped);
    // Left running, but another machine was selected for Teams meanwhile.
    remember(&profile, true);
    host.state
        .implementation
        .select_teams_machine("another-machine")
        .await;
    tokio::time::timeout(
        Duration::from_secs(5),
        restore(host.clone(), Duration::ZERO),
    )
    .await
    .unwrap();
    assert_eq!(*host.state.phase.borrow(), AssistantPhase::Stopped);
    assert!(host.state.running.lock().await.is_none());
    // The intent is kept for a later login on which this computer holds Teams again.
    assert!(intended(&profile));
}

#[test]
fn a_host_with_the_profiles_private_home_cannot_register_the_users_entry() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    // HOME is never under a profile in a person's session.
    assert!(!sandboxed(&profile));
}
