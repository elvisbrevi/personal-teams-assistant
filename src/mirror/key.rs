//! The account's content key: 32 random bytes that seal every record before it leaves this
//! installation. It lives in the installation's private `keys/` directory, is never sent to
//! Cloudflare, and travels only in the operator-held key backup (`variable_key_backup`). Next to
//! the keys the file holds two more values that never leave either: the random identifier of the
//! content account that the sealed records' associated data binds (so the Worker's account ID is
//! never needed and never sent) and the key that blinds record identifiers.

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

pub const FILE: &str = "cloud-content.json";
pub const FORMAT: &str = "pta-cloud-content-1";

/// The key material as it is stored and backed up.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContentBackup {
    pub format: String,
    /// Random identifier of the content account; part of every record's associated data.
    pub account: String,
    /// Base64 key that blinds record identifiers; stable across content key rotations.
    pub id_key: String,
    /// The version that seals new records.
    pub active: u32,
    /// Base64 content keys by version; older versions stay to open older records.
    pub keys: BTreeMap<u32, String>,
}

/// The decoded key material.
#[derive(Clone)]
pub struct Content {
    pub account: String,
    pub active: u32,
    pub(super) id_key: [u8; 32],
    keys: BTreeMap<u32, [u8; 32]>,
}

impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Content")
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

fn random_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    key
}

fn decode(text: &str) -> Result<[u8; 32]> {
    STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .context("The account content key file is damaged.")
}

impl Content {
    pub fn key(&self, version: u32) -> Option<&[u8; 32]> {
        self.keys.get(&version)
    }

    pub fn active_key(&self) -> &[u8; 32] {
        &self.keys[&self.active]
    }

    /// A key set with one fresh key, for tests and for `create`.
    pub fn generate() -> Self {
        Self {
            account: uuid::Uuid::new_v4().to_string(),
            active: 1,
            id_key: random_key(),
            keys: BTreeMap::from([(1, random_key())]),
        }
    }

    pub fn backup(&self) -> ContentBackup {
        ContentBackup {
            format: FORMAT.into(),
            account: self.account.clone(),
            id_key: STANDARD.encode(self.id_key),
            active: self.active,
            keys: self
                .keys
                .iter()
                .map(|(version, key)| (*version, STANDARD.encode(key)))
                .collect(),
        }
    }

    pub fn from_backup(backup: &ContentBackup) -> Result<Self> {
        ensure!(
            backup.format == FORMAT && !backup.keys.is_empty(),
            "This is not an account content key backup."
        );
        ensure!(
            uuid::Uuid::parse_str(&backup.account).is_ok_and(|id| id.to_string() == backup.account),
            "The account content key file is damaged."
        );
        let keys = backup
            .keys
            .iter()
            .map(|(version, text)| Ok((*version, decode(text)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        ensure!(
            keys.contains_key(&backup.active),
            "The account content key file is damaged."
        );
        Ok(Self {
            account: backup.account.clone(),
            active: backup.active,
            id_key: decode(&backup.id_key)?,
            keys,
        })
    }
}

fn path(keys: &Path) -> std::path::PathBuf {
    keys.join(FILE)
}

/// The content key kept in `keys`, if the installation has one.
pub fn load(keys: &Path) -> Result<Option<Content>> {
    match std::fs::read_to_string(path(keys)) {
        Ok(text) => {
            let backup: ContentBackup =
                serde_json::from_str(&text).context("The account content key file is damaged.")?;
            Ok(Some(Content::from_backup(&backup)?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_new(keys: &Path, backup: &ContentBackup) -> Result<()> {
    use std::io::Write;
    crate::security::private_dir(keys)?;
    let target = path(keys);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&target)
        .context("The account content key could not be created.")?;
    crate::security::protect_file(&target)?;
    file.write_all(serde_json::to_string(backup)?.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// Create the account's content key. An existing key is never replaced.
pub fn create(keys: &Path) -> Result<Content> {
    if path(keys).exists() {
        bail!("The account content key already exists.");
    }
    let content = Content::generate();
    write_new(keys, &content.backup())?;
    Ok(content)
}

/// Restore the key from an operator backup. An existing different key is never replaced; the
/// result says whether a file was written.
pub fn import(keys: &Path, backup: &ContentBackup) -> Result<bool> {
    let restored = Content::from_backup(backup)?;
    match load(keys)? {
        Some(current) if current.backup() == restored.backup() => Ok(false),
        Some(_) => {
            bail!("A different account content key is already installed; nothing was restored.")
        }
        None => {
            write_new(keys, &restored.backup())?;
            Ok(true)
        }
    }
}
