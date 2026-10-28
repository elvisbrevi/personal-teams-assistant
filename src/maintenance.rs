//! Local installation maintenance: which release of this crate exists, how this installation was
//! installed and a verified, matched host/CLI pair built next to it.
//!
//! Nothing here depends on the application. The registry, the crate and the installation root are
//! fixed or derived from the running executable; no caller supplies a URL, crate, registry, target
//! directory or command. Building only ever writes under a staging directory the caller chooses,
//! so the executables that are installed (and running) are never touched.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

pub const CRATE: &str = "personal-teams-assistant";
pub const CLI: &str = "pta";
/// The sparse index Cargo itself reads for crates.io.
pub const INDEX: &str = "https://index.crates.io";
const MAX_INDEX: usize = 4 << 20;
const MAX_RECORDS: u64 = 4 << 20;
/// What Cargo records for a crates.io package, with and without the sparse protocol.
const SOURCES: [&str; 2] = [
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
];
/// The only Cargo features this package has; anything else is a custom build.
const FEATURES: [&str; 1] = ["gui"];
pub const BUILD_SECONDS: u64 = 60 * 60;
const PROBE_SECONDS: u64 = 20;
pub const TAIL_LINES: usize = 20;
const TAIL_WIDTH: usize = 300;

// ---------------------------------------------------------------------------------------------
// Versions and the registry.

/// A stable `MAJOR.MINOR.PATCH` release. Ordering is numeric per part, so 0.10.0 > 0.9.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Strict: three numeric parts, nothing else. Pre-releases and build metadata are not stable
    /// releases and do not parse.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut part = || {
            let p = parts.next()?;
            (!p.is_empty()
                && p.len() <= 18
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p == "0" || !p.starts_with('0')))
            .then(|| p.parse::<u64>().ok())
            .flatten()
        };
        let version = Self(part()?, part()?, part()?);
        parts.next().is_none().then_some(version)
    }

    /// A `rust-version`: `1.95` or `1.95.0`.
    fn parse_rust(text: &str) -> Option<Self> {
        Self::parse(text).or_else(|| Self::parse(&format!("{text}.0")))
    }
}
impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A published release as the registry describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub rust_version: Option<Version>,
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    vers: String,
    #[serde(default)]
    yanked: bool,
    #[serde(default)]
    rust_version: Option<String>,
}

/// The path of a crate in a sparse index.
pub fn index_path(name: &str) -> String {
    let name = name.to_ascii_lowercase();
    match name.len() {
        1 | 2 => format!("{}/{name}", name.len()),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    }
}

/// The newest stable, not yanked release in a sparse-index answer. Lines about another crate,
/// malformed lines, pre-releases and yanked versions are ignored; an answer that does not
/// describe this crate at all is an error rather than "nothing newer".
pub fn newest_stable(body: &str) -> Result<Option<Release>> {
    let mut recognized = 0usize;
    let mut best: Option<Release> = None;
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(entry) = serde_json::from_str::<Entry>(line) else {
            continue;
        };
        if entry.name != CRATE {
            continue;
        }
        recognized += 1;
        if entry.yanked {
            continue;
        }
        let Some(version) = Version::parse(&entry.vers) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| version > b.version) {
            best = Some(Release {
                version,
                rust_version: entry.rust_version.as_deref().and_then(Version::parse_rust),
            });
        }
    }
    ensure!(
        recognized > 0,
        "The registry answer does not describe {CRATE}."
    );
    Ok(best)
}

/// Reads the crate's index entry: bounded, read-only, no redirects, no credentials.
pub async fn fetch_index(base: &str) -> Result<String> {
    let url = format!("{}/{}", base.trim_end_matches('/'), index_path(CRATE));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .user_agent(format!(
            "{CRATE}/{} (update check)",
            env!("CARGO_PKG_VERSION")
        ))
        .build()?;
    let mut response = client
        .get(url)
        .send()
        .await
        .context("The package registry could not be reached.")?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "The package registry answered with status {}.",
        response.status().as_u16()
    );
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("The package registry answer was interrupted.")?
    {
        body.extend_from_slice(&chunk);
        ensure!(
            body.len() <= MAX_INDEX,
            "The package registry answer is too large."
        );
    }
    String::from_utf8(body).context("The package registry answer is not text.")
}

