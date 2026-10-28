//! Applying a staged, verified pair: the durable journal and lock, the file replacement with its
//! undo, the data safety copies, the busy checks and the recovery of an interrupted update.
//!
//! Nothing here starts or stops a process. Replacement never overwrites an executable in place:
//! the live file is renamed aside (which Windows allows for a running program) and the verified
//! copy is renamed into its name, so every step has an inverse and the journal always says which
//! one to take. Files are named by the journal's ID, so a leftover of an earlier attempt can never
//! be mistaken for this one. Every observable point calls the `hook`, which production leaves empty
//! and tests use to inject failures at exactly that point.
use super::{CLI, CRATE, Digest, Manifest, digest, exe_name};
use anyhow::{Context, Result, anyhow, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

pub const JOURNAL: &str = "journal.json";
const LOCK: &str = "apply.lock";
const BACKUPS: &str = "backup";
const SCHEMA: u32 = 1;
/// How long an unfinished journal whose lock is free may still be a hand-over between the host and
/// its helper rather than an interrupted update.
pub const GRACE_SECONDS: i64 = 60;
const BINARIES: [&str; 2] = [CRATE, CLI];
const RECORDS: [&str; 2] = [".crates.toml", ".crates2.json"];
const RETRIES: u32 = 20;
const SMALL_FILE: u64 = 1 << 20;
const MARGIN: u64 = 64 << 20;

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

// ---------------------------------------------------------------------------------------------
// The journal.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Written by the host; the helper has not taken over yet.
    Requested,
    /// The helper waits for the previous host to stop.
    Stopping,
    BackingUp,
    Replacing,
    Restarting,
    Verifying,
    /// The new pair runs and was found healthy. The only success.
    Committed,
    RollingBack,
    /// The previous pair is back and verified.
    RolledBack,
    /// Neither the update nor the rollback could be completed: a person has to look.
    Failed,
}

impl Phase {
    pub fn finished(self) -> bool {
        matches!(self, Self::Committed | Self::RolledBack | Self::Failed)
    }
}

/// What the previous host was doing, so the new one can be left in the same state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    /// Flags the previous host was started with (`--headless`, `--host`); none is the window.
    pub launch: Vec<String>,
    pub assistant: bool,
    pub subscriptions: u64,
    pub tunnel: bool,
}

/// What a running host reports about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health {
    pub version: String,
    pub assistant: bool,
    pub subscriptions: u64,
    pub tunnel: bool,
}

/// A profile whose files a restart touches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub dir: PathBuf,
    pub data: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub from: PathBuf,
    /// Name inside the backup directory.
    pub file: String,
    pub sqlite: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Journal {
    pub schema: u32,
    pub id: String,
    pub from: String,
    pub to: String,
    pub phase: Phase,
    pub started_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
    /// The Cargo installation root (its `bin` holds the executables).
    pub root: PathBuf,
    /// The staged pair's root.
    pub staged: PathBuf,
    pub manifest: Manifest,
    pub intent: Intent,
    pub profiles: Vec<Profile>,
    /// Digests of the executables that were installed when the update began.
    #[serde(default)]
    pub previous: BTreeMap<String, Digest>,
    #[serde(default)]
    pub saved: Vec<Saved>,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub data_restored: bool,
    /// Follow-up for the person once the new version runs (skills, MCP clients).
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Journal {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &str,
        from: &str,
        manifest: Manifest,
        root: &Path,
        staged: &Path,
        intent: Intent,
        profiles: Vec<Profile>,
    ) -> Self {
        let at = now();
        Self {
            schema: SCHEMA,
            id: id.to_owned(),
            from: from.to_owned(),
            to: manifest.version.clone(),
            phase: Phase::Requested,
            started_at: at,
            updated_at: at,
            finished_at: None,
            root: root.to_owned(),
            staged: staged.to_owned(),
            manifest,
            intent,
            profiles,
            previous: BTreeMap::new(),
            saved: Vec::new(),
            steps: Vec::new(),
            error: None,
            data_restored: false,
            notes: Vec::new(),
        }
    }
    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }
}

