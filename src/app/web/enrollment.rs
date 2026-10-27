//! Browser-assisted machine sign-in. A native installation opens an expiring,
//! installation-bound request and keeps its verifier; the account owner approves it
//! once in an authenticated browser session; only the verifier holder can finish.
use super::*;

const LIFETIME: u64 = 600;
const LIMIT: usize = 100;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    machine_id: String,
    name: String,
    os: String,
    /// SHA-256 of the installation's local identity key, never the key itself.
    installation: String,
    /// SHA-256 of the verifier held by the initiating installation.
    challenge: String,
    /// Provider subject of the account this installation already belongs to.
    #[serde(default)]
    expected_account: Option<String>,
    expires_at: u64,
    #[serde(default)]
    approval: Option<Approval>,
    #[serde(default)]
    denied: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    account_id: String,
    version: String,
}

pub(super) enum Poll {
    State(&'static str),
    Register(Account, Value),
}

fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
/// The approval URL carries this public handle; it is not a credential by itself.
pub(super) fn transaction(value: &str) -> Option<&str> {
    hex64(value).then_some(value)
}
/// Short code shown on both the installation and the approval page.
fn code(transaction: &str) -> String {
    let digest = hash(transaction).to_ascii_uppercase();
    format!("{}-{}", &digest[..4], &digest[4..8])
}
fn logins(root: &Path) -> Result<HashMap<String, Login>> {
    let path = root.join("machine-logins.json");
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let entries: HashMap<String, Login> = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(entries.len() <= LIMIT, "invalid machine sign-in count");
    Ok(entries)
}
fn save(root: &Path, entries: &HashMap<String, Login>) -> Result<()> {
    write_private(
        &root.join("machine-logins.json"),
        &serde_json::to_string(entries)?,
    )
}
fn edit<T>(root: &Path, change: impl FnOnce(&mut HashMap<String, Login>) -> T) -> Result<T> {
    let _lock = store::file_lock(root, "machine-logins.lock")?;
    let mut entries = logins(root)?;
    entries.retain(|_, login| login.expires_at > access::now());
    let value = change(&mut entries);
    save(root, &entries)?;
    Ok(value)
}
fn owner_matches(login: &Login, account: &Account) -> bool {
    login.expected_account.as_ref().is_none_or(|expected| {
        account
            .access_binding
            .as_ref()
            .is_some_and(|binding| &binding.provider_user_id == expected)
    })
}
const MISMATCH: &str = "This computer is already connected to another account. Sign in with that account, or disconnect the computer before adding it to this one.";
const UNAVAILABLE: &str =
    "This sign-in request expired or was already used. Start again on the computer.";

/// Unauthenticated start: it records a pending request and grants nothing.
pub(super) fn start(root: &Path, input: &Value) -> Result<Value> {
    let id = input["id"].as_str().unwrap_or_default();
    let name = input["name"].as_str().unwrap_or_default();
    let os = input["os"].as_str().unwrap_or_default();
    let installation = input["installation"].as_str().unwrap_or_default();
    let challenge = input["challenge"].as_str().unwrap_or_default();
    let expected_account = match input.get("expected_account") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|id| *id > 0)
                .context("invalid machine sign-in")?
                .to_string(),
        ),
    };
    ensure!(
        access::canonical_uuid(id)
            && !name.trim().is_empty()
            && name.len() <= 100
            && !name.chars().any(char::is_control)
            && ["linux", "macos", "windows"].contains(&os)
            && hex64(installation)
            && hex64(challenge),
        "invalid machine sign-in"
    );
    let transaction = hash(&security::random_secret());
    let expires_at = access::now() + LIFETIME;
    edit(root, |entries| {
        // A restarted sign-in from the same installation replaces its own request only.
        entries.retain(|_, login| !(login.machine_id == id && login.installation == installation));
        ensure!(
            entries.len() < LIMIT,
            "Too many pending machine sign-ins. Try again in a few minutes."
        );
        entries.insert(
            hash(&transaction),
            Login {
                machine_id: id.into(),
                name: name.into(),
                os: os.into(),
                installation: installation.into(),
                challenge: challenge.into(),
                expected_account,
                expires_at,
                approval: None,
                denied: false,
            },
        );
        Ok(())
    })??;
    Ok(json!({"transaction":transaction,"code":code(&transaction),"expires_at":expires_at}))
}

/// What the signed-in browser user is asked to trust.
pub(super) fn inspect(root: &Path, account: &Account, transaction: &str) -> Result<Value> {
    edit(root, |entries| {
        let login = entries
            .get(&hash(transaction))
            .filter(|login| login.approval.is_none() && !login.denied)
            .context(UNAVAILABLE)?;
        ensure!(owner_matches(login, account), MISMATCH);
        Ok(
            json!({"machine":{"id":login.machine_id,"name":login.name,"os":login.os},
            "code":code(transaction),"expires_at":login.expires_at,"username":account.username}),
        )
    })?
}

