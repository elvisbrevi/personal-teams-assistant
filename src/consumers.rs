//! Consumers of account variables: the only code that may turn a stored value into a use.
//!
//! A consumer is selected by code, never by model or metadata text. Two kinds exist:
//!
//! * **Built-in consumers** (a fixed registry) use a bound variable themselves, for a fixed
//!   destination and operation. Only the reference and a status leave them.
//! * **Launchers** are local programs a person explicitly defined on a trusted personal machine.
//!   A run hands the process only the variables the launcher names, as environment variables of
//!   that one process; its output is never captured, so nothing it prints can reach an agent.
//!
//! Variable storage stays generic: names, descriptions and values carry no consumer rules. The
//! rules (key shape, reserved environment names, destinations, program, directory) live here.
use crate::variables::{Error, Secret};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

type Result<T> = std::result::Result<T, Error>;

fn input(message: impl Into<String>) -> Error {
    Error::Input(message.into())
}

/// A built-in consumer: what it is for, where it sends the value and what it can do.
pub struct BuiltIn {
    pub id: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    /// The only place the value is ever sent (HTTPS origin).
    pub destination: &'static str,
    pub slot: &'static str,
    /// The application credential this slot replaces while it is bound.
    pub credential: &'static str,
    pub operations: &'static [&'static str],
}

pub const DEEPSEEK: BuiltIn = BuiltIn {
    id: "deepseek",
    title: "DeepSeek API",
    summary: "The key the assistant uses to call the DeepSeek language-model API when DeepSeek is in the model chain.",
    destination: "https://api.deepseek.com",
    slot: "api_key",
    credential: "DEEPSEEK_API_KEY",
    operations: &["check"],
};

pub const BUILT_IN: &[BuiltIn] = &[DEEPSEEK];

pub fn built_in(id: &str) -> Option<&'static BuiltIn> {
    BUILT_IN.iter().find(|consumer| consumer.id == id)
}

/// The built-in consumer whose application credential has this name.
pub fn built_in_credential(credential: &str) -> Option<&'static BuiltIn> {
    BUILT_IN
        .iter()
        .find(|consumer| consumer.credential == credential)
}

/// The consumer's own rules for a value, checked at every use. The message never echoes it.
pub fn validate_value(consumer: &BuiltIn, value: &str) -> Result<()> {
    let _ = consumer;
    if !(8..=256).contains(&value.len()) || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(input(
            "This variable is not a valid API key for this consumer: it must be 8 to 256 visible characters without spaces or line breaks.",
        ));
    }
    Ok(())
}

/// What a connectivity check established, never the response body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckResult {
    /// `ok`, `rejected` (the provider refused the key), `failed` or `unreachable`.
    pub status: &'static str,
    pub http_status: Option<u16>,
}

/// Ask the consumer's destination whether the key is accepted (`GET /models` with the key as a
/// bearer token). Redirects are refused so the key cannot be forwarded elsewhere.
pub async fn check(consumer: &BuiltIn, value: &Secret) -> CheckResult {
    check_at(consumer.destination, value).await
}

pub async fn check_at(origin: &str, value: &Secret) -> CheckResult {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            return CheckResult {
                status: "failed",
                http_status: None,
            };
        }
    };
    match client
        .get(format!("{}/models", origin.trim_end_matches('/')))
        .bearer_auth(value.expose())
        .send()
        .await
    {
        Ok(response) => {
            let code = response.status().as_u16();
            CheckResult {
                status: match code {
                    200..=299 => "ok",
                    401 | 403 => "rejected",
                    _ => "failed",
                },
                http_status: Some(code),
            }
        }
        Err(_) => CheckResult {
            status: "unreachable",
            http_status: None,
        },
    }
}

// ---- launchers -------------------------------------------------------------------------

pub const MAX_LAUNCHERS: usize = 32;
pub const MAX_ENV: usize = 8;
pub const MAX_ARGS: usize = 32;
pub const MAX_ARG: usize = 2048;
pub const DEFAULT_TIMEOUT: u64 = 60;
pub const MAX_TIMEOUT: u64 = 600;