// ---------------------------------------------------------------------------------------------
// This installation.

/// How this executable was installed, as Cargo recorded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installation {
    pub root: PathBuf,
    pub version: Version,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    /// Present only when the install used a target other than the compiler's own.
    pub target: Option<String>,
}

#[derive(Deserialize)]
struct Records {
    #[serde(default)]
    installs: BTreeMap<String, Record>,
}
#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    bins: Vec<String>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    all_features: bool,
    #[serde(default)]
    no_default_features: bool,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    rustc: Option<String>,
}

fn bin_name(name: &str) -> &str {
    name.strip_suffix(".exe").unwrap_or(name)
}

fn read_records(root: &Path) -> std::result::Result<Records, String> {
    let path = root.join(".crates2.json");
    let file = std::fs::File::open(&path).map_err(|_| {
        "This executable has no Cargo install record, so it was not installed with `cargo install`. Update it the way you installed it.".to_owned()
    })?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::Read::take(file, MAX_RECORDS), &mut text)
        .map_err(|_| "Cargo's install record cannot be read.".to_owned())?;
    serde_json::from_str(&text).map_err(|_| "Cargo's install record is not valid.".to_owned())
}

/// The one entry for this crate: its version, source and build choices.
fn own_record(records: Records) -> std::result::Result<(Version, Record), String> {
    let mut found = records.installs.into_iter().filter_map(|(key, record)| {
        let (name, rest) = key.split_once(' ')?;
        (name == CRATE).then_some(())?;
        let (version, source) = rest.split_once(' ')?;
        let source = source.strip_prefix('(')?.strip_suffix(')')?;
        Some((version.to_owned(), source.to_owned(), record))
    });
    let (version, source, record) = found
        .next()
        .ok_or("Cargo has no install record for this package in that location.")?;
    if found.next().is_some() {
        return Err("Cargo has several install records for this package.".into());
    }
    if !SOURCES.contains(&source.as_str()) {
        return Err(
            "This installation does not come from crates.io (a path, git or custom registry). Update it the way you installed it."
                .into(),
        );
    }
    let version =
        Version::parse(&version).ok_or("The installed version is not a stable release number.")?;
    Ok((version, record))
}

/// The installation `exe` belongs to, or why it cannot be updated from here. A diagnostic is
/// shown to the user, so it says what to do.
pub fn inspect(exe: &Path) -> std::result::Result<Installation, String> {
    let bin = exe
        .parent()
        .filter(|p| p.file_name().is_some_and(|n| n == "bin"))
        .ok_or("This executable is not in a Cargo `bin` directory (for example a development build), so it is not updated from here.")?;
    let root = bin
        .parent()
        .ok_or("This executable is not in a Cargo installation root.")?
        .to_path_buf();
    let (version, record) = own_record(read_records(&root)?)?;
    let bins: BTreeSet<&str> = record.bins.iter().map(|b| bin_name(b)).collect();
    if !(bins.contains(CRATE) && bins.contains(CLI)) {
        return Err(
            "The installed package does not contain both the application and the pta command."
                .into(),
        );
    }
    if record.profile.as_deref().is_some_and(|p| p != "release") {
        return Err("This installation was not built in release mode.".into());
    }
    if let Some(unknown) = record
        .features
        .iter()
        .find(|f| !FEATURES.contains(&f.as_str()))
    {
        return Err(format!(
            "This installation uses the custom feature {unknown:?}; it is not updated from here."
        ));
    }
    let host = record.rustc.as_deref().and_then(|text| {
        text.lines()
            .find_map(|l| l.strip_prefix("host: "))
            .map(str::trim)
    });
    let target = record
        .target
        .filter(|t| Some(t.as_str()) != host)
        .filter(|t| !t.is_empty());
    if target.as_deref().is_some_and(|t| {
        !t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    }) {
        return Err("The recorded build target is not valid.".into());
    }
    let mut features = record.features;
    features.sort();
    features.dedup();
    Ok(Installation {
        root,
        version,
        features,
        all_features: record.all_features,
        no_default_features: record.no_default_features,
        target,
    })
}

