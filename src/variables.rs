//! Account-owned stored variables: a name, a description and a confidential value.
//!
//! Names and descriptions are metadata kept in clear so they can be listed and searched. Values
//! are authenticated ciphertext bound to account, entry, name, revision and data-key version.
//! Every account has its own random data key; a master (wrapping) key protects the data keys and
//! lives in a separate private directory, never in the database or in package output. A missing or
//! unreadable key never causes a new key to be generated over existing data.
use aes_gcm_siv::{
    Aes256GcmSiv, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const MAX_NAME: usize = 128;
pub const MAX_DESCRIPTION: usize = 500;
pub const MAX_VALUE: usize = 16 * 1024;
pub const MAX_VARIABLES: usize = 500;

const RECORD_DOMAIN: &[u8] = b"personal-teams-assistant/variable-record/v1";
const WRAP_DOMAIN: &[u8] = b"personal-teams-assistant/variable-key-wrap/v1";
const BACKUP_FORMAT: &str = "pta-variable-keys-1";

#[derive(Debug)]
pub enum Error {
    /// The request is invalid; the message says how to fix it.
    Input(String),
    /// The state differs from what the request assumed; inspect before repeating.
    Conflict(String),
    /// A required key is missing or unusable. Nothing was changed and no key was generated.
    Key(String),
    /// Stored data failed authentication and was not used.
    Integrity(String),
    /// The local store could not be used.
    Storage(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Self::Input(m)
        | Self::Conflict(m)
        | Self::Key(m)
        | Self::Integrity(m)
        | Self::Storage(m)) = self;
        f.write_str(m)
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage("The variable store could not be read or written.".into())
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Storage("The variable store or its key directory could not be accessed.".into())
    }
}
impl From<anyhow::Error> for Error {
    fn from(_: anyhow::Error) -> Self {
        Self::Storage("The variable store or its key directory could not be accessed.".into())
    }
}
type Result<T> = std::result::Result<T, Error>;

fn input(message: &str) -> Error {
    Error::Input(message.into())
}
fn key_missing() -> Error {
    Error::Key("The variable encryption key is missing or unreadable. Restore it from the operator's key backup (pta variables key restore); no key was generated and nothing was changed.".into())
}
fn integrity(name: &str) -> Error {
    Error::Integrity(format!(
        "The stored variable {name} failed authentication and was not used."
    ))
}

/// A confidential value. It never prints, so it cannot reach logs or error text by accident.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);
impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// What can be shown about a variable: never its value or ciphertext.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Increases with every change to the entry.
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct NewVariable {
    pub id: String,
    pub name: String,
    pub description: String,
    pub value: Secret,
}

#[derive(Debug)]
pub struct Created {
    pub variable: Metadata,
    /// The same entry already existed with identical content: nothing was written.
    pub replayed: bool,
}