/// Environment names a variable can never be injected as: runtime and loader controls, search
/// paths, proxy and certificate settings, tool configuration that runs code, and the
/// application's own state, machine, webhook and tunnel keys. Compared without case.
const RESERVED_ENV: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USER",
    "USERNAME",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "TMP",
    "TEMP",
    "IFS",
    "ENV",
    "BASH_ENV",
    "PS4",
    "PROMPT_COMMAND",
    "CDPATH",
    "GLOBIGNORE",
    "SHELLOPTS",
    "BASHOPTS",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "NODE_OPTIONS",
    "NODE_PATH",
    "NODE_EXTRA_CA_CERTS",
    "BUN_OPTIONS",
    "JAVA_TOOL_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "_JAVA_OPTIONS",
    "CLASSPATH",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTFLAGS",
    "CARGO_HOME",
    "CARGO_BUILD_RUSTFLAGS",
    "RUSTUP_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_CA_BUNDLE",
    "REQUESTS_CA_BUNDLE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "AZURE_CONFIG_DIR",
    "GRAPH_WEBHOOK_SECRET",
    "STATE_ENCRYPTION_KEY",
    "CLOUDFLARE_TUNNEL_TOKEN",
    "GITHUB_OAUTH_TOKENS",
    "TYPESAFE_API_KEY",
    "LANG",
    "LANGUAGE",
    "TZ",
    "RUBYOPT",
    "RUBYLIB",
    "PERLLIB",
    "GOFLAGS",
    "GOPATH",
    "GOROOT",
    "GOPROXY",
    "GOBIN",
];
const RESERVED_PREFIXES: &[&str] = &[
    "LD_",
    "DYLD_",
    "LC_",
    "PYTHON",
    "PERL5",
    "GEM_",
    "XDG_",
    "GIT_",
    "SSH_",
    "GPG_",
    "BASH_FUNC_",
    "PTA_",
    "TUNNEL_",
    "NPM_CONFIG_",
    "PIP_",
    "UV_",
    "BUN_",
    "DENO_",
    "CARGO_",
    "RUSTUP_",
];

/// Whether an environment name is protected from variable injection.
pub fn reserved_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    RESERVED_ENV.contains(&upper.as_str())
        || RESERVED_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

pub fn validate_env_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !first_ok || name.len() > 64 || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(input(
            "An environment name has letters, digits and _ (up to 64) and does not start with a digit.",
        ));
    }
    if reserved_env(name) {
        return Err(input(format!(
            "{name} is a protected runtime or application setting and cannot be set from a variable."
        )));
    }
    Ok(())
}

pub fn validate_launcher_id(id: &str) -> Result<()> {
    let mut chars = id.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase())
        || id.len() > 64
        || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(input(
            "A launcher name is lowercase letters, digits, - and _ (up to 64) and starts with a letter.",
        ));
    }
    Ok(())
}

/// A local program a person approved to receive named variables.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launcher {
    pub id: String,
    pub description: String,
    /// Canonical absolute path of the executable.
    pub program: PathBuf,
    /// SHA-256 of the executable when it was approved; a changed file does not run.
    pub digest: String,
    pub args: Vec<String>,
    /// Canonical working directory inside the machine's allowed roots.
    pub directory: PathBuf,
    /// Environment name to variable entry ID. Values are resolved at every run.
    pub env: BTreeMap<String, String>,
    pub timeout_seconds: u64,
    pub created_at: i64,
}

fn digest(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|_| input("The program cannot be read; check the path."))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    metadata.is_file()
}

/// Everything a definition fixes in code. `directory` must already be confined to the
/// machine's allowed roots by the caller; the entry IDs are resolved and checked at every run.
pub struct Definition {
    pub id: String,
    pub description: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub directory: PathBuf,
    pub env: BTreeMap<String, String>,
    pub timeout_seconds: Option<u64>,
}

