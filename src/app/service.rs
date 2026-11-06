//! Starting the host when the user logs in: one per-user entry of the operating system's service
//! manager for this profile and executable, without administrator rights. macOS uses a
//! LaunchAgent (`launchctl bootstrap gui/UID`), Linux a systemd user unit and Windows a scheduled
//! task at logon.
//!
//! The entry starts the host with `--host --service`: no page opens at a login, and the host never
//! starts the assistant because of the entry. The assistant runs after a login only when the
//! person left it running ([`remember`]) and this computer still holds the Teams selection and
//! lease ([`restore`]), checked by the same start every other surface uses.
//!
//! Definitions are built from a [`Target`] and commands go through a [`Runner`], so the three
//! definitions and their command sequences are tested on every platform with a scripted runner;
//! [`System`] runs the real managers through `security::program` (no console window on Windows).
//! A definition holds the executable, the launch flags, the profile when it is not the standard
//! one and, on macOS and Linux, a fixed minimal `PATH`: never an account variable or a credential.
use super::{
    authority,
    control::{self, Reply},
    *,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

/// Marks every definition this module writes; nothing without it is changed or removed.
const MARKER: &str = "Personal Teams Assistant host, created by pta service install";
/// The flag the entry adds to `--host`: the host restores the assistant's saved intent.
pub(crate) const FLAG: &str = "--service";
/// Passes an isolated profile to a host started by a manager that cannot set variables.
pub(crate) const PROFILE_FLAG: &str = "--profile-dir";
/// Where the person's last start or stop of the assistant is kept.
const INTENT: &str = "assistant-intent.json";
const LOCAL_ONLY: &str = "Starting the host at login is set up by the owner of this computer, with pta or the application's page on that computer.";
/// Windows Task Scheduler: the task is running (`SCHED_S_TASK_RUNNING`).
const TASK_RUNNING: &str = "267009";
/// Results that are not an error: success, ready, running, never run, terminated by the user.
const TASK_FINE: [&str; 5] = ["0", "267008", "267009", "267011", "267014"];
const RESTORE_ATTEMPTS: u32 = 10;
const RESTORE_PAUSE: Duration = Duration::from_secs(30);

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        "service_status" | "service_install" | "service_uninstall"
    )
}

/// The operating system's per-user service manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Manager {
    Launchd,
    Systemd,
    TaskScheduler,
}

impl Manager {
    pub(crate) fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::Launchd)
        } else if cfg!(windows) {
            Some(Self::TaskScheduler)
        } else if cfg!(target_os = "linux") {
            Some(Self::Systemd)
        } else {
            None
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd",
            Self::TaskScheduler => "task_scheduler",
        }
    }
}

/// Everything a definition depends on. Tests build synthetic ones.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub manager: Manager,
    /// The host executable the entry starts.
    pub exe: PathBuf,
    pub profile: PathBuf,
    /// The profile is not the standard one, so the entry names it.
    pub isolated: bool,
    /// 12 hex digits of the profile path's SHA-256: one entry per profile.
    pub id: String,
    pub home: PathBuf,
    /// `XDG_CONFIG_HOME` (Linux).
    pub config_home: PathBuf,
    /// The user's numeric ID (macOS and Linux).
    pub uid: Option<u32>,
    /// `DOMAIN\user` (Windows).
    pub user: String,
}