// ---------------------------------------------------------------------------------------------
// Building.

/// Environment variables the build keeps. Everything else, in particular any account variable,
/// credential, registry token or application setting, is dropped.
const KEEP: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "TMPDIR",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMDATA",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "LANG",
    "LC_ALL",
    // Compilers and native libraries the build finds through the environment.
    "CC",
    "CXX",
    "AR",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_LIBDIR",
    "PKG_CONFIG_SYSROOT_DIR",
    "DEVELOPER_DIR",
    "SDKROOT",
    "MACOSX_DEPLOYMENT_TARGET",
    "VCINSTALLDIR",
    "VSINSTALLDIR",
    "WINDOWSSDKDIR",
    "UNIVERSALCRTSDKDIR",
    "INCLUDE",
    "LIB",
    "LIBPATH",
    // The user's own network configuration, as for any Cargo download.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "CARGO_HTTP_PROXY",
    "CARGO_HTTP_CAINFO",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// The build's whole environment: the allowlisted variables of `source` plus fixed quiet,
/// non-interactive settings.
pub fn build_environment(
    source: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let mut kept: Vec<(OsString, OsString)> = source
        .into_iter()
        .filter(|(name, _)| {
            name.to_str().is_some_and(|name| {
                KEEP.iter().any(|keep| {
                    if cfg!(windows) {
                        keep.eq_ignore_ascii_case(name)
                    } else {
                        *keep == name
                    }
                })
            })
        })
        .collect();
    for (name, value) in [
        ("CARGO_TERM_COLOR", "never"),
        ("CARGO_TERM_PROGRESS_WHEN", "never"),
        ("CARGO_INCREMENTAL", "0"),
    ] {
        kept.push((name.into(), value.into()));
    }
    kept
}