pub fn define(definition: Definition, now: i64) -> Result<Launcher> {
    validate_launcher_id(&definition.id)?;
    crate::variables::validate_description(&definition.description)?;
    if !definition.program.is_absolute() {
        return Err(input("The program must be an absolute path."));
    }
    let program = definition
        .program
        .canonicalize()
        .map_err(|_| input("The program does not exist."))?;
    if !executable(&program) {
        return Err(input("The program is not an executable file."));
    }
    if definition.args.len() > MAX_ARGS
        || definition
            .args
            .iter()
            .any(|a| a.len() > MAX_ARG || a.contains('\0'))
    {
        return Err(input(
            "Use at most 32 fixed arguments of up to 2048 characters each, without NUL.",
        ));
    }
    if definition.env.is_empty() || definition.env.len() > MAX_ENV {
        return Err(input(
            "Name between one and eight variables the program receives.",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (name, entry) in &definition.env {
        validate_env_name(name)?;
        if !seen.insert(name.to_ascii_uppercase()) {
            return Err(input(
                "Environment names must differ without regard to case.",
            ));
        }
        uuid::Uuid::parse_str(entry).map_err(|_| {
            input("Each environment name refers to a variable entry ID (see variables list).")
        })?;
    }
    let timeout_seconds = definition.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT);
    if !(1..=MAX_TIMEOUT).contains(&timeout_seconds) {
        return Err(input("The timeout is between 1 and 600 seconds."));
    }
    Ok(Launcher {
        digest: digest(&program)?,
        program,
        id: definition.id,
        description: definition.description,
        args: definition.args,
        directory: definition.directory,
        env: definition
            .env
            .into_iter()
            .map(|(name, entry)| {
                let entry = uuid::Uuid::parse_str(&entry)
                    .map(|u| u.hyphenated().to_string())
                    .unwrap_or(entry);
                (name, entry)
            })
            .collect(),
        timeout_seconds,
        created_at: now,
    })
}

/// The machine-local list of launchers. It is never synchronized: paths and approvals belong to
/// this installation.
pub struct Launchers {
    path: PathBuf,
    /// The signed-in account these launchers belong to (an opaque qualifier); `None` for the
    /// installation's own list.
    owner: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LauncherFile {
    #[serde(default)]
    launchers: Vec<Launcher>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
}

impl Launchers {
    pub fn new(profile: &Path) -> Self {
        Self {
            path: profile.join("launchers.json"),
            owner: None,
        }
    }
    /// The launchers of the account this installation is signed in to. They reference that
    /// account's variables, so a list written for another account (or by an interrupted
    /// sign-out) reads as empty and is replaced by the next change.
    pub fn account(profile: &Path, owner: &str) -> Self {
        Self {
            path: profile.join("launchers-account.json"),
            owner: Some(owner.into()),
        }
    }
    fn load(&self) -> Result<LauncherFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<LauncherFile>(&bytes)
                .map_err(|_| {
                    Error::Storage("The launcher list is unreadable; nothing was run.".into())
                })
                .map(|file| {
                    if file.owner == self.owner {
                        file
                    } else {
                        LauncherFile::default()
                    }
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(LauncherFile::default())
            }
            Err(error) => Err(error.into()),
        }
    }
    fn save(&self, file: &LauncherFile) -> Result<()> {
        use std::io::Write;
        let file = &LauncherFile {
            launchers: file.launchers.clone(),
            owner: self.owner.clone(),
        };
        let parent = self
            .path
            .parent()
            .ok_or_else(|| input("Invalid profile."))?;
        crate::security::private_dir(parent)?;
        let temp = parent.join(format!(".launchers-{}", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut handle = options.open(&temp)?;
            crate::security::protect_file(&temp)?;
            handle
                .write_all(&serde_json::to_vec_pretty(file).map_err(|_| {
                    Error::Storage("The launcher list could not be saved.".into())
                })?)?;
            handle.sync_all()?;
            crate::security::replace_private_file(&temp, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
    pub fn list(&self) -> Result<Vec<Launcher>> {
        let mut launchers = self.load()?.launchers;
        launchers.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(launchers)
    }
    pub fn get(&self, id: &str) -> Result<Launcher> {
        self.load()?
            .launchers
            .into_iter()
            .find(|l| l.id == id)
            .ok_or_else(|| input("There is no launcher with that name on this machine."))
    }
    /// Add a launcher; an existing name is a conflict, never an overwrite.
    pub fn add(&self, launcher: Launcher) -> Result<()> {
        let mut file = self.load()?;
        if file.launchers.iter().any(|l| l.id == launcher.id) {
            return Err(Error::Conflict(
                "A launcher with that name already exists. Remove it first to define it again."
                    .into(),
            ));
        }
        if file.launchers.len() >= MAX_LAUNCHERS {
            return Err(input("At most 32 launchers can be defined on a machine."));
        }
        file.launchers.push(launcher);
        self.save(&file)
    }
    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut file = self.load()?;
        let before = file.launchers.len();
        file.launchers.retain(|l| l.id != id);
        let removed = file.launchers.len() != before;
        if removed {
            self.save(&file)?;
        }
        Ok(removed)
    }
}

/// What a launcher run established. Output of the process is not captured.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunResult {
    /// `succeeded`, `failed`, `timed_out` or `not_started`.
    pub status: &'static str,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

/// Run the launcher's program with exactly the selected values. The program and its digest are
/// checked again here; the environment is the vetted minimal one plus the selected variables.
pub async fn run(launcher: &Launcher, values: Vec<(String, Secret)>) -> Result<RunResult> {
    let program = launcher
        .program
        .canonicalize()
        .map_err(|_| Error::Conflict("The approved program is no longer there.".into()))?;
    if program != launcher.program || !executable(&program) || digest(&program)? != launcher.digest
    {
        return Err(Error::Conflict(
            "The approved program changed since it was approved. Remove the launcher and define it again after reviewing the program.".into(),
        ));
    }
    if !launcher.directory.is_dir() {
        return Err(Error::Conflict(
            "The approved working directory is no longer there.".into(),
        ));
    }
    let started = std::time::Instant::now();
    let mut command = tokio::process::Command::new(&program);
    command
        .args(&launcher.args)
        .current_dir(&launcher.directory)
        .env_clear()
        .envs(crate::llm::child_env(&program))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    for (name, value) in &values {
        command.env(name, value.expose());
    }
    let Ok(mut child) = command.spawn() else {
        return Ok(RunResult {
            status: "not_started",
            exit_code: None,
            duration_ms: 0,
        });
    };
    let waited =
        tokio::time::timeout(Duration::from_secs(launcher.timeout_seconds), child.wait()).await;
    let duration_ms = started.elapsed().as_millis() as u64;
    Ok(match waited {
        Ok(Ok(status)) => RunResult {
            status: if status.success() {
                "succeeded"
            } else {
                "failed"
            },
            exit_code: status.code(),
            duration_ms,
        },
        Ok(Err(_)) => RunResult {
            status: "failed",
            exit_code: None,
            duration_ms,
        },
        Err(_) => {
            let _ = child.kill().await;
            RunResult {
                status: "timed_out",
                exit_code: None,
                duration_ms,
            }
        }
    })
}

#[cfg(test)]
mod tests;