/// The identity of an entry: derived from the profile, so another profile gets its own.
pub(crate) fn id(profile: &Path) -> String {
    let path = profile
        .canonicalize()
        .unwrap_or_else(|_| profile.to_path_buf());
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

impl Target {
    /// The entry this computer would have for `profile` and `exe`.
    pub(crate) fn detect(profile: &Path, exe: &Path) -> Result<Self> {
        let manager = Manager::current().context(
            "Starting the host at login is available on macOS, Windows and Linux with systemd.",
        )?;
        let home = PathBuf::from(
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .context("home directory unavailable")?,
        );
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
            (Ok(domain), Ok(name)) if !domain.is_empty() => format!("{domain}\\{name}"),
            (_, Ok(name)) => name,
            _ => String::new(),
        };
        // A host replaced by `cargo install` while it ran reports its old file as deleted.
        let exe = PathBuf::from(
            exe.to_string_lossy()
                .trim_end_matches(" (deleted)")
                .to_owned(),
        );
        Ok(Self {
            manager,
            exe,
            profile: profile.to_path_buf(),
            isolated: control::isolated_profile_dir()?.is_some(),
            id: id(profile),
            home,
            config_home,
            uid: owner_uid(profile),
            user,
        })
    }

    /// What the operating system calls the entry; it reveals no path.
    pub(crate) fn name(&self) -> String {
        match self.manager {
            Manager::Launchd => format!("dev.personalteams.assistant.host.{}", self.id),
            Manager::Systemd => format!("personal-teams-assistant-{}.service", self.id),
            Manager::TaskScheduler => format!("PersonalTeamsAssistant-{}", self.id),
        }
    }

    /// The definition file (macOS and Linux); a scheduled task lives in the Task Scheduler.
    pub(crate) fn file(&self) -> Option<PathBuf> {
        match self.manager {
            Manager::Launchd => Some(
                self.home
                    .join("Library/LaunchAgents")
                    .join(format!("{}.plist", self.name())),
            ),
            Manager::Systemd => Some(self.config_home.join("systemd/user").join(self.name())),
            Manager::TaskScheduler => None,
        }
    }

    /// The host's arguments after the executable.
    pub(crate) fn arguments(&self) -> Vec<String> {
        let mut args = vec!["--host".to_owned(), FLAG.to_owned()];
        if self.isolated {
            args.push(PROFILE_FLAG.into());
            args.push(self.profile.to_string_lossy().into_owned());
        }
        args
    }

    /// The fixed `PATH` of a host started by launchd or systemd: the executable's directory and
    /// the usual per-user and system locations. The host also looks for the coding CLIs, `az` and
    /// `gh` in the per-user install locations itself, as it does when a desktop session starts it.
    pub(crate) fn path(&self) -> String {
        let mut dirs: Vec<PathBuf> = self
            .exe
            .parent()
            .map(Path::to_path_buf)
            .into_iter()
            .collect();
        dirs.push(self.home.join(".local/bin"));
        dirs.push(self.home.join(".cargo/bin"));
        if self.manager == Manager::Launchd {
            dirs.push("/opt/homebrew/bin".into());
        }
        dirs.extend(
            ["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from),
        );
        let mut seen = Vec::new();
        for dir in dirs {
            let dir = dir.to_string_lossy().into_owned();
            if !dir.is_empty() && !dir.contains(':') && !seen.contains(&dir) {
                seen.push(dir);
            }
        }
        seen.join(":")
    }

    /// The whole definition as the manager receives it.
    pub(crate) fn definition(&self) -> String {
        match self.manager {
            Manager::Launchd => plist(self),
            Manager::Systemd => unit(self),
            Manager::TaskScheduler => task(self),
        }
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid.unwrap_or_default())
    }
    fn service(&self) -> String {
        format!("{}/{}", self.domain(), self.name())
    }
}

#[cfg(unix)]
fn owner_uid(profile: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    // The profile and its parent belong to the user running the host.
    fs::metadata(profile)
        .ok()
        .or_else(|| fs::metadata(profile.parent()?).ok())
        .map(|m| m.uid())
}
#[cfg(not(unix))]
fn owner_uid(_: &Path) -> Option<u32> {
    None
}

