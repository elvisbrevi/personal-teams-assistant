//! Native side of browser-assisted machine sign-in. The verifier stays in host memory;
//! GUI, CLI and MCP receive only the coordinator origin, public approval URL, code and state.
use super::*;

pub(in crate::app) struct Pending {
    origin: String,
    endpoint: url::Url,
    transaction: String,
    verifier: String,
    code: String,
    expires_at: u64,
    expected_account: Option<u64>,
}
impl Pending {
    fn approval_url(&self) -> String {
        format!("{}/login?enroll={}", self.origin, self.transaction)
    }
    fn public(&self, state: &str) -> Value {
        json!({"state":state,"coordinator":self.origin,"approval_url":self.approval_url(),
            "code":self.code,"expires_at":self.expires_at})
    }
}

/// Installation-local identity and the last coordinator this installation used.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Local {
    key: String,
    coordinator: String,
}
fn local(root: &Path) -> Result<Local> {
    let path = root.join("installation.json");
    Ok(if path.exists() {
        serde_json::from_slice(&fs::read(path)?)?
    } else {
        Local::default()
    })
}
fn save_local(root: &Path, value: &Local) -> Result<()> {
    write_private(
        &root.join("installation.json"),
        &serde_json::to_string(value)?,
    )
}
/// Hash of this installation's private key: re-enrollment proves the same installation
/// without exposing the key, and another computer cannot claim this machine ID.
pub(super) fn installation(root: &Path) -> Result<String> {
    let mut value = local(root)?;
    if value.key.is_empty() {
        value.key = security::random_secret();
        save_local(root, &value)?;
    }
    Ok(digest(&value.key))
}
pub(super) fn retained(root: &Path) -> Option<String> {
    local(root)
        .ok()
        .map(|value| value.coordinator)
        .filter(|origin| !origin.is_empty())
}
fn retain(root: &Path, origin: &str) -> Result<()> {
    let mut value = local(root)?;
    if value.coordinator != origin {
        value.coordinator = origin.into();
        save_local(root, &value)?;
    }
    Ok(())
}
fn normalize(value: &str) -> Result<String> {
    Ok(fleet::endpoint(value.trim())?
        .origin()
        .ascii_serialization())
}
fn unix_now() -> u64 {
    now().max(0) as u64
}

const DENIED: &str = "The sign-in was denied in the browser. Nothing was trusted; start again if that was a mistake.";
const EXPIRED: &str =
    "The sign-in expired, was cancelled or was already used. Nothing was trusted; start again.";
const MISMATCH: &str = "The approving account is not the account this computer belongs to. Nothing changed; sign in with that account, or disconnect the computer first.";

async fn post(endpoint: &url::Url, body: Value) -> Result<Value> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?
        .post(endpoint.clone())
        .json(&body)
        .send()
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "The coordinator could not be reached. Check its HTTPS address and your connection."
            )
        })?;
    match response.status() {
        status if status.is_success() => {}
        StatusCode::TOO_MANY_REQUESTS => anyhow::bail!(
            "The coordinator is receiving too many sign-ins. Wait a minute and try again."
        ),
        StatusCode::FORBIDDEN => anyhow::bail!(EXPIRED),
        StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            if body["action"] == "login_start" =>
        {
            anyhow::bail!(
                "This coordinator does not support browser sign-in yet. Update it, or ask its operator for a pairing file."
            )
        }
        _ => anyhow::bail!("The coordinator is unavailable. Try again later."),
    }
    let reply: Value = crate::adapters::bounded_json(response, 16_000)
        .await
        .map_err(|_| anyhow::anyhow!("The coordinator returned an invalid reply."))?;
    if reply["ok"] != true {
        // The coordinator's own fixed messages explain a refused trust or registration.
        let message: String = reply["message"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(300)
            .collect();
        anyhow::bail!("The coordinator refused this sign-in. {message}");
    }
    Ok(reply["data"].clone())
}

