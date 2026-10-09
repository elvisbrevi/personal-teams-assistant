use super::*;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rand::RngCore;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Account {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub version: String,
    pub enabled: bool,
    #[serde(default)]
    pub current_profile: bool,
}

pub(super) fn username(value: &str) -> Result<String> {
    let name = value.trim().to_ascii_lowercase();
    ensure!(
        (1..=64).contains(&name.len())
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-@".contains(&c))
            && name.as_bytes()[0].is_ascii_alphanumeric(),
        "invalid username"
    );
    Ok(name)
}

pub(super) fn hash_password(password: &str) -> Result<String> {
    ensure!(
        (12..=256).contains(&password.len()) && !password.contains(['\r', '\n']),
        "invalid password: use 12 to 256 characters"
    );
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let salt =
        SaltString::encode_b64(&bytes).map_err(|_| anyhow::anyhow!("invalid password salt"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| anyhow::anyhow!("password hashing failed"))
}

pub(super) fn verify_password(hash: &str, password: &str) -> bool {
    password.len() <= 256
        && PasswordHash::new(hash).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
}

pub(super) fn accounts(root: &Path) -> Result<Vec<Account>> {
    let path = root.join("accounts.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let entries: Vec<Account> = serde_json::from_slice(&fs::read(path)?)?;
    for entry in &entries {
        ensure!(
            uuid::Uuid::parse_str(&entry.id).is_ok_and(|id| id.to_string() == entry.id)
                && username(&entry.username)? == entry.username,
            "invalid web account"
        );
    }
    Ok(entries)
}

pub(super) fn edit_accounts<T>(
    root: &Path,
    edit: impl FnOnce(&mut Vec<Account>) -> Result<T>,
) -> Result<T> {
    security::private_dir(root)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("accounts.lock"))?;
    security::protect_file(&root.join("accounts.lock"))?;
    lock.lock_exclusive()?;
    let mut entries = accounts(root)?;
    let value = edit(&mut entries)?;
    write_private(
        &root.join("accounts.json"),
        &serde_json::to_string(&entries)?,
    )?;
    Ok(value)
}

fn password_stdin() -> Result<String> {
    use std::io::IsTerminal;
    ensure!(
        !std::io::stdin().is_terminal(),
        "input must use a protected pipe for passwords"
    );
    let mut value = String::new();
    std::io::stdin().take(258).read_to_string(&mut value)?;
    Ok(value.trim_end_matches(['\r', '\n']).to_owned())
}

/// Account management is operator-local. There is no public registration or profile selector.
pub(super) fn admin(args: &[String]) -> Result<control::Reply> {
    let root = control::profile_dir()?.join("web");
    let action = args.get(1).map(String::as_str).unwrap_or("");
    let public_account = |account: &Account| {
        json!({"username":account.username,
        "enabled":account.enabled,"current_profile":account.current_profile,
        "profile":profile_dir(&root, account)})
    };
    let data = match action {
        "status" => {
            json!({"settings":settings(&root)?, "accounts":accounts(&root)?.iter().map(public_account).collect::<Vec<_>>(),
            "start":"personal-teams-assistant --web"})
        }
        "configure" => {
            let mut text = String::new();
            std::io::stdin().take(16_385).read_to_string(&mut text)?;
            ensure!(text.len() <= 16_384, "input is too large");
            let config: Settings = serde_json::from_str(&text).context("invalid web settings")?;
            config.validate()?;
            security::private_dir(&root)?;
            write_private(
                &root.join("settings.json"),
                &serde_json::to_string(&config)?,
            )?;
            json!({"configured":true,"restart_portal":true,"settings":config})
        }
        "users" => {
            let operation = args.get(2).map(String::as_str).unwrap_or("");
            if operation == "list" {
                json!(
                    accounts(&root)?
                        .iter()
                        .map(public_account)
                        .collect::<Vec<_>>()
                )
            } else {
                let name = username(args.get(3).context("missing username")?)?;
                let hash = if matches!(operation, "add" | "password") {
                    Some(hash_password(&password_stdin()?)?)
                } else {
                    None
                };
                edit_accounts(&root, |entries| {
                    if operation == "add" {
                        ensure!(
                            entries.iter().all(|entry| entry.username != name),
                            "invalid input: username already exists"
                        );
                        ensure!(entries.len() < 100, "invalid input: account limit reached");
                        let current_profile = args.iter().any(|a| a == "--current-profile");
                        ensure!(
                            !current_profile || entries.iter().all(|e| !e.current_profile),
                            "invalid input: current profile already has an account"
                        );
                        let entry = Account {
                            id: uuid::Uuid::new_v4().to_string(),
                            username: name,
                            password_hash: hash.unwrap(),
                            version: uuid::Uuid::new_v4().to_string(),
                            enabled: true,
                            current_profile,
                        };
                        if !current_profile {
                            create_profile(&root, &entry)?;
                        }
                        let value = public_account(&entry);
                        entries.push(entry);
                        Ok(value)
                    } else {
                        let entry = entries
                            .iter_mut()
                            .find(|e| e.username == name)
                            .context("unknown username")?;
                        match operation {
                            "password" => {
                                entry.password_hash = hash.unwrap();
                            }
                            "disable" => {
                                entry.enabled = false;
                            }
                            "enable" => {
                                entry.enabled = true;
                            }
                            _ => anyhow::bail!("unknown web user operation"),
                        }
                        entry.version = uuid::Uuid::new_v4().to_string();
                        Ok(public_account(entry))
                    }
                })?
            }
        }
        _ => anyhow::bail!("unknown web command"),
    };
    Ok(control::Reply::success(data))
}