pub fn read(dir: &Path) -> Option<Journal> {
    let text = fs::read_to_string(dir.join(JOURNAL)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Atomic and private: a reader sees the old journal or the new one, never a torn file.
pub fn write(dir: &Path, journal: &Journal) -> Result<()> {
    crate::security::private_dir(dir)?;
    let temp = dir.join(format!(".{JOURNAL}.{}", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut file = options.open(&temp)?;
        crate::security::protect_file(&temp)?;
        file.write_all(serde_json::to_string(journal)?.as_bytes())?;
        file.sync_all()?;
    }
    crate::security::replace_private_file(&temp, &dir.join(JOURNAL))
}

// ---------------------------------------------------------------------------------------------
// The lock.

/// Held by whoever drives an update, for as long as it does. A process that dies releases it.
pub struct Lock(#[allow(dead_code)] fs::File);

impl Lock {
    pub fn try_acquire(dir: &Path) -> Result<Option<Self>> {
        crate::security::private_dir(dir)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(LOCK))?;
        Ok(file.try_lock_exclusive().ok().map(|()| Self(file)))
    }

    pub fn wait(dir: &Path, timeout: Duration) -> Result<Self> {
        let started = std::time::Instant::now();
        loop {
            if let Some(lock) = Self::try_acquire(dir)? {
                return Ok(lock);
            }
            ensure!(
                started.elapsed() < timeout,
                "Another update is being applied."
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Work in flight.

fn sqlite_count(path: &Path, query: &str) -> std::result::Result<bool, rusqlite::Error> {
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    db.busy_timeout(Duration::from_secs(10))?;
    match db.query_row(query, [], |row| row.get::<_, i64>(0)) {
        Ok(count) => Ok(count > 0),
        // A database that never needed the table has no such work.
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("no such table") =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// Work a restart would interrupt, in every profile given. An unreadable record counts as work:
/// the update waits rather than guess. Reasons are fixed texts without paths or content.
pub fn busy(profiles: &[Profile]) -> Vec<String> {
    let mut reasons: Vec<String> = Vec::new();
    let mut add = |reason: &str| {
        if !reasons.iter().any(|r| r == reason) {
            reasons.push(reason.to_owned());
        }
    };
    for profile in profiles {
        let registry = profile.dir.join("implementation").join("registry.json");
        if registry.is_file() {
            match fs::read_to_string(&registry)
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            {
                Some(value) => {
                    let state = |v: &serde_json::Value| v["state"].as_str().map(str::to_owned);
                    let runs = value["runs"].as_array().is_some_and(|runs| {
                        runs.iter()
                            .any(|r| matches!(state(r).as_deref(), Some("planning" | "running")))
                    });
                    let sends = value["creations"].as_object().is_some_and(|all| {
                        all.values().any(|r| state(r).as_deref() == Some("sending"))
                    });
                    if runs {
                        add("A work item implementation is in progress.");
                    }
                    if sends {
                        add("Work items are being created.");
                    }
                }
                None => add("Implementation state could not be read."),
            }
        }
        let runs = profile.dir.join("implementation").join("runs.db");
        if runs.is_file() {
            let checks = [
                (
                    "A coding agent is running.",
                    "SELECT count(*) FROM agent_runs WHERE state IN ('planning','running')",
                ),
                (
                    "A request agent or creation is running.",
                    "SELECT count(*) FROM request_runs WHERE state IN ('generating','sending')",
                ),
            ];
            for (reason, query) in checks {
                match sqlite_count(&runs, query) {
                    Ok(true) => add(reason),
                    Ok(false) => {}
                    Err(_) => add("Agent execution state could not be read."),
                }
            }
        }
        let checks: [(&str, &str, &str); 3] = [
            (
                "activity-registration.db",
                "An activity review is running.",
                "SELECT count(*) FROM runs WHERE status='running'",
            ),
            (
                "activity-registration.db",
                "An activity task is being written.",
                "SELECT count(*) FROM entries WHERE state IN ('sending','refining')",
            ),
            (
                "assistant.db",
                "A Teams message is being answered or sent.",
                "SELECT count(*) FROM jobs WHERE status IN ('processing','sending','refining')",
            ),
        ];
        for (name, reason, query) in checks {
            let path = profile.data.join(name);
            if !path.is_file() {
                continue;
            }
            match sqlite_count(&path, query) {
                Ok(true) => add(reason),
                Ok(false) => {}
                Err(_) => add("Assistant state could not be read."),
            }
        }
    }
    reasons
}

// ---------------------------------------------------------------------------------------------
// Safety copies.

/// Directories that never hold the application's own databases: the update's own files, what an
/// agent works in, and the isolated profiles of a portal (listed as profiles of their own).
const SKIPPED: [&str; 6] = [
    "maintenance",
    "skills",
    "web",
    "home",
    "repositories",
    "target",
];

fn databases(dir: &Path, depth: u32) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = fs::symlink_metadata(&path).map(|m| m.file_type()) else {
            continue;
        };
        if kind.is_file() && path.extension().is_some_and(|e| e == "db") {
            found.push(path);
        } else if kind.is_dir()
            && depth > 0
            && !entry
                .file_name()
                .to_str()
                .is_some_and(|name| SKIPPED.contains(&name))
        {
            found.extend(databases(&path, depth - 1));
        }
    }
    found.sort();
    found
}

fn small_files(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == extension)
                && fs::symlink_metadata(p).is_ok_and(|m| m.is_file() && m.len() <= SMALL_FILE)
        })
        .collect();
    found.sort();
    found
}

/// What to copy before a restart: every database a version could migrate, and the settings files.
pub fn protected_files(profiles: &[Profile]) -> Vec<(PathBuf, bool)> {
    let mut found: Vec<(PathBuf, bool)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut take = |path: PathBuf, sqlite: bool| {
        let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            found.push((path, sqlite));
        }
    };
    for profile in profiles {
        let implementation = profile.dir.join("implementation");
        // The data directory holds its databases directly; the profile keeps some one level down
        // (`implementation/`, `variables/`).
        for (dir, depth) in [(&profile.data, 0), (&profile.dir, 1)] {
            for path in databases(dir, depth) {
                take(path, true);
            }
        }
        for path in small_files(&profile.dir, "toml") {
            take(path, false);
        }
        let registry = implementation.join("registry.json");
        if registry.is_file() {
            take(registry, false);
        }
    }
    found
}

fn size_with_sidecars(path: &Path) -> u64 {
    ["", "-wal"]
        .iter()
        .map(|suffix| {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            fs::metadata(PathBuf::from(name)).map_or(0, |m| m.len())
        })
        .sum()
}

/// Refuses, before anything is stopped, an update that cannot keep a way back: not enough room for
/// the safety copy of the data and for the new executables next to the old ones.
pub fn preflight(dir: &Path, root: &Path, manifest: &Manifest, profiles: &[Profile]) -> Result<()> {
    let data: u64 = protected_files(profiles)
        .iter()
        .map(|(path, _)| size_with_sidecars(path))
        .sum();
    crate::security::private_dir(dir)?;
    let free = fs2::available_space(dir)
        .context("The free space for the safety copy of your data could not be read.")?;
    ensure!(
        free >= data.saturating_mul(2).saturating_add(MARGIN),
        "There is not enough free disk space to keep a safety copy of your data before updating. Free some space and try again."
    );
    let binaries: u64 = manifest.binaries.values().map(|d| d.size).sum();
    let free = fs2::available_space(root.join("bin"))
        .context("The free space of the installation could not be read.")?;
    ensure!(
        free >= binaries.saturating_mul(2).saturating_add(MARGIN),
        "There is not enough free disk space in the installation folder to replace the executables. Free some space and try again."
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Cargo's install records.

/// Replaces this package's entry in the installation's records with the staged one, so Cargo later
/// sees the new version as installed. Other packages' entries are untouched.
fn merge_records(root: &Path, staged: &Path, id: &str) -> Result<()> {
    let json = ".crates2.json";
    if let (Ok(live), Ok(new)) = (
        fs::read_to_string(root.join(json)),
        fs::read_to_string(staged.join(json)),
    ) {
        let mut live: serde_json::Value = serde_json::from_str(&live)?;
        let new: serde_json::Value = serde_json::from_str(&new)?;
        let ours = |key: &str| key.split(' ').next() == Some(CRATE);
        let installs = live["installs"]
            .as_object_mut()
            .context("Cargo's install record is not valid.")?;
        installs.retain(|key, _| !ours(key));
        for (key, value) in new["installs"].as_object().into_iter().flatten() {
            if ours(key) {
                installs.insert(key.clone(), value.clone());
            }
        }
        atomic(root, json, id, &serde_json::to_string(&live)?)?;
    }
    let toml_name = ".crates.toml";
    if let (Ok(live), Ok(new)) = (
        fs::read_to_string(root.join(toml_name)),
        fs::read_to_string(staged.join(toml_name)),
    ) {
        let mut live: toml::Table = live.parse()?;
        let new: toml::Table = new.parse()?;
        let ours = |key: &str| key.split(' ').next() == Some(CRATE);
        let table = live
            .entry("v1")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .context("Cargo's install record is not valid.")?;
        table.retain(|key, _| !ours(key));
        if let Some(added) = new.get("v1").and_then(toml::Value::as_table) {
            for (key, value) in added {
                if ours(key) {
                    table.insert(key.clone(), value.clone());
                }
            }
        }
        atomic(root, toml_name, id, &toml::to_string(&live)?)?;
    }
    Ok(())
}

fn atomic(dir: &Path, name: &str, id: &str, content: &str) -> Result<()> {
    let temp = dir.join(format!(".{name}.{id}.new"));
    fs::write(&temp, content)?;
    crate::security::replace_private_file(&temp, &dir.join(name))
}

// ---------------------------------------------------------------------------------------------
// Replacement and undo.

type Hook = Box<dyn Fn(&str) -> io::Result<()> + Send + Sync>;

pub struct Apply {
    pub dir: PathBuf,
    pub journal: Journal,
    hook: Hook,
    pause: Duration,
}

fn retry<T>(pause: Duration, mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut last = None;
    for n in 0..RETRIES {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(error),
            Err(error) => {
                last = Some(error);
                if n + 1 < RETRIES {
                    std::thread::sleep(pause);
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no attempt was made")))
}

impl Apply {
    pub fn new(dir: &Path, journal: Journal) -> Self {
        Self {
            dir: dir.to_owned(),
            journal,
            hook: Box::new(|_| Ok(())),
            // A running executable can be renamed on Windows but a scanner may hold it briefly.
            pause: Duration::from_millis(250),
        }
    }

    /// Fault injection: called at every named point before its work.
    pub fn with_hook(
        mut self,
        hook: impl Fn(&str) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.hook = Box::new(hook);
        self
    }

    pub fn without_pause(mut self) -> Self {
        self.pause = Duration::ZERO;
        self
    }

    fn at(&self, point: &str) -> Result<()> {
        (self.hook)(point).map_err(|error| anyhow!("{point}: {error}"))
    }

    /// Moves to `phase` and persists the journal.
    pub fn phase(&mut self, phase: Phase) -> Result<()> {
        self.journal.phase = phase;
        self.save()
    }

    pub fn save(&mut self) -> Result<()> {
        self.journal.updated_at = now();
        if self.journal.phase.finished() && self.journal.finished_at.is_none() {
            self.journal.finished_at = Some(self.journal.updated_at);
        }
        write(&self.dir, &self.journal)
    }

    fn step(&mut self, name: &str) -> Result<()> {
        self.journal.steps.push(name.to_owned());
        self.save()
    }

    fn path(&self, name: &str, kind: &str) -> PathBuf {
        self.journal
            .bin()
            .join(format!(".{}.{}.{kind}", exe_name(name), self.journal.id))
    }

    fn backup_dir(&self) -> PathBuf {
        self.dir.join(BACKUPS).join(&self.journal.id)
    }

    /// Records what is installed and copies what the new version could change: Cargo's records
    /// and the data. Nothing here modifies anything that is in use.
    pub fn back_up(&mut self) -> Result<()> {
        self.phase(Phase::BackingUp)?;
        self.at("backup")?;
        let base = self.backup_dir();
        // Only the latest safety copy is kept.
        if let Ok(entries) = fs::read_dir(self.dir.join(BACKUPS)) {
            for entry in entries.flatten() {
                if entry.file_name().to_str() != Some(self.journal.id.as_str()) {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
        crate::security::private_dir(&base)?;
        for name in BINARIES {
            let live = self.journal.bin().join(exe_name(name));
            self.journal.previous.insert(
                name.to_owned(),
                digest(&live).context("The installed executable cannot be read.")?,
            );
        }
        let records = base.join("records");
        crate::security::private_dir(&records)?;
        for name in RECORDS {
            let source = self.journal.root.join(name);
            if source.is_file() {
                fs::copy(&source, records.join(name))?;
            }
        }
        let files = protected_files(&self.journal.profiles);
        self.journal.saved.clear();
        for (n, (from, sqlite)) in files.into_iter().enumerate() {
            self.at("backup:data")?;
            let file = format!("{n:04}");
            let target = base.join(&file);
            if sqlite {
                let db = rusqlite::Connection::open(&from)?;
                db.busy_timeout(Duration::from_secs(10))?;
                let text = target.to_str().context("The backup path is not text.")?;
                db.execute("VACUUM INTO ?1", [text])?;
            } else {
                fs::copy(&from, &target)?;
            }
            crate::security::protect_file(&target)?;
            self.journal.saved.push(Saved { from, file, sqlite });
        }
        self.step("backup")
    }

    /// Puts the verified pair in place: for each executable, a checked copy next to it, the live
    /// file renamed aside, the copy renamed into its name; then Cargo's records.
    pub fn replace(&mut self) -> Result<()> {
        self.phase(Phase::Replacing)?;
        for name in BINARIES {
            let expected = self
                .journal
                .manifest
                .binaries
                .get(name)
                .cloned()
                .context("The staged pair is incomplete.")?;
            let source = self.journal.staged.join("bin").join(exe_name(name));
            let live = self.journal.bin().join(exe_name(name));
            let new = self.path(name, "new");
            let previous = self.path(name, "previous");
            self.at(&format!("copy:{name}"))?;
            fs::copy(&source, &new).context("The staged executable could not be copied.")?;
            ensure!(
                digest(&new)? == expected,
                "A staged executable changed after it was verified."
            );
            self.step(&format!("{name}:copied"))?;
            self.at(&format!("retire:{name}"))?;
            retry(self.pause, || fs::rename(&live, &previous))
                .context("The installed executable is in use and cannot be replaced.")?;
            self.step(&format!("{name}:retired"))?;
            self.at(&format!("install:{name}"))?;
            retry(self.pause, || fs::rename(&new, &live))
                .context("The new executable could not be put in place.")?;
            self.step(&format!("{name}:installed"))?;
        }
        self.at("records")?;
        merge_records(&self.journal.root, &self.journal.staged, &self.journal.id)?;
        self.step("records")
    }

    /// Whether the files in place are exactly the staged pair.
    pub fn installed_is_new(&self) -> bool {
        super::unchanged(&self.journal.root, &self.journal.manifest)
    }

    /// Whether the files in place are exactly the pair that was installed before.
    pub fn installed_is_previous(&self) -> bool {
        !self.journal.previous.is_empty()
            && self.journal.previous.iter().all(|(name, expected)| {
                digest(&self.journal.bin().join(exe_name(name))).is_ok_and(|d| &d == expected)
            })
    }

    fn undo(&self, name: &str) -> Result<()> {
        let live = self.journal.bin().join(exe_name(name));
        let previous = self.path(name, "previous");
        let failed = self.path(name, "failed");
        if previous.exists() {
            if live.exists() {
                let _ = fs::remove_file(&failed);
                retry(self.pause, || fs::rename(&live, &failed))
                    .context("The new executable is in use and cannot be removed.")?;
            }
            retry(self.pause, || fs::rename(&previous, &live))
                .context("The previous executable could not be restored.")?;
        }
        let _ = fs::remove_file(self.path(name, "new"));
        Ok(())
    }

    /// Restores both previous executables and Cargo's records, and checks they are the pair that
    /// was installed. Safe to run again after any interruption, in any state.
    pub fn roll_back(&mut self) -> Result<()> {
        self.phase(Phase::RollingBack)?;
        self.at("rollback")?;
        let mut first = None;
        for name in BINARIES {
            if let Err(error) = self
                .at(&format!("undo:{name}"))
                .and_then(|()| self.undo(name))
            {
                first.get_or_insert(error);
            }
        }
        if let Some(error) = first {
            return Err(error);
        }
        let records = self.backup_dir().join("records");
        for name in RECORDS {
            let saved = records.join(name);
            if saved.is_file() {
                let temp = self
                    .journal
                    .root
                    .join(format!(".{name}.{}.restore", self.journal.id));
                fs::copy(&saved, &temp)?;
                crate::security::replace_private_file(&temp, &self.journal.root.join(name))?;
            }
        }
        for (name, expected) in &self.journal.previous {
            ensure!(
                digest(&self.journal.bin().join(exe_name(name))).is_ok_and(|d| &d == expected),
                "The restored executable is not the one that was installed before."
            );
        }
        Ok(())
    }

    /// Puts the data copies back over the live files, for a previous version that cannot read what
    /// the new one wrote. Work done since the update began is lost, which is why it is a last resort.
    pub fn restore_data(&mut self) -> Result<()> {
        self.at("restore:data")?;
        let base = self.backup_dir();
        for saved in self.journal.saved.clone() {
            let source = base.join(&saved.file);
            ensure!(
                source.is_file(),
                "The safety copy of your data is incomplete."
            );
            let temp = saved
                .from
                .with_extension(format!("restore-{}", self.journal.id));
            fs::copy(&source, &temp)?;
            if saved.sqlite {
                for suffix in ["-wal", "-shm"] {
                    let mut name = saved.from.as_os_str().to_owned();
                    name.push(suffix);
                    let _ = fs::remove_file(PathBuf::from(name));
                }
            }
            crate::security::replace_private_file(&temp, &saved.from)?;
        }
        self.journal.data_restored = true;
        self.save()
    }

    /// Ends successfully: the new pair is healthy. Leftovers are removed without insisting; what
    /// a running program still holds is swept later.
    pub fn commit(&mut self) -> Result<()> {
        self.phase(Phase::Committed)?;
        for name in BINARIES {
            let _ = fs::remove_file(self.path(name, "previous"));
        }
        let _ = fs::remove_dir_all(self.backup_dir());
        Ok(())
    }

    pub fn finish(&mut self, phase: Phase, error: Option<String>) -> Result<()> {
        self.journal.error = error;
        self.phase(phase)
    }

    /// Drops the safety copy once it is no longer needed: the previous version runs again on the
    /// live data, or the new one was committed.
    pub fn discard_backup(&self) {
        let _ = fs::remove_dir_all(self.backup_dir());
    }

    /// The path of the safety copy while one exists.
    pub fn backup_location(&self) -> PathBuf {
        self.backup_dir()
    }
}

// ---------------------------------------------------------------------------------------------
// Recovery and housekeeping.

#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    /// Nothing unfinished.
    Idle,
    /// An update is being applied (or has just been handed over): do not start anything.
    Active,
    /// An interrupted update was undone; the previous pair is installed again.
    RolledBack,
    /// An interrupted update could not be undone.
    Failed(String),
}

/// Looks at an unfinished journal whose driver is gone and undoes it. Called before any host is
/// started and when the state is read.
pub fn recover(dir: &Path, at: i64) -> Result<Recovery> {
    let Some(journal) = read(dir) else {
        return Ok(Recovery::Idle);
    };
    if journal.phase.finished() {
        return Ok(Recovery::Idle);
    }
    let Some(_lock) = Lock::try_acquire(dir)? else {
        return Ok(Recovery::Active);
    };
    if at - journal.updated_at < GRACE_SECONDS {
        return Ok(Recovery::Active);
    }
    let mut apply = Apply::new(dir, journal);
    Ok(match apply.roll_back() {
        Ok(()) => {
            apply.finish(
                Phase::RolledBack,
                Some("The update was interrupted and the previous version was restored.".into()),
            )?;
            Recovery::RolledBack
        }
        Err(error) => {
            let message = format!(
                "The update was interrupted and the previous version could not be restored: {error}"
            );
            apply.finish(Phase::Failed, Some(message.clone()))?;
            Recovery::Failed(message)
        }
    })
}

/// Removes what earlier updates left behind: the safety copy of a committed one, executables
/// renamed aside, and staged pairs that are no longer newer. Does nothing while an update runs.
pub fn sweep(dir: &Path, root: &Path, installed: super::Version) {
    let Ok(Some(_lock)) = Lock::try_acquire(dir) else {
        return;
    };
    let journal = read(dir);
    if journal.as_ref().is_some_and(|j| !j.phase.finished()) {
        return;
    }
    if journal
        .as_ref()
        .is_some_and(|j| j.phase == Phase::Committed)
    {
        let _ = fs::remove_dir_all(dir.join(BACKUPS));
    }
    if let Ok(entries) = fs::read_dir(root.join("bin")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let ours = BINARIES
                .iter()
                .any(|b| name.starts_with(&format!(".{}.", exe_name(b))));
            // A rolled-back update keeps nothing in `bin` that it needs: its `.previous` files
            // were renamed back, so what is left is garbage.
            if ours
                && [".new", ".previous", ".failed"]
                    .iter()
                    .any(|s| name.ends_with(s))
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    if let Ok(entries) = fs::read_dir(dir.join("staged")) {
        for entry in entries.flatten() {
            let stale = entry
                .file_name()
                .to_str()
                .and_then(super::Version::parse)
                .is_some_and(|v| v <= installed);
            if stale {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Everything the user may be told about the last update, without any path.
pub fn describe(journal: &Journal) -> serde_json::Value {
    let (state, label) = match journal.phase {
        Phase::Requested | Phase::Stopping | Phase::BackingUp | Phase::Replacing => {
            ("applying", "applying")
        }
        Phase::Restarting => ("restarting", "restarting"),
        Phase::Verifying => ("verifying", "verifying"),
        Phase::Committed => ("success", "success"),
        Phase::RollingBack => ("rolling_back", "rolling_back"),
        Phase::RolledBack => ("rolled_back", "rolled_back"),
        Phase::Failed => ("failed", "failed"),
    };
    serde_json::json!({
        "state": state, "phase": label, "from": journal.from, "to": journal.to,
        "started_at": journal.started_at, "finished_at": journal.finished_at,
        "error": journal.error, "data_restored": journal.data_restored,
        "notes": journal.notes,
        "recovery": (journal.phase == Phase::Failed).then_some(
            "Reinstall the exact version with `cargo install personal-teams-assistant --version X.Y.Z --locked --force` (add --no-default-features for a headless installation), then start the application. Your data and credentials were not removed."),
    })
}

/// Refuses to start a host while an update is in progress and undoes an interrupted one first.
/// Returns the reason a host must not start now.
pub fn startup_block(dir: &Path) -> Option<String> {
    match recover(dir, now()) {
        Ok(Recovery::Active) => Some(
            "An update is being applied. Try again in a moment; `pta update status` shows its progress.".into(),
        ),
        Ok(Recovery::Failed(message)) => Some(message),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