/// The `cargo install` arguments that rebuild `version` the way `installation` was built.
pub fn install_args(installation: &Installation, version: &Version, root: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["install", CRATE, "--version"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(version.to_string().into());
    args.extend(["--locked", "--root"].map(OsString::from));
    args.push(root.as_os_str().to_owned());
    args.extend(["--color", "never"].map(OsString::from));
    if installation.no_default_features {
        args.push("--no-default-features".into());
    }
    if installation.all_features {
        args.push("--all-features".into());
    } else if !installation.features.is_empty() {
        args.extend(["--features".into(), installation.features.join(",").into()]);
    }
    if let Some(target) = &installation.target {
        args.extend(["--target".into(), target.into()]);
    }
    args
}

/// The first `name` (or `name.exe`) in `path`.
pub fn find_program(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    let candidates = [name.to_owned(), format!("{name}.exe")];
    std::env::split_paths(path?)
        .flat_map(|dir| candidates.iter().map(move |c| dir.join(c)))
        .find(|p| p.is_file())
}

fn command(program: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
}

/// Runs a short probe with the build environment and returns its standard output.
async fn probe(program: &Path, args: &[&str], env: &[(OsString, OsString)]) -> Result<String> {
    let mut child = command(program)
        .args(args)
        .env_clear()
        .envs(env.iter().cloned())
        .stdout(Stdio::piped())
        .spawn()
        .context("The program could not be started.")?;
    let mut stdout = child.stdout.take().context("no output")?;
    let run = async {
        let mut text = String::new();
        (&mut stdout).take(8192).read_to_string(&mut text).await?;
        let status = child.wait().await?;
        ensure!(status.success(), "The program reported a failure.");
        Ok(text)
    };
    tokio::time::timeout(Duration::from_secs(PROBE_SECONDS), run)
        .await
        .context("The program did not answer in time.")?
}

/// The Cargo version, or an actionable reason it cannot build.
pub async fn cargo_version(
    cargo: &Path,
    env: &[(OsString, OsString)],
) -> std::result::Result<Version, String> {
    let text = probe(cargo, &["--version"], env).await.map_err(|_| {
        "Cargo could not run. Install the Rust toolchain from https://rustup.rs (or run `rustup default stable`) and try again.".to_owned()
    })?;
    text.split_whitespace()
        .nth(1)
        .and_then(Version::parse)
        .ok_or_else(|| "Cargo's version could not be read.".to_owned())
}

/// A hint for a failure a user can fix, from the end of a build's output.
pub fn hint(tail: &[String]) -> Option<&'static str> {
    let text = tail.join("\n");
    [
        ("linker `cc` not found", "Install a C compiler and linker (for example `build-essential` on Debian and Ubuntu, or Xcode's command line tools on macOS) and try again."),
        ("linker `link.exe` not found", "Install the Visual Studio Build Tools with the C++ workload and try again."),
        ("pkg-config", "Building the desktop app needs the GTK and WebKit development packages (see the README); or reinstall without the desktop app using `--no-default-features`."),
        ("No space left on device", "There is not enough free disk space to build. Free some and try again."),
        ("failed to download", "Cargo could not download the package. Check the network connection and proxy settings and try again."),
        ("spurious network error", "Cargo could not reach the registry. Check the network connection and proxy settings and try again."),
        ("is not supported by the version of the rust compiler", "The Rust toolchain is too old for this release. Run `rustup update` and try again."),
    ]
    .into_iter()
    .find(|(needle, _)| text.contains(needle))
    .map(|(_, advice)| advice)
}

/// How far a build has got.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub crates: u64,
    pub current: Option<String>,
}
impl Progress {
    fn line(&mut self, line: &str) {
        if let Some(rest) = line.trim_start().strip_prefix("Compiling ") {
            self.crates += 1;
            self.current = rest.split_whitespace().next().map(str::to_owned);
        }
    }
}

fn clean_line(line: &str) -> String {
    let mut text: String = line.chars().filter(|c| !c.is_control()).collect();
    if let Ok(redactor) = crate::security::Redactor::new(&[], Vec::new()) {
        text = redactor.redact(&text);
    }
    text.chars().take(TAIL_WIDTH).collect()
}

/// A failed build: what Cargo said last (bounded and redacted) and what the user can do.
#[derive(Debug)]
pub struct BuildFailure {
    pub tail: Vec<String>,
    pub reason: String,
}
impl std::fmt::Display for BuildFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}
impl std::error::Error for BuildFailure {}

/// Runs `cargo`, keeping the old executables untouched: it writes only where `args` say.
/// `report` sees the progress after each output line.
pub async fn build(
    cargo: &Path,
    args: &[OsString],
    env: &[(OsString, OsString)],
    cwd: &Path,
    timeout: Duration,
    mut report: impl FnMut(&Progress),
) -> std::result::Result<(), BuildFailure> {
    let fail = |reason: &str, tail: Vec<String>| BuildFailure {
        reason: reason.to_owned(),
        tail,
    };
    let mut child = command(cargo)
        .args(args)
        .env_clear()
        .envs(env.iter().cloned())
        .current_dir(cwd)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| fail("Cargo could not be started.", Vec::new()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| fail("Cargo's output could not be read.", Vec::new()))?;
    let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let run = async {
        let mut progress = Progress::default();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            progress.line(&line);
            report(&progress);
            let line = clean_line(&line);
            if !line.trim().is_empty() {
                if tail.len() == TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        }
        child.wait().await
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(_)) => {
            let tail: Vec<String> = tail.into();
            let reason = hint(&tail).map_or_else(
                || "Cargo could not build this release.".to_owned(),
                |advice| format!("Cargo could not build this release. {advice}"),
            );
            Err(BuildFailure { tail, reason })
        }
        Ok(Err(_)) => Err(fail("Cargo stopped unexpectedly.", tail.into())),
        Err(_) => Err(fail(
            "The build took too long and was stopped.",
            tail.into(),
        )),
    }
}

