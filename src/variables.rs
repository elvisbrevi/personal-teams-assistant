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
/// Audit events kept per account.
const AUDIT_KEPT: i64 = 5000;

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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Increases with every change to the entry.
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A slice of the catalog and the revision it belongs to.
#[derive(Debug, Serialize)]
pub struct CatalogPage {
    pub revision: u64,
    pub total: usize,
    pub items: Vec<Metadata>,
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

#[derive(Debug)]
pub struct Updated {
    pub variable: Metadata,
    /// The same change was already applied: nothing was written.
    pub replayed: bool,
}

#[derive(Debug, Serialize)]
pub struct Deleted {
    pub id: String,
    pub name: String,
    /// The version recorded by the deletion; any later write based on an earlier one is stale.
    pub version: u64,
    pub replayed: bool,
}

/// One security-relevant action on the variables of an account. Never holds a value.
#[derive(Debug, Serialize)]
pub struct AuditEvent {
    pub at: i64,
    pub account: String,
    pub action: String,
    pub entry: Option<String>,
    pub outcome: String,
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

    /// One consistent page of the account's catalog for synchronization, ordered by entry ID.
    /// `revision` counts every committed change (each create, edit and delete adds exactly one,
    /// because a deletion replaces the entry's version with a tombstone one higher), so it only
    /// grows and a smaller number than a client already holds means a restored or replayed
    /// catalog. Values and ciphertext are never selected.
    pub fn catalog_page(&self, offset: usize, limit: usize) -> Result<CatalogPage> {
        let mut db = self.lock();
        let tx = db.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let sum = |table: &str| -> Result<u64> {
            Ok(tx.query_row(
                &format!("SELECT COALESCE(SUM(revision),0) FROM {table} WHERE account=?1"),
                [&self.account],
                |r| r.get::<_, i64>(0),
            )? as u64)
        };
        let revision = sum("variables")? + sum("tombstones")?;
        let total: i64 = tx.query_row(
            "SELECT COUNT(*) FROM variables WHERE account=?1",
            [&self.account],
            |r| r.get(0),
        )?;
        let mut statement = tx.prepare(
            "SELECT id,name,description,revision,created_at,updated_at FROM variables WHERE account=?1 ORDER BY id LIMIT ?2 OFFSET ?3",
        )?;
        let items = statement
            .query_map(params![self.account, limit as i64, offset as i64], |r| {
                Ok(Metadata {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    description: r.get(2)?,
                    version: r.get::<_, i64>(3)? as u64,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        Ok(CatalogPage {
            revision,
            total: total as usize,
            items,
        })
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
        if tombstone(&tx, &self.account, &id)?.is_some() {
            return Err(deleted());
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
        let row = fetch(
            &db,
            "SELECT id,name,description,revision,dek_version,ciphertext,created_at,updated_at FROM variables WHERE account=?1 AND name_key=?2",
            params![self.account, name_key(name)],
        )?
        .ok_or_else(|| input("There is no stored variable with that name."))?;
        self.plaintext(&db, &row)
    }

    /// One entry of this account by ID with its value, for a human's explicit reveal or copy.
    /// An ID of another account, or one never issued, is simply absent; a deleted one says so.
    pub fn reveal(&self, id: &str) -> Result<(Metadata, Secret)> {
        let id = entry_id(id)?;
        let db = self.lock();
        let Some(row) = fetch(
            &db,
            "SELECT id,name,description,revision,dek_version,ciphertext,created_at,updated_at FROM variables WHERE account=?1 AND id=?2",
            params![self.account, id],
        )?
        else {
            return Err(missing(&db, &self.account, &id)?);
        };
        let value = self.plaintext(&db, &row)?;
        Ok((row.metadata(), value))
    }

    /// Change the description, the value or both of one entry. `expected` is the version the
    /// caller saw: another change in between is a conflict, never an overwrite. The ID, name and
    /// description survive a rotation, so existing references keep working.
    pub fn update(
        &self,
        id: &str,
        expected: u64,
        description: Option<&str>,
        value: Option<&Secret>,
    ) -> Result<Updated> {
        let id = entry_id(id)?;
        if description.is_none() && value.is_none() {
            return Err(input("Give a new description, a new value or both."));
        }
        if let Some(description) = description {
            validate_description(description)?;
        }
        if let Some(value) = value {
            validate_value(value.expose())?;
        }
        let mut db = self.lock();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(row) = fetch(
            &tx,
            "SELECT id,name,description,revision,dek_version,ciphertext,created_at,updated_at FROM variables WHERE account=?1 AND id=?2",
            params![self.account, id],
        )?
        else {
            return Err(missing(&tx, &self.account, &id)?);
        };
        let current = self.plaintext(&tx, &row)?;
        let same_description = description.is_none_or(|d| d == row.description);
        let same_value =
            value.is_none_or(|v| crate::security::constant_eq(v.expose(), current.expose()));
        if row.revision != expected {
            // The caller's earlier attempt may have succeeded before its reply was lost.
            if row.revision == expected + 1 && same_description && same_value {
                return Ok(Updated {
                    variable: row.metadata(),
                    replayed: true,
                });
            }
            return Err(Error::Conflict(format!(
                "{} changed since it was loaded (it is now version {}). Inspect it before editing again.",
                row.name, row.revision
            )));
        }
        if same_description && same_value {
            return Err(input(
                "Nothing would change: the description and value match.",
            ));
        }
        let (data_key, key) = self.active_data_key(&tx)?;
        let revision = row.revision + 1;
        let at = now();
        let description = description.unwrap_or(&row.description);
        let sealed = seal(
            &key,
            &record_aad(&self.account, &id, &row.name, revision, data_key),
            value.unwrap_or(&current).expose().as_bytes(),
        )?;
        tx.execute(
            "UPDATE variables SET description=?3, revision=?4, dek_version=?5, ciphertext=?6, updated_at=?7 WHERE account=?1 AND id=?2",
            params![self.account, id, description, revision as i64, data_key, sealed, at],
        )?;
        tx.commit()?;
        Ok(Updated {
            variable: Metadata {
                id,
                name: row.name,
                description: description.to_owned(),
                version: revision,
                created_at: row.created_at,
                updated_at: at,
            },
            replayed: false,
        })
    }

    /// Delete one entry. The deletion keeps a tombstone so a stale write, a replay of the
    /// creation or a later synchronization cannot bring the entry back. It cannot retract a
    /// value already given to a program, nor revoke the credential at its provider.
    pub fn delete(&self, id: &str, expected: u64) -> Result<Deleted> {
        let id = entry_id(id)?;
        let mut db = self.lock();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(row) = fetch(
            &tx,
            "SELECT id,name,description,revision,dek_version,ciphertext,created_at,updated_at FROM variables WHERE account=?1 AND id=?2",
            params![self.account, id],
        )?
        else {
            return match tombstone(&tx, &self.account, &id)? {
                Some((name, version)) if version == expected + 1 => Ok(Deleted {
                    id,
                    name,
                    version,
                    replayed: true,
                }),
                Some(_) => Err(deleted()),
                None => Err(missing(&tx, &self.account, &id)?),
            };
        };
        if row.revision != expected {
            return Err(Error::Conflict(format!(
                "{} changed since it was loaded (it is now version {}). Inspect it before deleting.",
                row.name, row.revision
            )));
        }
        let version = row.revision + 1;
        tx.execute(
            "INSERT INTO tombstones(account,id,name,revision,deleted_at) VALUES (?1,?2,?3,?4,?5)",
            params![self.account, id, row.name, version as i64, now()],
        )?;
        tx.execute(
            "DELETE FROM variables WHERE account=?1 AND id=?2",
            params![self.account, id],
        )?;
        tx.commit()?;
        Ok(Deleted {
            id,
            name: row.name,
            version,
            replayed: false,
        })
    }

    /// Record an action without any value. Best effort: a full disk never blocks the action's
    /// own result, but the caller records the outcome after it is known.
    pub fn audit(&self, action: &str, entry: Option<&str>, outcome: &str) {
        let db = self.lock();
        let portal = meta_text(&db, "portal_account").ok().flatten();
        let _ = db.execute(
            "INSERT INTO variable_audit(account,portal_account,at,action,entry,outcome) VALUES (?1,?2,?3,?4,?5,?6)",
            params![self.account, portal, now(), action, entry, outcome],
        );
        let _ = db.execute(
            "DELETE FROM variable_audit WHERE account=?1 AND seq <= (SELECT MAX(seq) FROM variable_audit WHERE account=?1) - ?2",
            params![self.account, AUDIT_KEPT],
        );
    }

    /// The newest audit events of this account, newest first.
    pub fn audit_events(&self, limit: usize) -> Result<Vec<AuditEvent>> {
        let db = self.lock();
        let mut statement = db.prepare(
            "SELECT at,COALESCE(portal_account,account),action,entry,outcome FROM variable_audit WHERE account=?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![self.account, limit as i64], |r| {
            Ok(AuditEvent {
                at: r.get(0)?,
                account: r.get(1)?,
                action: r.get(2)?,
                entry: r.get(3)?,
                outcome: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn plaintext(&self, db: &Connection, row: &Row) -> Result<Secret> {
        let key = self.unwrap_data_key(db, row.dek_version)?;
        let plain = open(
            &key,
            &record_aad(
                &self.account,
                &row.id,
                &row.name,
                row.revision,
                row.dek_version,
            ),
            &row.ciphertext,
        )
        .ok_or_else(|| integrity(&row.name))?;
        String::from_utf8(plain)
            .map(Secret)
            .map_err(|_| integrity(&row.name))
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

/// One stored entry as read from the database.
struct Row {
    id: String,
    name: String,
    description: String,
    revision: u64,
    dek_version: u32,
    ciphertext: Vec<u8>,
    created_at: i64,
    updated_at: i64,
}
impl Row {
    fn metadata(&self) -> Metadata {
        Metadata {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            version: self.revision,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
fn fetch(db: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Option<Row>> {
    Ok(db
        .query_row(sql, args, |r| {
            Ok(Row {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                revision: r.get::<_, i64>(3)? as u64,
                dek_version: r.get::<_, i64>(4)? as u32,
                ciphertext: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
            })
        })
        .optional()?)
}
/// An entry ID is a UUID; anything else is malformed and never reaches the database.
fn entry_id(id: &str) -> Result<String> {
    Ok(uuid::Uuid::parse_str(id)
        .map_err(|_| input("The entry ID must be a UUID."))?
        .hyphenated()
        .to_string())
}
/// The name and deletion version of a deleted entry.
fn tombstone(db: &Connection, account: &str, id: &str) -> Result<Option<(String, u64)>> {
    Ok(db
        .query_row(
            "SELECT name,revision FROM tombstones WHERE account=?1 AND id=?2",
            params![account, id],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)),
        )
        .optional()?)
}
fn deleted() -> Error {
    Error::Conflict(
        "This variable was deleted. Refresh the list; create a new entry to store it again.".into(),
    )
}
/// Why an entry is absent: deleted earlier, or never an entry of this account.
fn missing(db: &Connection, account: &str, id: &str) -> Result<Error> {
    Ok(if tombstone(db, account, id)?.is_some() {
        deleted()
    } else {
        input("There is no stored variable with that entry ID in this account.")
    })
}

fn connect(path: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    crate::security::protect_file(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
        PRAGMA secure_delete=ON;
        CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS account_keys (
            account TEXT NOT NULL, version INTEGER NOT NULL, master_version INTEGER NOT NULL,
            wrapped BLOB NOT NULL, active INTEGER NOT NULL, created_at INTEGER NOT NULL,
            PRIMARY KEY (account, version));
        CREATE TABLE IF NOT EXISTS variables (
            account TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL, name_key TEXT NOT NULL,
            description TEXT NOT NULL, revision INTEGER NOT NULL, dek_version INTEGER NOT NULL,
            ciphertext BLOB NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            PRIMARY KEY (account, id), UNIQUE (account, name_key));
        CREATE TABLE IF NOT EXISTS tombstones (
            account TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL, revision INTEGER NOT NULL,
            deleted_at INTEGER NOT NULL, PRIMARY KEY (account, id));
        CREATE TABLE IF NOT EXISTS variable_audit (
            seq INTEGER PRIMARY KEY AUTOINCREMENT, account TEXT NOT NULL, portal_account TEXT,
            at INTEGER NOT NULL, action TEXT NOT NULL, entry TEXT, outcome TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS consumer_bindings (
            account TEXT NOT NULL, consumer TEXT NOT NULL, slot TEXT NOT NULL, entry TEXT NOT NULL,
            bound_at INTEGER NOT NULL, PRIMARY KEY (account, consumer, slot));",
    )?;
    Ok(db)
}
fn meta_text(db: &Connection, key: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .optional()?)
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

mod bindings;
pub use bindings::{Binding, BoundVariable};

#[cfg(test)]
mod tests;