/// One explicit decision, bound to the session's verified account and version.
pub(super) fn decide(
    root: &Path,
    account: &Account,
    transaction: &str,
    trust: bool,
) -> Result<Value> {
    edit(root, |entries| {
        let login = entries
            .get_mut(&hash(transaction))
            .filter(|login| login.approval.is_none() && !login.denied)
            .context(UNAVAILABLE)?;
        if !trust {
            login.denied = true;
            return Ok(json!({"denied":true}));
        }
        ensure!(owner_matches(login, account), MISMATCH);
        login.approval = Some(Approval {
            account_id: account.id.clone(),
            version: account.version.clone(),
        });
        Ok(
            json!({"trusted":true,"machine":{"id":login.machine_id,"name":login.name,"os":login.os}}),
        )
    })?
}

/// Only the initiating installation (verifier and machine ID) can read or finish its request.
/// Finishing consumes it before any host is contacted, so a lost reply cannot be replayed.
pub(super) fn poll(
    root: &Path,
    config: &Settings,
    input: &Value,
    finish: bool,
) -> Result<Option<Poll>> {
    let (Some(transaction), Some(verifier), Some(id)) = (
        input["transaction"].as_str().and_then(transaction),
        input["verifier"]
            .as_str()
            .filter(|v| (8..=128).contains(&v.len())),
        input["id"].as_str(),
    ) else {
        return Ok(None);
    };
    let accounts = accounts(root)?;
    edit(root, |entries| {
        let key = hash(transaction);
        let Some(login) = entries.get(&key) else {
            return Some(Poll::State("expired"));
        };
        if login.machine_id != id || !security::constant_eq(&login.challenge, &hash(verifier)) {
            return None;
        }
        if login.denied {
            entries.remove(&key);
            return Some(Poll::State("denied"));
        }
        let Some(approval) = login.approval.clone() else {
            return Some(Poll::State("pending"));
        };
        let account = accounts.iter().find(|a| {
            config.admits(a) && a.id == approval.account_id && a.version == approval.version
        });
        let Some(account) = account.filter(|a| owner_matches(login, a)).cloned() else {
            entries.remove(&key);
            return Some(Poll::State("unavailable"));
        };
        if !finish {
            return Some(Poll::State("approved"));
        }
        let login = entries.remove(&key).unwrap();
        Some(Poll::Register(
            account,
            json!({"id":login.machine_id,"name":login.name,"os":login.os,
                "installation":login.installation,"enrollment":"login"}),
        ))
    })
}

pub(super) fn cancel(root: &Path, input: &Value) -> Result<bool> {
    let (Some(transaction), Some(verifier), Some(id)) = (
        input["transaction"].as_str().and_then(transaction),
        input["verifier"].as_str(),
        input["id"].as_str(),
    ) else {
        return Ok(false);
    };
    edit(root, |entries| {
        let key = hash(transaction);
        let owned = entries.get(&key).is_some_and(|login| {
            login.machine_id == id && security::constant_eq(&login.challenge, &hash(verifier))
        });
        if owned {
            entries.remove(&key);
        }
        owned
    })
}

/// Machine transport actions that need no machine credential yet.
pub(super) async fn machine(portal: &Arc<Portal>, input: Value) -> Response {
    let reply = |result: Result<Value>| match result {
        Ok(value) => Json(control::Reply::success(value)).into_response(),
        Err(error) => Json(control::Reply::error(
            "invalid_input",
            2,
            &error.to_string(),
        ))
        .into_response(),
    };
    match input["action"].as_str().unwrap_or_default() {
        "login_start" => {
            let id = input["id"].as_str().unwrap_or_default();
            if !access::canonical_uuid(id) {
                return StatusCode::BAD_REQUEST.into_response();
            }
            if !portal
                .limit(&[("machine-login", 30), (&format!("machine-login:{id}"), 5)])
                .await
            {
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            }
            match start(&portal.root, &input) {
                Err(error) if error.to_string() == "invalid machine sign-in" => {
                    StatusCode::BAD_REQUEST.into_response()
                }
                result => reply(result),
            }
        }
        action @ ("login_status" | "login_finish") => {
            match poll(
                &portal.root,
                &portal.settings,
                &input,
                action == "login_finish",
            ) {
                Ok(Some(Poll::State(state))) => reply(Ok(json!({"state":state}))),
                Ok(Some(Poll::Register(account, args))) => {
                    super::register(portal, account, args).await
                }
                Ok(None) => StatusCode::FORBIDDEN.into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        "login_cancel" => match cancel(&portal.root, &input) {
            Ok(true) => reply(Ok(json!({"cancelled":true}))),
            Ok(false) => StatusCode::FORBIDDEN.into_response(),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// Authenticated browser decision. Opening the portal alone never reaches this endpoint.
pub(super) async fn browser(
    State(portal): State<Arc<Portal>>,
    Extension(identity): Extension<access::Verified>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Response {
    let (account, _) = match portal.authenticate(&headers, &identity, true).await {
        Ok(value) => value,
        Err(status) => return status.into_response(),
    };
    let Some(transaction) = input["transaction"].as_str().and_then(transaction) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let result = match input["action"].as_str().unwrap_or_default() {
        "inspect" => inspect(&portal.root, &account, transaction),
        "trust" => decide(&portal.root, &account, transaction, true),
        "deny" => decide(&portal.root, &account, transaction, false),
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    Json(match result {
        Ok(value) => control::Reply::success(value),
        Err(error) => control::Reply::error("invalid_input", 2, &error.to_string()),
    })
    .into_response()
}
