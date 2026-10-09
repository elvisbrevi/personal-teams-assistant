use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Account {
    pub id: String,
    pub username: String,
    // Preserve legacy hashes during schema migration. No authentication uses them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password_hash: String,
    pub version: String,
    pub enabled: bool,
    #[serde(default)]
    pub current_profile: bool,
    #[serde(default)]
    pub access_binding: Option<access::Binding>,
    #[serde(default)]
    pub access_valid_after: u64,
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
pub(super) fn accounts(root: &Path) -> Result<Vec<Account>> {
    let path = root.join("accounts.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let entries: Vec<Account> = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(entries.len() <= 100, "invalid web account count");
    for (i, entry) in entries.iter().enumerate() {
        ensure!(
            access::canonical_uuid(&entry.id) && username(&entry.username)? == entry.username,
            "invalid web account"
        );
        if let Some(binding) = &entry.access_binding {
            binding.validate()?;
        }
        ensure!(
            entries[..i].iter().all(|other| other.id != entry.id
                && other.username != entry.username
                && !(other.current_profile && entry.current_profile)
                && (entry.access_binding.is_none()
                    || other.access_binding != entry.access_binding)),
            "duplicate web account or Access binding"
        );
    }
    Ok(entries)
}
fn file_lock(root: &Path, name: &str) -> Result<fs::File> {
    security::private_dir(root)?;
    let path = root.join(name);
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    security::protect_file(&path)?;
    lock.lock_exclusive()?;
    Ok(lock)
}
pub(super) fn edit_accounts<T>(
    root: &Path,
    edit: impl FnOnce(&mut Vec<Account>) -> Result<T>,
) -> Result<T> {
    let _lock = file_lock(root, "accounts.lock")?;
    let mut entries = accounts(root)?;
    let value = edit(&mut entries)?;
    write_private(
        &root.join("accounts.json"),
        &serde_json::to_string(&entries)?,
    )?;
    Ok(value)
}
fn json_stdin<T: serde::de::DeserializeOwned>() -> Result<T> {
    let mut text = String::new();
    std::io::stdin().take(16_385).read_to_string(&mut text)?;
    ensure!(text.len() <= 16_384, "input is too large");
    serde_json::from_str(&text).context("invalid JSON input")
}
fn revocations(root: &Path) -> Result<HashMap<String, u64>> {
    let path = root.join("access-revocations.json");
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let entries: HashMap<String, u64> = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        entries.len() <= 5000
            && entries
                .keys()
                .all(|key| key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit())),
        "invalid Access revocation file"
    );
    Ok(entries)
}
pub(super) fn access_revoked(root: &Path, identity: &access::Verified) -> Result<bool> {
    Ok(revocations(root)?
        .get(&identity.token_hash)
        .is_some_and(|exp| *exp > access::now()))
}
pub(super) fn revoke_access(root: &Path, identity: &access::Verified) -> Result<()> {
    let _lock = file_lock(root, "access-revocations.lock")?;
    let mut entries = revocations(root)?;
    entries.retain(|_, exp| *exp > access::now());
    ensure!(
        entries.len() < 5000 || entries.contains_key(&identity.token_hash),
        "Access revocation limit reached"
    );
    entries.insert(identity.token_hash.clone(), identity.expires_at);
    write_private(
        &root.join("access-revocations.json"),
        &serde_json::to_string(&entries)?,
    )
}
pub(super) fn callback_path(value: &str) -> bool {
    let segments: Vec<_> = value.split('/').collect();
    matches!(segments.as_slice(), ["", "webhooks", id, "graph", "notifications" | "lifecycle"] if access::canonical_uuid(id))
}
fn callbacks(root: &Path) -> Result<Vec<String>> {
    Ok(accounts(root)?
        .iter()
        .filter(|e| !e.current_profile)
        .flat_map(|e| {
            [
                format!("/webhooks/{}/graph/notifications", e.id),
                format!("/webhooks/{}/graph/lifecycle", e.id),
            ]
        })
        .collect())
}

/// Only the local OS operator can associate identities, recover accounts or revoke
/// sessions. A GitHub login never creates or selects a local profile.
pub(super) fn admin(args: &[String]) -> Result<control::Reply> {
    let root = control::profile_dir()?.join("web");
    let action = args.get(1).map(String::as_str).unwrap_or("");
    let public_account = |account: &Account| {
        json!({"id":account.id,"username":account.username,
        "enabled":account.enabled,"current_profile":account.current_profile,
        "access_binding":account.access_binding,"profile":profile_dir(&root, account)})
    };
    let data = match action {
        "status" => {
            json!({"settings":settings(&root)?,"access_configured":settings(&root)?.access.is_some(),
            "accounts":accounts(&root)?.iter().map(public_account).collect::<Vec<_>>(),"start":"personal-teams-assistant --web"})
        }
        "callbacks" => {
            json!({"hostname":url::Url::parse(&settings(&root)?.public_url)?.host_str(),"paths":callbacks(&root)?})
        }
        "configure" => {
            let config: Settings = json_stdin()?;
            config.validate()?;
            ensure!(
                config.access.is_some(),
                "invalid web settings: Cloudflare Access configuration is required"
            );
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
                // Older desktop/profile hosts reject additional account fields.
                // Finish their controlled transition before changing this schema.
                let mut directories: Vec<_> = accounts(&root)?
                    .iter()
                    .map(|e| profile_dir(&root, e))
                    .collect();
                directories.push(root.parent().unwrap().to_path_buf());
                for dir in directories {
                    if control::existing_host_at(&dir).is_ok() {
                        control::require_web_access_host_at(&dir)?;
                    }
                }
                let name = username(args.get(3).context("missing username")?)?;
                let binding = if operation == "bind" {
                    let config = settings(&root)?
                        .access
                        .context("configure Access before associating an identity")?;
                    let identity: access::Identity = json_stdin()?;
                    Some(identity.binding(&config)?)
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
                            password_hash: String::new(),
                            version: uuid::Uuid::new_v4().to_string(),
                            enabled: true,
                            current_profile,
                            access_binding: None,
                            access_valid_after: 0,
                        };
                        if !current_profile {
                            create_profile(&root, &entry)?;
                        }
                        let value = public_account(&entry);
                        entries.push(entry);
                        Ok(value)
                    } else {
                        if let Some(binding) = &binding {
                            ensure!(
                                entries.iter().all(|e| e.username == name
                                    || e.access_binding.as_ref() != Some(binding)),
                                "invalid input: identity already belongs to another account"
                            );
                        }
                        let entry = entries
                            .iter_mut()
                            .find(|e| e.username == name)
                            .context("unknown username")?;
                        match operation {
                            "bind" => entry.access_binding = binding,
                            "unbind" => entry.access_binding = None,
                            "disable" => entry.enabled = false,
                            "enable" => entry.enabled = true,
                            "revoke" => entry.access_valid_after = access::now() + 1,
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
