use aes_gcm_siv::{
    Aes256GcmSiv, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::RngCore;
use regex::Regex;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{OnceLock, RwLock},
};
use subtle::ConstantTimeEq;

static KEYRING_PROFILE: OnceLock<RwLock<Option<String>>> = OnceLock::new();
static FILE_STORE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
static PREFER_FILE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Isolated web profiles must not fall back to the desktop account's OS keychain.
pub fn prefer_file_credentials(prefer: bool) {
    PREFER_FILE.store(prefer, std::sync::atomic::Ordering::Relaxed);
}
fn prefers_files() -> bool {
    !cfg!(any(target_os = "macos", target_os = "windows"))
        || PREFER_FILE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Select the one local profile whose credentials may be read by this process.
pub fn keyring_profile(profile: Option<&str>) -> Result<()> {
    if let Some(profile) = profile {
        validate_profile(profile)?;
    }
    *KEYRING_PROFILE
        .get_or_init(|| RwLock::new(None))
        .write()
        .unwrap() = profile.map(str::to_owned);
    Ok(())
}

fn validate_profile(profile: &str) -> Result<()> {
    ensure!(
        !profile.is_empty()
            && profile.len() <= 160
            && profile
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c)),
        "invalid credential profile"
    );
    Ok(())
}

/// Root of the private file credential store, used only where no OS keychain is
/// available (Linux and other Unix). Each credential is `<dir>/<profile>/<NAME>`, mode 0600.
pub fn file_credential_store(dir: Option<&Path>) {
    *FILE_STORE
        .get_or_init(|| RwLock::new(None))
        .write()
        .unwrap() = dir.map(Path::to_path_buf);
}

fn file_store_path(profile: &str, name: &str) -> Option<PathBuf> {
    FILE_STORE
        .get()
        .and_then(|s| s.read().unwrap().clone())
        .map(|dir| dir.join(profile).join(name))
}

// Exercised on every platform by tests; used at runtime only without an OS keychain.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn file_read(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(value) => Ok(Some(value.trim_end_matches(['\r', '\n']).to_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

// Exercised on every platform by tests; used at runtime only without an OS keychain.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn file_write(path: &Path, value: &str) -> Result<()> {
    let parent = path.parent().context("invalid credential path")?;
    private_dir(parent)?;
    let temp = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        use std::io::Write;
        let mut file = options.open(&temp)?;
        protect_file(&temp)?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        replace_private_file(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

// Exercised on every platform by tests; used at runtime only without an OS keychain.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn file_delete(path: &Path) -> Result<()> {
    std::fs::remove_file(path).context("credential not found")
}

fn active_profile() -> Option<String> {
    KEYRING_PROFILE
        .get()
        .and_then(|p| p.read().unwrap().clone())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn keyring_entry(profile: &str, name: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(
        &format!("personal-teams-assistant.{profile}"),
        name,
    )?)
}