#[derive(Debug, Serialize)]
pub struct Verification {
    pub checked: usize,
    pub failed: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Rotation {
    pub master_version: u32,
    pub data_key_version: u32,
    pub variables: usize,
    pub retained_master_versions: Vec<u32>,
}

/// Master keys for an operator-held backup. The file never contains ciphertext or values.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyBackup {
    pub format: String,
    pub masters: BTreeMap<u32, String>,
}
impl fmt::Debug for KeyBackup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyBackup({} keys)", self.masters.len())
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let first = chars
        .next()
        .ok_or_else(|| input("A variable needs a name."))?;
    if name.len() > MAX_NAME
        || !(first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(input(
            "A variable name uses letters, digits and _, cannot start with a digit and has at most 128 characters, like DEPLOY_ENDPOINT.",
        ));
    }
    Ok(())
}
pub fn validate_description(description: &str) -> Result<()> {
    let length = description.chars().count();
    if description.trim().is_empty() || length > MAX_DESCRIPTION {
        return Err(input(
            "A variable needs a description of 1 to 500 characters saying what it is for.",
        ));
    }
    // Control and bidirectional-override characters could make the text mislead its reader.
    if description
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
    {
        return Err(input(
            "A description is plain text on one line, without control characters.",
        ));
    }
    Ok(())
}
pub fn validate_value(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_VALUE || value.contains('\0') {
        return Err(input(
            "A value has 1 byte to 16 KiB of UTF-8 text (newlines allowed, no NUL characters).",
        ));
    }
    Ok(())
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn name_key(name: &str) -> String {
    // Environment names are case-insensitive on some systems, so FOO and foo are one name.
    name.to_ascii_uppercase()
}
fn length_prefixed(out: &mut Vec<u8>, part: &[u8]) {
    out.extend((part.len() as u32).to_be_bytes());
    out.extend(part);
}
fn record_aad(account: &str, id: &str, name: &str, revision: u64, data_key: u32) -> Vec<u8> {
    let mut aad = Vec::new();
    length_prefixed(&mut aad, RECORD_DOMAIN);
    length_prefixed(&mut aad, account.as_bytes());
    length_prefixed(&mut aad, id.as_bytes());
    length_prefixed(&mut aad, name.as_bytes());
    aad.extend(revision.to_be_bytes());
    aad.extend(data_key.to_be_bytes());
    aad
}
fn wrap_aad(account: &str, data_key: u32, master: u32) -> Vec<u8> {
    let mut aad = Vec::new();
    length_prefixed(&mut aad, WRAP_DOMAIN);
    length_prefixed(&mut aad, account.as_bytes());
    aad.extend(data_key.to_be_bytes());
    aad.extend(master.to_be_bytes());
    aad
}
fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256GcmSiv::new_from_slice(key).map_err(|_| key_missing())?;
    let mut nonce = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| Error::Storage("The value could not be encrypted.".into()))?;
    Ok([nonce.to_vec(), encrypted].concat())
}
fn open(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() <= 12 {
        return None;
    }
    Aes256GcmSiv::new_from_slice(key)
        .ok()?
        .decrypt(
            Nonce::from_slice(&sealed[..12]),
            Payload {
                msg: &sealed[12..],
                aad,
            },
        )
        .ok()
}
fn random_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    key
}
fn decode_key(text: &str) -> Option<[u8; 32]> {
    STANDARD.decode(text.trim()).ok()?.try_into().ok()
}