// ---------------------------------------------------------------------------------------------
// The three definitions.

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn plist(t: &Target) -> String {
    let arguments: String = std::iter::once(t.exe.to_string_lossy().into_owned())
        .chain(t.arguments())
        .map(|a| format!("    <string>{}</string>\n", xml(&a)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- {MARKER}; `pta service uninstall` removes it. -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{arguments}  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>30</integer>
  <key>AbandonProcessGroup</key>
  <true/>
</dict>
</plist>
"#,
        label = xml(&t.name()),
        path = xml(&t.path()),
    )
}

/// One systemd argument in double quotes: no specifier or variable is expanded.
fn systemd_word(word: &str) -> String {
    format!(
        "\"{}\"",
        word.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}

fn unit(t: &Target) -> String {
    let exec: Vec<String> = std::iter::once(t.exe.to_string_lossy().into_owned())
        .chain(t.arguments())
        .map(|a| systemd_word(&a))
        .collect();
    let path = format!("PATH={}", t.path())
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!(
        "# {MARKER}; `pta service uninstall` removes it.
[Unit]
Description=Personal Teams Assistant host

[Service]
Type=simple
ExecStart={exec}
Environment=\"{path}\"
Restart=on-failure
RestartSec=30
# Only the host: an update's helper that it started outlives it and starts the new one.
KillMode=process
TimeoutStopSec=90

[Install]
WantedBy=default.target
",
        exec = exec.join(" "),
    )
}

/// One argument as `CommandLineToArgvW` reads it back.
fn windows_word(word: &str) -> String {
    if !word.is_empty() && !word.contains([' ', '\t', '"']) {
        return word.to_owned();
    }
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for c in word.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(slashes * 2 + 1));
                quoted.push('"');
                slashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(slashes));
                quoted.push(c);
                slashes = 0;
            }
        }
    }
    quoted.push_str(&"\\".repeat(slashes * 2));
    quoted.push('"');
    quoted
}