// ---------------------------------------------------------------------------------------------
// Verifying a staged pair.

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Digest {
    pub sha256: String,
    pub size: u64,
}

/// What was staged: enough to recognize the pair again and to install exactly it later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    pub staged_at: i64,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    pub target: Option<String>,
    pub binaries: BTreeMap<String, Digest>,
}

fn exe_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn digest(path: &Path) -> Result<Digest> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() > 0,
        "A staged executable is missing or empty."
    );
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok(Digest {
        sha256: format!("{:x}", hash.finalize()),
        size: metadata.len(),
    })
}

/// Checks a staging root as Cargo left it: its own install record names exactly this version,
/// source, features and target, both executables exist and report this version. Returns the
/// manifest to store. Executes only the two staged programs, with the build environment, and only
/// to read their version.
pub async fn verify(
    root: &Path,
    version: &Version,
    wanted: &Installation,
    env: &[(OsString, OsString)],
    now: i64,
) -> Result<Manifest> {
    let (recorded, record) =
        own_record(read_records(root).map_err(anyhow::Error::msg)?).map_err(anyhow::Error::msg)?;
    ensure!(
        recorded == *version,
        "The staged package is not version {version}."
    );
    let bins: BTreeSet<&str> = record.bins.iter().map(|b| bin_name(b)).collect();
    ensure!(
        bins == BTreeSet::from([CRATE, CLI]),
        "The staged package is not the matched application and pta pair."
    );
    let mut features = record.features.clone();
    features.sort();
    features.dedup();
    ensure!(
        features == wanted.features
            && record.all_features == wanted.all_features
            && record.no_default_features == wanted.no_default_features,
        "The staged build does not use the installed feature choice."
    );
    let mut binaries = BTreeMap::new();
    for name in [CRATE, CLI] {
        let path = root.join("bin").join(exe_name(name));
        binaries.insert(name.to_owned(), digest(&path)?);
    }
    let cli = root.join("bin").join(exe_name(CLI));
    let reported = probe(&cli, &["--version", "--json"], env).await?;
    let reported: serde_json::Value = serde_json::from_str(reported.trim())
        .context("The staged pta does not report its version.")?;
    ensure!(
        reported["data"]["cli_version"] == version.to_string(),
        "The staged pta reports a different version."
    );
    let host = root.join("bin").join(exe_name(CRATE));
    let reported = probe(&host, &["--version"], env).await?;
    ensure!(
        reported.trim() == format!("{CRATE} {version}"),
        "The staged application reports a different version."
    );
    Ok(Manifest {
        schema: 1,
        version: version.to_string(),
        staged_at: now,
        features,
        all_features: wanted.all_features,
        no_default_features: wanted.no_default_features,
        target: wanted.target.clone(),
        binaries,
    })
}

/// A cheap check that the staged files are still there with the recorded sizes.
pub fn present(root: &Path, manifest: &Manifest) -> bool {
    manifest.binaries.iter().all(|(name, expected)| {
        std::fs::symlink_metadata(root.join("bin").join(exe_name(name)))
            .is_ok_and(|m| m.is_file() && m.len() == expected.size)
    })
}

/// Whether the files under `root` still match what was verified, byte for byte.
pub fn unchanged(root: &Path, manifest: &Manifest) -> bool {
    manifest.binaries.iter().all(|(name, expected)| {
        digest(&root.join("bin").join(exe_name(name))).is_ok_and(|d| &d == expected)
    })
}

#[cfg(test)]
mod tests;
