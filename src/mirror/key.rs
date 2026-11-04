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

/// Rotate the content key: a new random key becomes the active version and the earlier ones stay,
/// so records sealed with them still open until they are sealed again. The content account and
/// the ID key stay (the blinded IDs do not change). The file is replaced atomically.
pub fn rotate(keys: &Path) -> Result<Content> {
    let mut content = load(keys)?.context("There is no account content key to rotate.")?;
    let next = content.keys.keys().max().copied().unwrap_or(0) + 1;
    content.keys.insert(next, random_key());
    content.active = next;
    replace(keys, &content.backup())?;
    Ok(content)
}

/// Install the account's key as the coordinator holds it after a rotation: the same content
/// account and ID key, every version kept here unchanged and an active version at least as new.
/// Without a key here it is written as `import` would. Anything else (another account's key, a
/// changed or missing version, an older active one) is refused and nothing changes. The result
/// says whether a file was written.
pub fn update(keys: &Path, backup: &ContentBackup) -> Result<bool> {
    let received = Content::from_backup(backup)?;
    let Some(current) = load(keys)? else {
        write_new(keys, &received.backup())?;
        return Ok(true);
    };
    if current.backup() == received.backup() {
        return Ok(false);
    }
    ensure!(
        received.account == current.account
            && received.id_key == current.id_key
            && received.active >= current.active
            && current
                .keys
                .iter()
                .all(|(version, key)| received.keys.get(version) == Some(key)),
        "A different account content key is already installed; nothing was replaced."
    );
    replace(keys, &received.backup())?;
    Ok(true)
}

/// Replace the key file atomically.
fn replace(keys: &Path, backup: &ContentBackup) -> Result<()> {
    let temporary = keys.join(format!(".{FILE}.{}", uuid::Uuid::new_v4()));
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(serde_json::to_string(backup)?.as_bytes())?;
        file.sync_all()?;
    }
    crate::security::replace_private_file(&temporary, &path(keys))
}

/// Set the key aside after the account store's copy was erased: the file is renamed to
/// `cloud-content-retired-<at>.json` (still private), so the next upload creates a new key and
/// what Cloudflare may still hold sealed with this one (point-in-time recovery) stays unreadable
/// without it. Returns the new path, or `None` when there was no key.
pub fn retire(keys: &Path, at: i64) -> Result<Option<std::path::PathBuf>> {
    let current = path(keys);
    if !current.exists() {
        return Ok(None);
    }
    let mut retired = keys.join(format!("cloud-content-retired-{at}.json"));
    let mut n = 1;
    while retired.exists() {
        retired = keys.join(format!("cloud-content-retired-{at}-{n}.json"));
        n += 1;
    }
    std::fs::rename(&current, &retired)?;
    Ok(Some(retired))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rotated_key_replaces_only_the_same_accounts_earlier_one() {
        let dir = tempfile::tempdir().unwrap();
        let (here, coordinator) = (dir.path().join("here"), dir.path().join("coordinator"));
        let created = create(&coordinator).unwrap();
        assert!(update(&here, &created.backup()).unwrap());
        assert!(!update(&here, &created.backup()).unwrap());
        let rotated = rotate(&coordinator).unwrap();
        assert_eq!(rotated.active, 2);
        // The rotated set keeps version 1, so it replaces the copy here.
        assert!(update(&here, &rotated.backup()).unwrap());
        assert_eq!(load(&here).unwrap().unwrap().backup(), rotated.backup());
        // An older set, another account's key or a changed version is refused and nothing changes.
        assert!(update(&here, &created.backup()).is_err());
        assert!(update(&here, &Content::generate().backup()).is_err());
        let mut changed = rotated.backup();
        changed.keys.insert(1, STANDARD.encode(random_key()));
        assert!(update(&here, &changed).is_err());
        assert_eq!(load(&here).unwrap().unwrap().backup(), rotated.backup());
        // The trust-time import still never replaces a different key.
        assert!(import(&here, &created.backup()).is_err());
    }
}