fn task_arguments(t: &Target) -> String {
    t.arguments()
        .iter()
        .map(|a| windows_word(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn task(t: &Target) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{MARKER} ({id}); pta service uninstall removes it.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        id = t.id,
        user = xml(&t.user),
        command = xml(&t.exe.to_string_lossy()),
        arguments = xml(&task_arguments(t)),
    )
}

/// The Task Scheduler reads UTF-16 with a byte order mark.
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

// ---------------------------------------------------------------------------------------------
// Running the manager.

/// What one command of the manager answered.
#[derive(Clone, Debug, Default)]
pub(crate) struct Ran {
    pub ok: bool,
    pub stdout: String,
}

/// Runs the manager's commands; tests script the answers.
pub(crate) trait Runner {
    /// `program` is the manager's command (`launchctl`, `systemctl`, `loginctl`, `schtasks`).
    fn run(&self, program: &str, args: &[String]) -> Ran;
}

/// The real managers, without a console window and with no input.
pub(crate) struct System;

impl System {
    fn binary(program: &str) -> PathBuf {
        if program == "launchctl" {
            return PathBuf::from("/bin/launchctl");
        }
        if program == "schtasks" {
            return std::env::var_os("SystemRoot")
                .map(|root| PathBuf::from(root).join("System32").join("schtasks.exe"))
                .filter(|p| p.is_file())
                .unwrap_or_else(|| PathBuf::from("schtasks.exe"));
        }
        ["/usr/bin", "/bin"]
            .iter()
            .map(|dir| Path::new(dir).join(program))
            .find(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from(program))
    }
}

impl Runner for System {
    fn run(&self, program: &str, args: &[String]) -> Ran {
        match security::program(Self::binary(program))
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        {
            Ok(output) => Ran {
                ok: output.status.success(),
                stdout: decode(&output.stdout),
            },
            Err(_) => Ran::default(),
        }
    }
}

/// A manager's output: `schtasks /XML` may write UTF-16, everything else UTF-8 or ASCII.
fn decode(bytes: &[u8]) -> String {
    let utf16 =
        bytes.starts_with(&[0xFF, 0xFE]) || (bytes.len() >= 4 && bytes[1] == 0 && bytes[3] == 0);
    if !utf16 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    let units: Vec<u16> = body
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The text of the first `<tag>` element of a stored task, with XML's entities read back.
fn element(document: &str, tag: &str) -> Option<String> {
    let start = document.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = start + document[start..].find(&format!("</{tag}>"))?;
    Some(
        document[start..end]
            .trim()
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

// ---------------------------------------------------------------------------------------------
// Status.

/// What the manager says about the entry. Nothing in it is a path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Status {
    /// The manager answered (a Linux session without a user manager does not).
    pub available: bool,
    pub installed: bool,
    /// An entry with this name exists that this module did not create.
    pub foreign: bool,
    /// The entry starts this executable for this profile, as `install` would write it.
    pub current: bool,
    /// The manager loaded the entry (launchd) or enabled it (systemd).
    pub loaded: bool,
    pub running: bool,
    pub last_error: Option<String>,
    /// Linux: the user manager keeps running without a session.
    pub linger: Option<bool>,
}

fn read_file(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

pub(crate) fn status(t: &Target, runner: &impl Runner) -> Status {
    match t.manager {
        Manager::Launchd => {
            let content = t.file().and_then(|f| read_file(&f));
            let print = runner.run("launchctl", &words(&["print", &t.service()]));
            let code = print.stdout.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("last exit code = ")
                    .map(|v| {
                        v.chars()
                            .enumerate()
                            .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && *c == '-'))
                            .map(|(_, c)| c)
                            .collect::<String>()
                    })
                    .and_then(|v| v.parse::<i64>().ok())
            });
            Status {
                available: true,
                installed: content.as_deref().is_some_and(|c| c.contains(MARKER)),
                foreign: content.as_deref().is_some_and(|c| !c.contains(MARKER)),
                current: content.as_deref() == Some(t.definition().as_str()),
                loaded: print.ok,
                running: print.ok
                    && print
                        .stdout
                        .lines()
                        .any(|line| line.trim() == "state = running"),
                last_error: code
                    .filter(|c| print.ok && *c != 0)
                    .map(|c| format!("The service manager reports exit code {c}.")),
                linger: None,
            }
        }
        Manager::Systemd => {
            let content = t.file().and_then(|f| read_file(&f));
            let show = runner.run(
                "systemctl",
                &words(&[
                    "--user",
                    "show",
                    &t.name(),
                    "--property=LoadState,ActiveState,SubState,Result,ExecMainStatus,UnitFileState",
                ]),
            );
            let value = |key: &str| {
                show.stdout.lines().find_map(|line| {
                    line.strip_prefix(key)
                        .and_then(|rest| rest.strip_prefix('='))
                        .map(str::trim)
                        .map(str::to_owned)
                })
            };
            let result = value("Result").filter(|r| !r.is_empty() && r != "success");
            let linger = t.uid.and_then(|uid| {
                let ran = runner.run(
                    "loginctl",
                    &words(&[
                        "show-user",
                        &uid.to_string(),
                        "--property=Linger",
                        "--value",
                    ]),
                );
                ran.ok.then(|| ran.stdout.trim() == "yes")
            });
            Status {
                available: show.ok,
                installed: content.as_deref().is_some_and(|c| c.contains(MARKER)),
                foreign: content.as_deref().is_some_and(|c| !c.contains(MARKER)),
                current: content.as_deref() == Some(t.definition().as_str()),
                loaded: show.ok && value("UnitFileState").as_deref() == Some("enabled"),
                running: show.ok && value("ActiveState").as_deref() == Some("active"),
                last_error: result.filter(|_| show.ok).map(|r| {
                    let code = value("ExecMainStatus").unwrap_or_default();
                    format!("The service manager reports {r} (exit status {code}).")
                }),
                linger,
            }
        }
        Manager::TaskScheduler => {
            let query = runner.run("schtasks", &words(&["/Query", "/TN", &t.name(), "/XML"]));
            let installed = query.ok && query.stdout.contains(MARKER);
            // The stored task is reformatted by the Task Scheduler: compare the values.
            let current = installed
                && element(&query.stdout, "Command").is_some_and(|command| {
                    command
                        .trim_matches('"')
                        .eq_ignore_ascii_case(&t.exe.to_string_lossy())
                })
                && element(&query.stdout, "Arguments").as_deref()
                    == Some(task_arguments(t).as_str());
            let detail = installed
                .then(|| {
                    runner.run(
                        "schtasks",
                        &words(&["/Query", "/TN", &t.name(), "/FO", "CSV", "/NH", "/V"]),
                    )
                })
                .filter(|ran| ran.ok);
            // The columns are fixed; their titles and the status text follow the system's
            // language, so only the numeric last result is read.
            let result = detail.and_then(|ran| {
                csv_fields(ran.stdout.lines().find(|l| !l.trim().is_empty())?)
                    .get(6)
                    .cloned()
            });
            Status {
                available: true,
                installed,
                foreign: query.ok && !installed,
                current,
                loaded: installed,
                running: result.as_deref() == Some(TASK_RUNNING),
                last_error: result
                    .filter(|r| !TASK_FINE.contains(&r.as_str()))
                    .map(|r| format!("The service manager reports result {r}.")),
                linger: None,
            }
        }
    }
}