/// Master keys, one private file per version, in a directory apart from the ciphertext.
struct KeyDir(PathBuf);
impl KeyDir {
    fn path(&self, version: u32) -> PathBuf {
        self.0.join(format!("master-{version}.key"))
    }
    fn versions(&self) -> Result<BTreeSet<u32>> {
        let mut found = BTreeSet::new();
        let entries = match std::fs::read_dir(&self.0) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let name = entry?.file_name();
            if let Some(version) = name
                .to_str()
                .and_then(|n| n.strip_prefix("master-"))
                .and_then(|n| n.strip_suffix(".key"))
                .and_then(|n| n.parse::<u32>().ok())
            {
                found.insert(version);
            }
        }
        Ok(found)
    }
    fn read(&self, version: u32) -> Result<[u8; 32]> {
        let text = std::fs::read_to_string(self.path(version)).map_err(|_| key_missing())?;
        decode_key(&text).ok_or_else(key_missing)
    }
    /// Never replaces an existing key file.
    fn create(&self, version: u32, key: &[u8; 32]) -> Result<()> {
        crate::security::private_dir(&self.0)?;
        let target = self.path(version);
        if target.exists() {
            return Err(Error::Conflict(
                "A master key with that version already exists.".into(),
            ));
        }
        let temp = self.0.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            crate::security::protect_file(&temp)?;
            file.write_all(STANDARD.encode(key).as_bytes())?;
            file.sync_all()?;
            crate::security::replace_private_file(&temp, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
}

pub struct Store {
    db: Mutex<Connection>,
    keys: KeyDir,
    account: String,
}

impl Store {
    /// The store of one installation profile. Its account key is stable across account binding
    /// changes; a profile already bound to one portal account refuses to serve another.
    pub fn open_profile(profile: &Path, portal_account: Option<&str>) -> Result<Self> {
        let dir = profile.join("variables");
        crate::security::private_dir(&dir)?;
        let db = connect(&dir.join("variables.db"))?;
        let meta = |key: &str| -> Result<Option<String>> {
            Ok(db
                .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
                .optional()?)
        };
        let set = |key: &str, value: &str| -> Result<()> {
            db.execute(
                "INSERT INTO meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2",
                params![key, value],
            )?;
            Ok(())
        };
        let account = match meta("storage_account")? {
            Some(account) => account,
            None => {
                let account = uuid::Uuid::new_v4().to_string();
                set("storage_account", &account)?;
                account
            }
        };
        if let Some(portal) = portal_account {
            match meta("portal_account")? {
                Some(bound) if bound != portal => {
                    return Err(Error::Conflict(
                        "These variables belong to another assistant account.".into(),
                    ));
                }
                Some(_) => {}
                None => set("portal_account", portal)?,
            }
        }
        Ok(Self {
            db: Mutex::new(db),
            keys: KeyDir(profile.join("keys")),
            account,
        })
    }

    /// A store scoped to an explicit account key (several accounts may share one database).
    pub fn open_account(database: &Path, keys: &Path, account: &str) -> Result<Self> {
        if let Some(parent) = database.parent() {
            crate::security::private_dir(parent)?;
        }
        Ok(Self {
            db: Mutex::new(connect(database)?),
            keys: KeyDir(keys.to_path_buf()),
            account: account.to_owned(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Account-owned names, descriptions and version metadata, optionally filtered by a text
    /// found in either (case-insensitive). Values and ciphertext are never selected.
    pub fn list(&self, query: Option<&str>) -> Result<Vec<Metadata>> {
        let db = self.lock();
        let mut statement = db.prepare(
            "SELECT id,name,description,revision,created_at,updated_at FROM variables WHERE account=?1 ORDER BY name_key",
        )?;
        let rows = statement.query_map([&self.account], |r| {
            Ok(Metadata {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                version: r.get::<_, i64>(3)? as u64,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
            })
        })?;
        let needle = query
            .map(str::to_lowercase)
            .filter(|q| !q.trim().is_empty());
        let mut found = Vec::new();
        for row in rows {
            let row = row?;
            if needle.as_ref().is_none_or(|q| {
                row.name.to_lowercase().contains(q.trim())
                    || row.description.to_lowercase().contains(q.trim())
            }) {
                found.push(row);
            }
        }
        Ok(found)
    }

    /// Create an entry. Never overwrites: a retry with the same entry ID and content reports the
    /// existing entry, anything else that collides is refused so the caller inspects first.
    pub fn create(&self, new: NewVariable) -> Result<Created> {
        validate_name(&new.name)?;
        validate_description(&new.description)?;
        validate_value(new.value.expose())?;
        let id = uuid::Uuid::parse_str(&new.id)
            .map_err(|_| input("The entry ID must be a UUID."))?
            .hyphenated()
            .to_string();
        let mut db = self.lock();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT name,description,revision,dek_version,ciphertext,created_at,updated_at FROM variables WHERE account=?1 AND id=?2",
                params![self.account, id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? as u64,
                        r.get::<_, i64>(3)? as u32,
                        r.get::<_, Vec<u8>>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;
        if let Some((name, description, revision, data_key, ciphertext, created_at, updated_at)) =
            existing
        {
            let same = name == new.name
                && description == new.description
                && self
                    .unwrap_data_key(&tx, data_key)
                    .ok()
                    .and_then(|key| {
                        open(
                            &key,
                            &record_aad(&self.account, &id, &name, revision, data_key),
                            &ciphertext,
                        )
                    })
                    .is_some_and(|plain| {
                        crate::security::constant_eq(
                            &String::from_utf8_lossy(&plain),
                            new.value.expose(),
                        )
                    });
            if !same {
                return Err(Error::Conflict(
                    "This entry ID already holds a different variable. Inspect the list before choosing a new entry.".into(),
                ));
            }
            return Ok(Created {
                variable: Metadata {
                    id,
                    name,
                    description,
                    version: revision,
                    created_at,
                    updated_at,
                },
                replayed: true,
            });
        }
        let taken: Option<String> = tx
            .query_row(
                "SELECT name FROM variables WHERE account=?1 AND name_key=?2",
                params![self.account, name_key(&new.name)],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(taken) = taken {
            return Err(Error::Conflict(format!(
                "A variable named {taken} already exists in this account. Inspect it before choosing another name."
            )));
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM variables WHERE account=?1",
            [&self.account],
            |r| r.get(0),
        )?;
        if count as usize >= MAX_VARIABLES {
            return Err(input("This account holds the maximum of 500 variables."));
        }
        let (data_key, key) = self.active_data_key(&tx)?;
        let at = now();
        let ciphertext = seal(
            &key,
            &record_aad(&self.account, &id, &new.name, 1, data_key),
            new.value.expose().as_bytes(),
        )?;
        tx.execute(
            "INSERT INTO variables(account,id,name,name_key,description,revision,dek_version,ciphertext,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,1,?6,?7,?8,?8)",
            params![
                self.account,
                id,
                new.name,
                name_key(&new.name),
                new.description,
                data_key,
                ciphertext,
                at
            ],
        )?;
        tx.commit()?;
        Ok(Created {
            variable: Metadata {
                id,
                name: new.name,
                description: new.description,
                version: 1,
                created_at: at,
                updated_at: at,
            },
            replayed: false,
        })
    }

    /// The value of one entry, for an authorized in-process consumer. No operation returns it to
    /// a caller that could pass it to a model.
    pub fn resolve(&self, name: &str) -> Result<Secret> {
        let db = self.lock();
        let row = db
            .query_row(
                "SELECT id,name,revision,dek_version,ciphertext FROM variables WHERE account=?1 AND name_key=?2",
                params![self.account, name_key(name)],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? as u64,
                        r.get::<_, i64>(3)? as u32,
                        r.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| input("There is no stored variable with that name."))?;
        let (id, name, revision, data_key, ciphertext) = row;
        let key = self.unwrap_data_key(&db, data_key)?;
        let plain = open(
            &key,
            &record_aad(&self.account, &id, &name, revision, data_key),
            &ciphertext,
        )
        .ok_or_else(|| integrity(&name))?;
        String::from_utf8(plain)
            .map(Secret)
            .map_err(|_| integrity(&name))
    }

    /// Check that every entry authenticates with the current keys, reporting names only.
    pub fn verify(&self) -> Result<Verification> {
        let rows: Vec<(String, String, u64, u32, Vec<u8>)> = {
            let db = self.lock();
            let mut statement = db.prepare(
                "SELECT id,name,revision,dek_version,ciphertext FROM variables WHERE account=?1 ORDER BY name_key",
            )?;
            statement
                .query_map([&self.account], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get::<_, i64>(2)? as u64,
                        r.get::<_, i64>(3)? as u32,
                        r.get(4)?,
                    ))
                })?
                .collect::<std::result::Result<_, _>>()?
        };
        let db = self.lock();
        let mut failed = Vec::new();
        for (id, name, revision, data_key, ciphertext) in &rows {
            let good = self
                .unwrap_data_key(&db, *data_key)
                .ok()
                .and_then(|key| {
                    open(
                        &key,
                        &record_aad(&self.account, id, name, *revision, *data_key),
                        ciphertext,
                    )
                })
                .is_some();
            if !good {
                failed.push(name.clone());
            }
        }
        Ok(Verification {
            checked: rows.len(),
            failed,
        })
    }

    /// Replace the master key and this account's data key. Everything is re-wrapped and
    /// re-encrypted in one transaction after each entry authenticated with its current keys; a
    /// failure changes nothing. Earlier master keys are kept so older backups still restore.
    pub fn rotate(&self) -> Result<Rotation> {
        let mut db = self.lock();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (old_data_key, old_key) = self.active_data_key_existing(&tx)?;
        let records: Vec<(String, String, u64, Vec<u8>)> = {
            let mut statement =
                tx.prepare("SELECT id,name,revision,ciphertext FROM variables WHERE account=?1")?;
            statement
                .query_map([&self.account], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64, r.get(3)?))
                })?
                .collect::<std::result::Result<_, _>>()?
        };
        let mut plain = Vec::with_capacity(records.len());
        for (id, name, revision, ciphertext) in &records {
            let value = open(
                &old_key,
                &record_aad(&self.account, id, name, *revision, old_data_key),
                ciphertext,
            )
            .ok_or_else(|| integrity(name))?;
            plain.push(value);
        }
        // Every wrapped data key (of any account sharing the database) moves to the new master.
        let wrapped: Vec<(String, u32, u32, Vec<u8>)> = {
            let mut statement =
                tx.prepare("SELECT account,version,master_version,wrapped FROM account_keys")?;
            statement
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get::<_, i64>(1)? as u32,
                        r.get::<_, i64>(2)? as u32,
                        r.get(3)?,
                    ))
                })?
                .collect::<std::result::Result<_, _>>()?
        };
        let existing = self.keys.versions()?;
        let new_master = existing
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .max(meta_number(&tx, "current_master")?.unwrap_or(0))
            + 1;
        let master_key = random_key();
        let mut rewrapped = Vec::new();
        for (account, version, master, sealed) in &wrapped {
            if *account == self.account && *version == old_data_key {
                continue; // replaced below
            }
            let data_key = open(
                &self.keys.read(*master)?,
                &wrap_aad(account, *version, *master),
                sealed,
            )
            .and_then(|k| <[u8; 32]>::try_from(k).ok())
            .ok_or_else(key_missing)?;
            rewrapped.push((
                account.clone(),
                *version,
                seal(
                    &master_key,
                    &wrap_aad(account, *version, new_master),
                    &data_key,
                )?,
            ));
        }
        let new_data_key = wrapped
            .iter()
            .filter(|(a, ..)| *a == self.account)
            .map(|(_, v, ..)| *v)
            .max()
            .unwrap_or(0)
            + 1;
        let fresh = random_key();
        self.keys.create(new_master, &master_key)?;
        for (account, version, sealed) in &rewrapped {
            tx.execute(
                "UPDATE account_keys SET master_version=?3, wrapped=?4 WHERE account=?1 AND version=?2",
                params![account, version, new_master, sealed],
            )?;
        }
        tx.execute(
            "DELETE FROM account_keys WHERE account=?1 AND version=?2",
            params![self.account, old_data_key],
        )?;
        tx.execute(
            "UPDATE account_keys SET active=0 WHERE account=?1",
            [&self.account],
        )?;
        tx.execute(
            "INSERT INTO account_keys(account,version,master_version,wrapped,active,created_at) VALUES (?1,?2,?3,?4,1,?5)",
            params![
                self.account,
                new_data_key,
                new_master,
                seal(
                    &master_key,
                    &wrap_aad(&self.account, new_data_key, new_master),
                    &fresh
                )?,
                now()
            ],
        )?;
        for ((id, name, revision, _), value) in records.iter().zip(&plain) {
            tx.execute(
                "UPDATE variables SET dek_version=?3, ciphertext=?4 WHERE account=?1 AND id=?2",
                params![
                    self.account,
                    id,
                    new_data_key,
                    seal(
                        &fresh,
                        &record_aad(&self.account, id, name, *revision, new_data_key),
                        value
                    )?
                ],
            )?;
        }
        set_meta(&tx, "current_master", &new_master.to_string())?;
        tx.commit()?;
        Ok(Rotation {
            master_version: new_master,
            data_key_version: new_data_key,
            variables: records.len(),
            retained_master_versions: self.keys.versions()?.into_iter().collect(),
        })
    }

    /// The master keys, for an operator-held backup kept apart from the database.
    pub fn export_keys(&self) -> Result<KeyBackup> {
        let mut masters = BTreeMap::new();
        for version in self.keys.versions()? {
            masters.insert(version, STANDARD.encode(self.keys.read(version)?));
        }
        if masters.is_empty() {
            return Err(Error::Key("There are no variable keys to back up.".into()));
        }
        Ok(KeyBackup {
            format: BACKUP_FORMAT.into(),
            masters,
        })
    }

    /// Restore master keys from a backup. An existing key is never replaced.
    pub fn import_keys(&self, backup: &KeyBackup) -> Result<Vec<u32>> {
        if backup.format != BACKUP_FORMAT || backup.masters.is_empty() {
            return Err(input("This is not a variable key backup."));
        }
        let mut parsed = Vec::new();
        for (version, text) in &backup.masters {
            let key = decode_key(text).ok_or_else(|| input("The key backup is damaged."))?;
            if let Ok(current) = self.keys.read(*version)
                && current != key
            {
                return Err(Error::Conflict(
                    "A different master key with the same version is already installed; nothing was restored.".into(),
                ));
            }
            parsed.push((*version, key));
        }
        let mut written = Vec::new();
        for (version, key) in parsed {
            if !self.keys.path(version).exists() {
                self.keys.create(version, &key)?;
                written.push(version);
            }
        }
        Ok(written)
    }

    fn unwrap_data_key(&self, db: &Connection, version: u32) -> Result<[u8; 32]> {
        let (master, sealed): (u32, Vec<u8>) = db
            .query_row(
                "SELECT master_version,wrapped FROM account_keys WHERE account=?1 AND version=?2",
                params![self.account, version],
                |r| Ok((r.get::<_, i64>(0)? as u32, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(key_missing)?;
        open(
            &self.keys.read(master)?,
            &wrap_aad(&self.account, version, master),
            &sealed,
        )
        .and_then(|k| <[u8; 32]>::try_from(k).ok())
        .ok_or_else(key_missing)
    }

    fn active_data_key_existing(&self, db: &Connection) -> Result<(u32, [u8; 32])> {
        let version: Option<i64> = db
            .query_row(
                "SELECT version FROM account_keys WHERE account=?1 AND active=1",
                [&self.account],
                |r| r.get(0),
            )
            .optional()?;
        let version = version
            .ok_or_else(|| Error::Key("This account has no variable key yet.".into()))?
            as u32;
        Ok((version, self.unwrap_data_key(db, version)?))
    }

    /// The account's active data key. Key material is created only for a store that has none at
    /// all; a missing key for existing data fails closed instead.
    fn active_data_key(&self, db: &Connection) -> Result<(u32, [u8; 32])> {
        let has_active: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM account_keys WHERE account=?1 AND active=1)",
            [&self.account],
            |r| r.get(0),
        )?;
        if has_active {
            return self.active_data_key_existing(db);
        }
        let own_rows: i64 = db.query_row(
            "SELECT COUNT(*) FROM account_keys WHERE account=?1",
            [&self.account],
            |r| r.get(0),
        )?;
        let own_variables: i64 = db.query_row(
            "SELECT COUNT(*) FROM variables WHERE account=?1",
            [&self.account],
            |r| r.get(0),
        )?;
        if own_rows > 0 || own_variables > 0 {
            return Err(key_missing());
        }
        let files = self.keys.versions()?;
        let any_wrapped: i64 =
            db.query_row("SELECT COUNT(*) FROM account_keys", [], |r| r.get(0))?;
        let recorded = meta_number(db, "current_master")?;
        let master = match (recorded, files.iter().next_back().copied()) {
            (Some(version), _) => (version, self.keys.read(version)?),
            (None, Some(version)) => (version, self.keys.read(version)?),
            (None, None) if any_wrapped == 0 => {
                let key = random_key();
                self.keys.create(1, &key)?;
                (1, key)
            }
            (None, None) => return Err(key_missing()),
        };
        set_meta(db, "current_master", &master.0.to_string())?;
        let data_key = random_key();
        db.execute(
            "INSERT INTO account_keys(account,version,master_version,wrapped,active,created_at) VALUES (?1,1,?2,?3,1,?4)",
            params![
                self.account,
                master.0,
                seal(&master.1, &wrap_aad(&self.account, 1, master.0), &data_key)?,
                now()
            ],
        )?;
        Ok((1, data_key))
    }
}