/// Start (or reuse) one pending sign-in for this installation.
pub(super) async fn begin(host: &Arc<Host>, args: &Value) -> Result<Value> {
    let state = &host.state.implementation;
    let connection = state.connection()?;
    let requested = args["url"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let origin = match (requested, &connection) {
        (Some(url), _) => normalize(url)?,
        (None, Some(connection)) => normalize(&connection.url)?,
        (None, None) => {
            normalize(&retained(&state.root).context("Enter your coordinator's HTTPS address.")?)?
        }
    };
    if let Some(connection) = &connection {
        ensure!(
            normalize(&connection.url)? == origin,
            "This computer is connected to another coordinator. Disconnect it before signing in to a different one."
        );
    } else {
        ensure!(
            host.state.running.lock().await.is_none(),
            "Stop the Teams assistant on this computer before adding it to an account."
        );
    }
    let endpoint = fleet::endpoint(&origin)?;
    begin_at(
        host,
        &origin,
        endpoint,
        connection.map(|c| c.account),
        args["open"] == true,
    )
    .await
}

pub(in crate::app) async fn begin_at(
    host: &Arc<Host>,
    origin: &str,
    endpoint: url::Url,
    expected_account: Option<u64>,
    open: bool,
) -> Result<Value> {
    let state = &host.state.implementation;
    let (id, name) = {
        let data = state.registry.lock().await;
        (data.id.clone(), data.settings.name.clone())
    };
    let mut pending = state.login.lock().await;
    let reusable = pending.as_ref().is_some_and(|p| {
        p.origin == origin
            && p.expected_account == expected_account
            && p.expires_at > unix_now() + 30
    });
    if !reusable {
        if let Some(previous) = pending.take() {
            let _ = post(
                &previous.endpoint,
                json!({"action":"login_cancel","id":id,"transaction":previous.transaction,"verifier":previous.verifier}),
            )
            .await;
        }
        let verifier = security::random_secret();
        let reply = post(
            &endpoint,
            json!({"action":"login_start","id":id,"name":name,"os":std::env::consts::OS,
                "installation":installation(&state.root)?,"challenge":digest(&verifier),
                "expected_account":expected_account}),
        )
        .await?;
        let transaction = reply["transaction"]
            .as_str()
            .filter(|t| t.len() == 64 && t.bytes().all(|c| c.is_ascii_hexdigit()))
            .context("The coordinator returned an invalid sign-in.")?;
        let code = reply["code"]
            .as_str()
            .filter(|c| {
                c.len() == 9
                    && c.bytes().enumerate().all(|(i, b)| {
                        if i == 4 {
                            b == b'-'
                        } else {
                            b.is_ascii_digit() || (b'A'..=b'F').contains(&b)
                        }
                    })
            })
            .context("The coordinator returned an invalid sign-in.")?;
        let expires_at = reply["expires_at"]
            .as_u64()
            .filter(|at| (unix_now()..=unix_now() + 3600).contains(at))
            .context("The coordinator returned an invalid sign-in.")?;
        retain(&state.root, origin)?;
        *pending = Some(Pending {
            origin: origin.into(),
            endpoint,
            transaction: transaction.into(),
            verifier,
            code: code.into(),
            expires_at,
            expected_account,
        });
    }
    let current = pending.as_ref().unwrap();
    let opened =
        open && !host.shell.headless() && host.shell.open_url(&current.approval_url()).is_ok();
    let mut value = current.public("pending");
    value["opened"] = json!(opened);
    value["machine"] = json!({"id":id,"name":name,"os":std::env::consts::OS});
    Ok(value)
}

/// Read the remote state without finishing. Terminal states clear the local request.
pub(in crate::app) async fn status(host: &Arc<Host>) -> Result<Value> {
    let state = &host.state.implementation;
    let id = state.registry.lock().await.id.clone();
    let mut pending = state.login.lock().await;
    let Some(current) = pending.as_ref() else {
        let connection = state.connection()?;
        return Ok(json!({"state":"none","connected":connection.is_some(),
            "coordinator":connection.map(|c| c.url).or_else(|| retained(&state.root))}));
    };
    if current.expires_at <= unix_now() {
        pending.take();
        return Ok(json!({"state":"expired","message":EXPIRED}));
    }
    let reply = match post(
        &current.endpoint,
        json!({"action":"login_status","id":id,"transaction":current.transaction,"verifier":current.verifier}),
    )
    .await
    {
        Ok(reply) => reply,
        Err(error) if error.to_string() == EXPIRED => json!({"state":"expired"}),
        Err(error) => return Err(error),
    };
    match reply["state"].as_str().unwrap_or_default() {
        state @ ("pending" | "approved") => Ok(current.public(state)),
        "denied" => {
            pending.take();
            Ok(json!({"state":"denied","message":DENIED}))
        }
        _ => {
            pending.take();
            Ok(json!({"state":"expired","message":EXPIRED}))
        }
    }
}

pub(super) async fn finish(host: &Arc<Host>) -> Result<Value> {
    finish_with(host, |token| {
        security::put_desktop_secret("default", "PTA_MACHINE_TOKEN", token)
    })
    .await
}

/// Finish only with the verifier this installation kept. Failures keep any previous
/// connection unchanged; the new credential is stored before the connection is switched.
pub(in crate::app) async fn finish_with(
    host: &Arc<Host>,
    store: impl FnOnce(&str) -> Result<()>,
) -> Result<Value> {
    let state = &host.state.implementation;
    let id = state.registry.lock().await.id.clone();
    let mut pending = state.login.lock().await;
    let current = pending
        .as_ref()
        .context("Start signing in to your coordinator first.")?;
    let reply = match post(
        &current.endpoint,
        json!({"action":"login_finish","id":id,"transaction":current.transaction,"verifier":current.verifier}),
    )
    .await
    {
        Ok(reply) => reply,
        Err(error) => {
            // A refused registration consumed the approval; a network failure keeps it.
            let text = error.to_string();
            if text == EXPIRED || text.starts_with("The coordinator refused") {
                pending.take();
            }
            return Err(error);
        }
    };
    match reply["state"].as_str() {
        Some("pending") => {
            let mut value = current.public("pending");
            value["pending"] = json!(true);
            return Ok(value);
        }
        Some("denied") => {
            pending.take();
            anyhow::bail!(DENIED);
        }
        Some(_) => {
            pending.take();
            anyhow::bail!(EXPIRED);
        }
        None => {}
    }
    let current = pending.take().unwrap();
    let token = reply["token"]
        .as_str()
        .filter(|t| (32..=128).contains(&t.len()))
        .context("The coordinator returned an invalid machine credential.")?;
    let account = reply["account"]
        .as_u64()
        .context("The coordinator returned an invalid account.")?;
    if let Some(expected) = current.expected_account {
        ensure!(account == expected, MISMATCH);
    }
    store(token)?;
    {
        let mut data = state.registry.lock().await;
        state.reset_roots(&mut data)?;
    }
    write_private(
        &state.root.join("connection.json"),
        &serde_json::to_string(&Connection {
            url: current.origin.clone(),
            account,
            id,
        })?,
    )?;
    retain(&state.root, &current.origin)?;
    Ok(
        json!({"connected":true,"roots_reset":true,"coordinator":current.origin,
        "username":reply["username"].as_str().unwrap_or_default()}),
    )
}

pub(in crate::app) async fn cancel(host: &Arc<Host>) -> Result<Value> {
    let state = &host.state.implementation;
    let id = state.registry.lock().await.id.clone();
    if let Some(previous) = state.login.lock().await.take() {
        let _ = post(
            &previous.endpoint,
            json!({"action":"login_cancel","id":id,"transaction":previous.transaction,"verifier":previous.verifier}),
        )
        .await;
    }
    Ok(json!({"cancelled":true}))
}