/// Store a credential for desktop use. The value never belongs in TOML or logs.
pub fn put_desktop_secret(profile: &str, name: &str, value: &str) -> Result<()> {
    validate_profile(profile)?;
    validate_secret(name, value)?;
    if prefers_files() {
        return file_write(
            &file_store_path(profile, name).context("credential store is unavailable")?,
            value,
        );
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        keyring_entry(profile, name)?.set_password(value)?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    file_write(
        &file_store_path(profile, name).context("credential store is unavailable")?,
        value,
    )
}

pub fn delete_desktop_secret(profile: &str, name: &str) -> Result<()> {
    validate_profile(profile)?;
    validate_name(name)?;
    if prefers_files() {
        return file_delete(
            &file_store_path(profile, name).context("credential store is unavailable")?,
        );
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        keyring_entry(profile, name)?.delete_credential()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    file_delete(&file_store_path(profile, name).context("credential store is unavailable")?)
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        Regex::new(r"^[A-Z][A-Z0-9_]*$")?.is_match(name),
        "invalid secret variable name"
    );
    Ok(())
}

fn validate_secret(name: &str, value: &str) -> Result<()> {
    validate_name(name)?;
    ensure!(
        value.len() >= 8 && !value.contains(['\n', '\r']),
        "empty or invalid credential: {name}"
    );
    Ok(())
}

pub fn secret_source(name: &str) -> Result<Option<&'static str>> {
    validate_name(name)?;
    if std::env::var_os(format!("{name}_FILE")).is_some() {
        return Ok(Some("file"));
    }
    if std::env::var_os(name).is_some() {
        return Ok(Some("environment"));
    }
    if prefers_files() {
        return Ok(active_profile()
            .and_then(|profile| file_store_path(&profile, name))
            .filter(|path| path.is_file())
            .map(|_| "system"));
    }
    #[cfg(target_os = "macos")]
    if let Some(profile) = active_profile() {
        // Metadata audit must not decrypt a password or open a Keychain permission dialog.
        let status = std::process::Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                &format!("personal-teams-assistant.{profile}"),
                "-a",
                name,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if status.success() {
            return Ok(Some("system"));
        }
        ensure!(
            status.code() == Some(44),
            "credential metadata audit failed"
        );
    }
    #[cfg(target_os = "windows")]
    if let Some(profile) = active_profile() {
        match keyring_entry(&profile, name)?.get_password() {
            Ok(_) => return Ok(Some("system")),
            Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error.into()),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    if let Some(path) = active_profile().and_then(|profile| file_store_path(&profile, name))
        && path.is_file()
    {
        return Ok(Some("system"));
    }
    Ok(None)
}

pub fn secret(name: &str) -> Result<String> {
    validate_name(name)?;
    let value = if let Ok(file) = std::env::var(format!("{name}_FILE")) {
        std::fs::read_to_string(file)
            .context("cannot read secret mount")?
            .trim_end()
            .to_owned()
    } else if let Ok(value) = std::env::var(name) {
        value
    } else {
        if prefers_files() {
            let path = active_profile()
                .and_then(|profile| file_store_path(&profile, name))
                .with_context(|| format!("missing credential: {name}"))?;
            let value = file_read(&path)?.with_context(|| format!("missing credential: {name}"))?;
            validate_secret(name, &value)?;
            return Ok(value);
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let profile = active_profile().context(format!("missing credential: {name}"))?;
            keyring_entry(&profile, name)?
                .get_password()
                .with_context(|| format!("missing credential: {name}"))?
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let path = active_profile()
                .and_then(|profile| file_store_path(&profile, name))
                .with_context(|| format!("missing credential: {name}"))?;
            file_read(&path)?.with_context(|| format!("missing credential: {name}"))?
        }
    };
    validate_secret(name, &value)?;
    Ok(value)
}
pub fn resolve(reference: &str, bindings: &BTreeMap<String, String>) -> Result<String> {
    ensure!(
        reference.starts_with("secret://"),
        "expected a secret reference"
    );
    secret(
        bindings
            .get(reference)
            .context("secret reference is not allowlisted")?,
    )
}
pub fn constant_eq(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
pub fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    STANDARD.encode(bytes)
}
pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    restrict_windows(path, true)?;
    Ok(())
}
/// Restrict control/configuration files to the OS account, including Windows ACLs.
pub fn protect_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    restrict_windows(path, false)?;
    Ok(())
}
#[cfg(windows)]
fn restrict_windows(path: &Path, directory: bool) -> Result<()> {
    let output = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()?;
    ensure!(
        output.status.success(),
        "cannot resolve Windows account SID"
    );
    let text = String::from_utf8(output.stdout)?;
    let sid = text
        .split(',')
        .next_back()
        .context("missing Windows SID")?
        .trim()
        .trim_matches('"');
    ensure!(
        sid.starts_with("S-1-")
            && sid
                .chars()
                .all(|c| c.is_ascii_digit() || c == 'S' || c == '-'),
        "invalid Windows SID"
    );
    let grant = format!("*{sid}:{}F", if directory { "(OI)(CI)" } else { "" });
    let status = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &grant])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    ensure!(status.success(), "cannot restrict Windows ACL");
    Ok(())
}

