use aes_gcm_siv::{
    Aes256GcmSiv, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::RngCore;
use regex::Regex;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{OnceLock, RwLock},
};
use subtle::ConstantTimeEq;

static KEYRING_PROFILE: OnceLock<RwLock<Option<String>>> = OnceLock::new();

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

#[cfg(any(target_os = "macos", target_os = "windows"))]
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
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        keyring_entry(profile, name)?.set_password(value)?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    anyhow::bail!("desktop credential store is unavailable")
}

pub fn delete_desktop_secret(profile: &str, name: &str) -> Result<()> {
    validate_profile(profile)?;
    validate_name(name)?;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        keyring_entry(profile, name)?.delete_credential()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    anyhow::bail!("desktop credential store is unavailable")
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
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Some(profile) = active_profile() {
        match keyring_entry(&profile, name)?.get_password() {
            Ok(_) => return Ok(Some("system")),
            Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error.into()),
        }
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
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let profile = active_profile().context(format!("missing credential: {name}"))?;
            keyring_entry(&profile, name)?
                .get_password()
                .with_context(|| format!("missing credential: {name}"))?
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            anyhow::bail!("missing credential: {name}")
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
            r#"(?i)\b(?:password|pwd|api[_-]?key|client[_-]?secret|access[_-]?token|refresh[_-]?token|connection[_-]?string)\b[\s\"']*[:=]\s*[\"']?[^\s\"'<>]+"#.to_owned(),
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

/// Dynamic SQL is intentionally unavailable. Only hard-coded, parameterized semantic queries run.
pub fn reject_dynamic_sql(_sql: &str) -> Result<()> {
    bail!("dynamic SQL is disabled; use a semantic read-only tool")
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
    fn vault_authenticates_ciphertext() {
        let v = Vault::new(&STANDARD.encode([7; 32])).unwrap();
        let mut sealed = v.seal(b"example-refresh-token").unwrap();
        assert_eq!(v.open(&sealed).unwrap(), b"example-refresh-token");
        sealed[14] ^= 1;
        assert!(v.open(&sealed).is_err());
    }
    #[test]
    fn sql_fail_closed() {
        for sql in ["SELECT 1", "DROP TABLE x", "SELECT 1; DELETE x", "EXEC foo"] {
            assert!(reject_dynamic_sql(sql).is_err());
        }
    }
}