/// The fields of one CSV line as `schtasks /FO CSV` writes it.
fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    fields.push(field);
    fields
}

// ---------------------------------------------------------------------------------------------
// Install, uninstall, start and stop.

/// Why a change was refused, as `(code, message)`.
type Refusal = (&'static str, String);

const FOREIGN: &str = "An entry with this name exists that pta did not create; it was left unchanged. Remove it with the operating system's tools first.";
const UNAVAILABLE: &str = "The systemd user manager is not available in this session (systemctl --user does not answer). Log in to a session with systemd, or enable lingering with loginctl enable-linger, and try again.";
const SANDBOXED: &str = "This host runs with the isolated profile's private home, as pta starts it, so it cannot register an entry for your login. Quit it with pta app quit, start personal-teams-assistant --host from your own session with PTA_PROFILE_DIR set, and try again.";
const RUNNING_OTHER: &str = "The service is running with another executable or profile. Quit the host with pta app quit, then install the service again.";

fn failed(what: &str) -> Refusal {
    (
        "operation_failed",
        format!("The service manager refused to {what}."),
    )
}

/// Registers the entry; with an up-to-date entry it changes nothing. Returns whether it changed.
pub(crate) fn install(t: &Target, runner: &impl Runner) -> std::result::Result<bool, Refusal> {
    let before = status(t, runner);
    if before.foreign {
        return Err(("state_conflict", FOREIGN.into()));
    }
    let definition = t.definition();
    match t.manager {
        Manager::Launchd => {
            let file = t.file().expect("a LaunchAgent has a file");
            if before.current && before.loaded {
                return Ok(false);
            }
            if !before.current && before.running {
                return Err(("state_conflict", RUNNING_OTHER.into()));
            }
            if !before.current {
                write_definition(&file, &definition)?;
            }
            if before.loaded {
                // A stopped job with another definition is replaced; nothing runs it now.
                runner.run("launchctl", &words(&["bootout", &t.service()]));
            }
            let ran = runner.run(
                "launchctl",
                &words(&["bootstrap", &t.domain(), &file.to_string_lossy()]),
            );
            if !ran.ok {
                return Err(failed("load the LaunchAgent"));
            }
            Ok(true)
        }
        Manager::Systemd => {
            if !before.available {
                return Err(("not_ready", UNAVAILABLE.into()));
            }
            if before.current && before.loaded {
                return Ok(false);
            }
            if !before.current {
                write_definition(&t.file().expect("a unit has a file"), &definition)?;
            }
            if !runner
                .run("systemctl", &words(&["--user", "daemon-reload"]))
                .ok
            {
                return Err(failed("reload its units"));
            }
            if !runner
                .run("systemctl", &words(&["--user", "enable", &t.name()]))
                .ok
            {
                return Err(failed("enable the unit"));
            }
            Ok(true)
        }
        Manager::TaskScheduler => {
            if before.current {
                return Ok(false);
            }
            if before.running {
                return Err(("state_conflict", RUNNING_OTHER.into()));
            }
            if t.user.is_empty() {
                return Err((
                    "not_ready",
                    "The Windows user name is unavailable in this session.".into(),
                ));
            }
            let dir = t.profile.join("service");
            let file = dir.join(format!("task-{}.xml", uuid::Uuid::new_v4().simple()));
            let written =
                fs::create_dir_all(&dir).and_then(|()| fs::write(&file, utf16(&definition)));
            if written.is_err() {
                return Err(failed("receive the task definition"));
            }
            let ran = runner.run(
                "schtasks",
                &words(&[
                    "/Create",
                    "/TN",
                    &t.name(),
                    "/XML",
                    &file.to_string_lossy(),
                    "/F",
                ]),
            );
            let _ = fs::remove_file(&file);
            if !ran.ok {
                return Err(failed("create the scheduled task"));
            }
            Ok(true)
        }
    }
}

fn write_definition(file: &Path, definition: &str) -> std::result::Result<(), Refusal> {
    let dir = file.parent().expect("a definition file has a directory");
    fs::create_dir_all(dir)
        .map_err(anyhow::Error::from)
        .and_then(|()| write_private(file, definition))
        .map_err(|_| failed("receive the definition file"))
}

/// Removes the entry this module created and nothing else. A host the entry is running keeps
/// running until it quits; nothing starts it at the next login. Returns whether it removed one.
pub(crate) fn uninstall(t: &Target, runner: &impl Runner) -> std::result::Result<bool, Refusal> {
    let before = status(t, runner);
    if before.foreign {
        return Err(("state_conflict", FOREIGN.into()));
    }
    match t.manager {
        Manager::Launchd => {
            if before.loaded && !before.running {
                runner.run("launchctl", &words(&["bootout", &t.service()]));
            }
            if !before.installed {
                return Ok(false);
            }
            fs::remove_file(t.file().expect("a LaunchAgent has a file"))
                .map_err(|_| failed("remove the LaunchAgent"))?;
            Ok(true)
        }
        Manager::Systemd => {
            if !before.installed {
                return Ok(false);
            }
            if before.available {
                runner.run("systemctl", &words(&["--user", "disable", &t.name()]));
            }
            fs::remove_file(t.file().expect("a unit has a file"))
                .map_err(|_| failed("remove the unit"))?;
            if before.available {
                runner.run("systemctl", &words(&["--user", "daemon-reload"]));
            }
            Ok(true)
        }
        Manager::TaskScheduler => {
            if !before.installed {
                return Ok(false);
            }
            if !runner
                .run("schtasks", &words(&["/Delete", "/TN", &t.name(), "/F"]))
                .ok
            {
                return Err(failed("delete the scheduled task"));
            }
            Ok(true)
        }
    }
}

/// Starts the host through the manager, so the manager tracks the process it runs.
pub(crate) fn start(t: &Target, runner: &impl Runner) -> Result<()> {
    let before = status(t, runner);
    ensure!(before.installed, "The service is no longer installed.");
    let ran = match t.manager {
        Manager::Launchd if before.loaded => {
            runner.run("launchctl", &words(&["kickstart", &t.service()]))
        }
        Manager::Launchd => runner.run(
            "launchctl",
            &words(&[
                "bootstrap",
                &t.domain(),
                &t.file()
                    .expect("a LaunchAgent has a file")
                    .to_string_lossy(),
            ]),
        ),
        Manager::Systemd => {
            runner.run("systemctl", &words(&["--user", "reset-failed", &t.name()]));
            runner.run("systemctl", &words(&["--user", "start", &t.name()]))
        }
        Manager::TaskScheduler => runner.run("schtasks", &words(&["/Run", "/TN", &t.name()])),
    };
    ensure!(ran.ok, "The service manager did not start the application.");
    Ok(())
}

/// Asks the manager to stop the host it runs (after a quit that did not end it).
pub(crate) fn stop(t: &Target, runner: &impl Runner) {
    match t.manager {
        Manager::Launchd => {
            runner.run("launchctl", &words(&["kill", "SIGTERM", &t.service()]));
        }
        Manager::Systemd => {
            runner.run("systemctl", &words(&["--user", "stop", &t.name()]));
        }
        Manager::TaskScheduler => {
            runner.run("schtasks", &words(&["/End", "/TN", &t.name()]));
        }
    }
}

/// Whether an installed entry starts this executable for this profile: then an update restarts
/// the host through the manager.
pub(crate) fn manages(profile: &Path, exe: &Path) -> Option<Target> {
    let target = Target::detect(profile, exe).ok()?;
    let found = status(&target, &System);
    (found.installed && found.current).then_some(target)
}

// ---------------------------------------------------------------------------------------------
// The host operations.

/// Whether this process was started by the entry.
pub(crate) fn launched_by_service() -> bool {
    std::env::args().any(|a| a == FLAG)
}

fn view(t: &Target, s: &Status) -> Value {
    let mut notes = Vec::new();
    if s.installed && !s.current {
        notes.push("The entry starts another executable or profile: install the service again to point it at this one.".to_owned());
    }
    if s.installed && t.manager == Manager::Systemd && s.linger == Some(false) {
        notes.push("Lingering is off, so the host runs only while you are logged in. To keep it running without a session, run loginctl enable-linger.".to_owned());
    }
    if !s.available {
        notes.push(UNAVAILABLE.to_owned());
    }
    json!({
        "supported": true,
        "manager": t.manager.label(),
        "name": t.name(),
        "installed": s.installed,
        "current": s.current,
        "running": s.running,
        "last_error": s.last_error,
        "linger": s.linger,
        "this_host": launched_by_service(),
        "notes": notes,
    })
}

fn unsupported() -> Value {
    json!({
        "supported": false, "installed": false, "running": false, "last_error": null,
        "notes": ["Starting the host at login is available on macOS, Windows and Linux with systemd."],
    })
}

/// The executable an entry should start: this host's own.
/// A host `pta` started for an isolated profile runs with that profile's private home and
/// without the session's variables, so it cannot see or register the user's own entry.
fn sandboxed(profile: &Path) -> bool {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .is_some_and(|home| home.starts_with(profile))
}

fn own_target(host: &Host) -> Option<Target> {
    let exe = std::env::current_exe().ok()?;
    Target::detect(host.state.settings.profile(), &exe).ok()
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Reply {
    let Ok(invocation) = authority::invocation(&host.state) else {
        return Reply::error("authority_denied", 6, authority::ACCOUNT_UNAVAILABLE);
    };
    if !maintenance::is_local_owner(&invocation.scope, invocation.transport) {
        return Reply::error("authority_denied", 6, LOCAL_ONLY);
    }
    if request
        .args
        .as_object()
        .is_some_and(|args| !args.is_empty())
    {
        return Reply::error("invalid_input", 2, "This operation takes no arguments.");
    }
    if sandboxed(host.state.settings.profile()) {
        return Reply::error("not_ready", 3, SANDBOXED);
    }
    let Some(target) = own_target(host) else {
        return Reply::success(unsupported());
    };
    let method = request.method.clone();
    let worked = tokio::task::spawn_blocking(move || {
        let change = match method.as_str() {
            "service_install" => Some(install(&target, &System)),
            "service_uninstall" => Some(uninstall(&target, &System)),
            _ => None,
        };
        (change, view(&target, &status(&target, &System)))
    })
    .await;
    let Ok((change, mut data)) = worked else {
        return Reply::error(
            "operation_failed",
            1,
            "The service manager could not be asked.",
        );
    };
    match change {
        None => Reply::success(data),
        Some(Err((code, message))) => {
            let exit = match code {
                "state_conflict" => 6,
                "not_ready" => 3,
                _ => 1,
            };
            Reply::error(code, exit, &message)
        }
        Some(Ok(changed)) => {
            data["changed"] = json!(changed);
            let note = if request.method == "service_install" {
                "The host starts when you log in. It never starts the assistant by itself: the assistant runs after a login only if it was running before and this computer still holds the Teams selection."
            } else if data["running"] == true {
                "Nothing starts the host at the next login. The host that runs now keeps running until you quit it."
            } else {
                "Nothing starts the host at the next login."
            };
            if let Some(notes) = data["notes"].as_array_mut() {
                notes.insert(0, json!(note));
            }
            Reply::success(data)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The assistant's intent across a login.

#[derive(Serialize, Deserialize)]
struct Intent {
    running: bool,
    at: i64,
}

/// Records the person's last start or stop of the assistant. Internal stops (a quit, a restart
/// for new settings, a lost Teams lease) do not change it.
pub(crate) fn remember(profile: &Path, running: bool) {
    let intent = Intent {
        running,
        at: chrono::Utc::now().timestamp(),
    };
    if let Ok(text) = serde_json::to_string(&intent) {
        let _ = write_private(&profile.join(INTENT), &text);
    }
}

/// Whether the assistant was left running.
pub(crate) fn intended(profile: &Path) -> bool {
    fs::read_to_string(profile.join(INTENT))
        .ok()
        .and_then(|text| serde_json::from_str::<Intent>(&text).ok())
        .is_some_and(|intent| intent.running)
}

/// What a host started by the entry does about the assistant, given the Teams lease check.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Restore {
    Start,
    /// Another machine holds the Teams selection or lease: the assistant stays stopped.
    Stay,
    /// The answer is unknown (no network yet after a login): ask again later.
    Wait,
}

pub(crate) fn decide(allowed: &Result<bool>) -> Restore {
    match allowed {
        Ok(true) => Restore::Start,
        Ok(false) => Restore::Stay,
        Err(_) => Restore::Wait,
    }
}

/// Starts the assistant on a host the entry started, only when it was left running and this
/// computer still holds the Teams selection and lease. The start is the ordinary one, which
/// checks the lease again; it is asked once, never repeated after it answered.
pub(super) async fn restore(host: Arc<Host>, pause: Duration) {
    if !intended(host.state.settings.profile()) {
        return;
    }
    for _ in 0..RESTORE_ATTEMPTS {
        match decide(&implementation::fleet::teams_allowed(&host.state).await) {
            Restore::Start => {
                let reply =
                    control::dispatch(&host, control::Request::new("start_assistant")).await;
                if !reply.ok {
                    tracing::warn!(event = "service_assistant_not_started", code = %reply.code);
                }
                return;
            }
            Restore::Stay => {
                tracing::info!(event = "service_assistant_left_stopped");
                return;
            }
            Restore::Wait => tokio::time::sleep(pause).await,
        }
    }
    tracing::warn!(event = "service_assistant_lease_unknown");
}

pub(super) fn restore_pause() -> Duration {
    RESTORE_PAUSE
}

/// Splits `--profile-dir DIR` off the host's arguments.
pub(crate) fn profile_argument(args: Vec<String>) -> (Vec<String>, Option<PathBuf>) {
    let mut rest = Vec::with_capacity(args.len());
    let mut profile = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == PROFILE_FLAG {
            profile = iter.next().map(PathBuf::from);
        } else {
            rest.push(arg);
        }
    }
    (rest, profile)
}

#[cfg(test)]
mod tests;