pub fn replace_private_file(temp: &Path, target: &Path) -> Result<()> {
    #[cfg(not(windows))]
    std::fs::rename(temp, target)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
        }
        let source: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let dest: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        // REPLACE_EXISTING | WRITE_THROUGH, same filesystem and private directory.
        if unsafe { MoveFileExW(source.as_ptr(), dest.as_ptr(), 1 | 8) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
pub struct Vault(Aes256GcmSiv);
impl Vault {
    pub fn new(encoded_key: &str) -> Result<Self> {
        let bytes = STANDARD
            .decode(encoded_key)
            .context("state key must be base64")?;
        ensure!(bytes.len() == 32, "state key must contain exactly 32 bytes");
        Ok(Self(
            Aes256GcmSiv::new_from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid state key"))?,
        ))
    }
    pub fn seal(&self, value: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce);
        let encrypted = self
            .0
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: value,
                    aad: b"teams-oauth-v1",
                },
            )
            .map_err(|_| anyhow::anyhow!("token encryption failed"))?;
        Ok([nonce.to_vec(), encrypted].concat())
    }
    pub fn open(&self, value: &[u8]) -> Result<Vec<u8>> {
        ensure!(value.len() > 12, "invalid encrypted token");
        self.0
            .decrypt(
                Nonce::from_slice(&value[..12]),
                Payload {
                    msg: &value[12..],
                    aad: b"teams-oauth-v1",
                },
            )
            .map_err(|_| anyhow::anyhow!("token decryption failed"))
    }
}

pub struct Redactor {
    patterns: Vec<Regex>,
    secrets: Vec<String>,
}
impl Redactor {
    pub fn new(custom: &[String], secrets: Vec<String>) -> Result<Self> {
        let mut patterns = vec![
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----".to_owned(),
            r"(?i)\b(?:bearer|basic)\s+[a-z0-9+/_.=-]+".to_owned(),
            r#"(?i)\b(?:password|pwd|token|api[_-]?key|client[_-]?secret|access[_-]?token|refresh[_-]?token|connection[_-]?string)\b[\s\"']*[:=]\s*[\"']?[^\s\"'<>]+"#.to_owned(),
            r"\b(?:sk-|tsf_|apikey_|gh[pousr]_)[A-Za-z0-9_-]{8,}\b".to_owned(),
            r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b".to_owned(),
            r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b".to_owned(),
            r"\b\d{1,2}\.\d{3}\.\d{3}-[0-9kK]\b".to_owned(),
            r"\b\d{7,8}-[0-9kK]\b".to_owned(),
            r"(?:\+\d[\d ()-]{8,}\d)|(?:\b\d[\d -]{11,}\d\b)".to_owned(),
            r"(?i)https?://[^\s/@]+:[^\s/@]+@[^\s]+".to_owned(),
            r"(?i)secret://[a-z0-9_./-]+".to_owned(),
        ];
        patterns.extend(custom.iter().cloned());
        Ok(Self {
            patterns: patterns
                .iter()
                .map(|p| Regex::new(p))
                .collect::<std::result::Result<_, _>>()?,
            secrets,
        })
    }
    pub fn redact(&self, input: &str) -> String {
        let mut out = input.to_owned();
        for value in &self.secrets {
            if value.len() >= 4 {
                out = out.replace(value, "[REDACTED]");
            }
        }
        for p in &self.patterns {
            out = p.replace_all(&out, "[REDACTED]").into_owned();
        }
        out
    }
    pub fn clean(&self, input: &str) -> bool {
        self.redact(input) == input && !input.contains("[REDACTED]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redacts_credentials_and_pii() {
        let r = Redactor::new(&[], vec!["an-exact-test-secret".into()]).unwrap();
        for s in [
            "mail alice@example.com",
            "password=hunter2",
            r#"{"Token":"synthetic-example-token"}"#,
            "Bearer abcdef123",
            "an-exact-test-secret",
            "+56 9 1234 5678",
            "12.345.678-9",
            "secret://sqlserver/payments",
        ] {
            assert!(!r.clean(s), "{s}");
        }
        assert!(r.clean("El servicio está disponible."));
    }
    #[test]
    fn file_store_is_private_atomic_and_reports_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("default").join("TEST_API_KEY");
        assert!(file_read(&path).unwrap().is_none());
        file_write(&path, "first-synthetic-value").unwrap();
        file_write(&path, "second-synthetic-value").unwrap();
        assert_eq!(
            file_read(&path).unwrap().as_deref(),
            Some("second-synthetic-value")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        file_delete(&path).unwrap();
        assert!(file_delete(&path).is_err());
    }
    #[test]
    fn vault_authenticates_ciphertext() {
        let v = Vault::new(&STANDARD.encode([7; 32])).unwrap();
        let mut sealed = v.seal(b"example-refresh-token").unwrap();
        assert_eq!(v.open(&sealed).unwrap(), b"example-refresh-token");
        sealed[14] ^= 1;
        assert!(v.open(&sealed).is_err());
    }
}