fn connect(path: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    crate::security::protect_file(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS account_keys (
            account TEXT NOT NULL, version INTEGER NOT NULL, master_version INTEGER NOT NULL,
            wrapped BLOB NOT NULL, active INTEGER NOT NULL, created_at INTEGER NOT NULL,
            PRIMARY KEY (account, version));
        CREATE TABLE IF NOT EXISTS variables (
            account TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL, name_key TEXT NOT NULL,
            description TEXT NOT NULL, revision INTEGER NOT NULL, dek_version INTEGER NOT NULL,
            ciphertext BLOB NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            PRIMARY KEY (account, id), UNIQUE (account, name_key));",
    )?;
    Ok(db)
}
fn meta_number(db: &Connection, key: &str) -> Result<Option<u32>> {
    Ok(db
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .and_then(|v| v.parse().ok()))
}
fn set_meta(db: &Connection, key: &str, value: &str) -> Result<()> {
    db.execute(
        "INSERT INTO meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2",
        params![key, value],
    )?;
    Ok(())
}

/// Write a private file that must not exist yet (key backups).
pub fn write_new_private(path: &Path, content: &str) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            Error::Conflict("The output file already exists; choose a new private path.".into())
        } else {
            Error::from(error)
        }
    })?;
    crate::security::protect_file(path)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests;
