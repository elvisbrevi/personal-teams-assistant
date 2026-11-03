//! This installation's link to the account store Worker (`cloud/`, phase 3a of the cloud storage
//! design): enrollment and status. Nothing is uploaded yet and the mode stays `off`; the outbox
//! (phase 3b) and the migration (phase 4a) use this link.
//!
//! Two credentials reach the Worker: the account's Access service token at the edge
//! (`CLOUD_ACCESS_CLIENT_ID` / `CLOUD_ACCESS_CLIENT_SECRET`) and the installation credential the
//! Worker issued at enrollment (`CLOUD_STORE_TOKEN`). All three stay in this installation's
//! credential store; `[cloud]` keeps only the endpoint and the installation ID.
use super::{
    authority,
    control::{self, Reply},
    *,
};
use crate::config::{Cloud, CloudMode};
use serde_json::{Value, json};
use std::time::Duration;

pub(super) const ACCESS_ID: &str = "CLOUD_ACCESS_CLIENT_ID";
pub(super) const ACCESS_SECRET: &str = "CLOUD_ACCESS_CLIENT_SECRET";
pub(super) const TOKEN: &str = "CLOUD_STORE_TOKEN";
/// The installation-local credentials of the link, listed (never required) by `credentials list`.
pub(super) const CREDENTIALS: [&str; 3] = [ACCESS_ID, ACCESS_SECRET, TOKEN];
/// The protocol version this host speaks.
const PROTOCOL: u64 = 1;
const LABEL_CHARS: usize = 80;

pub(super) fn handles(method: &str) -> bool {
    matches!(method, "cloud_status" | "cloud_connect")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectArgs {
    endpoint: String,
    #[serde(default)]
    label: String,
    /// Both or neither: without them the stored service token is used.
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
}

/// Why an exchange with the Worker did not answer what was asked.
#[derive(Debug, PartialEq)]
enum Failure {
    /// Network, timeout or a server error: the link is offline, nothing is lost.
    Offline,
    /// Access or the Worker refused the credentials.
    Revoked,
    /// The account's daily share is used up; behaves like being offline.
    Quota,
    /// The Worker has no Access settings yet.
    Unconfigured,
    /// Anything else the Worker refused, with its error code.
    Refused(String),
}

impl Failure {
    fn standing(&self) -> &'static str {
        match self {
            Failure::Offline | Failure::Quota => "offline",
            Failure::Revoked => "revoked",
            Failure::Unconfigured => "unconfigured",
            Failure::Refused(_) => "offline",
        }
    }
}

struct Credentials {
    client_id: String,
    client_secret: String,
    token: Option<String>,
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Reply {
    let invocation = match authority::invocation(&host.state) {
        Ok(invocation) => invocation,
        Err(_) => return Reply::error("state_conflict", 6, authority::ACCOUNT_UNAVAILABLE),
    };
    if invocation.transport == authority::Transport::Machine {
        return Reply::error(
            "authority_denied",
            6,
            "This operation is not available from this client.",
        );
    }
    match request.method.as_str() {
        "cloud_status" => {
            if request.args != json!({}) && !request.args.is_null() {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            }
            match status(host).await {
                Ok(data) => Reply::success(data),
                Err(_) => Reply::error("operation_failed", 1, "The cloud link could not be read."),
            }
        }
        "cloud_connect" => {
            // Phase 4b opens the account store to other accounts; until then only the owner.
            if !invocation.scope.owner || invocation.scope.shared {
                return Reply::error("authority_denied", 6, authority::EXECUTION_DENIED);
            }
            let Ok(args) = serde_json::from_value::<ConnectArgs>(request.args.clone()) else {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            };
            connect(host, args).await.unwrap_or_else(|reply| *reply)
        }
        _ => Reply::error("invalid_input", 2, "Unknown operation."),
    }
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!(
            "personal-teams-assistant/",
            env!("CARGO_PKG_VERSION")
        ))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// One request to the Worker; the reply's JSON on success, otherwise why not.
async fn exchange(
    endpoint: &str,
    method: reqwest::Method,
    path: &str,
    credentials: Option<&Credentials>,
    body: Option<Value>,
) -> std::result::Result<Value, Failure> {
    let client = client().map_err(|_| Failure::Offline)?;
    let mut request = client.request(method, format!("{endpoint}{path}"));
    if let Some(credentials) = credentials {
        request = request
            .header("CF-Access-Client-Id", &credentials.client_id)
            .header("CF-Access-Client-Secret", &credentials.client_secret);
        if let Some(token) = &credentials.token {
            request = request.bearer_auth(token);
        }
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.map_err(|_| Failure::Offline)?;
    let status = response.status().as_u16();
    let json = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .is_some_and(|v| v.to_str().is_ok_and(|v| v.starts_with("application/json")));
    // Access answers its own refusals (401/403, HTML); the Worker answers JSON.
    let value: Value = if json {
        crate::adapters::bounded_json(response, 2_000_000)
            .await
            .map_err(|_| Failure::Offline)?
    } else {
        Value::Null
    };
    match status {
        200..=299 if json => Ok(value),
        401 | 403 => Err(Failure::Revoked),
        429 => Err(Failure::Quota),
        503 if value["error"]["code"] == "not_configured" => Err(Failure::Unconfigured),
        400..=499 if json => Err(Failure::Refused(
            value["error"]["code"]
                .as_str()
                .unwrap_or("refused")
                .to_owned(),
        )),
        _ => Err(Failure::Offline),
    }
}

/// The Worker's health, checked against the protocol this host speaks.
async fn health(endpoint: &str) -> std::result::Result<Value, Failure> {
    let health = exchange(endpoint, reqwest::Method::GET, "/v1/health", None, None).await?;
    if health["configured"] != true {
        return Err(Failure::Unconfigured);
    }
    let speaks = health["protocols"]
        .as_array()
        .is_some_and(|p| p.iter().any(|v| v.as_u64() == Some(PROTOCOL)));
    if !speaks {
        return Err(Failure::Refused("incompatible_protocol".into()));
    }
    Ok(health)
}

fn stored_credentials() -> Option<Credentials> {
    Some(Credentials {
        client_id: security::secret(ACCESS_ID).ok()?,
        client_secret: security::secret(ACCESS_SECRET).ok()?,
        token: security::secret(TOKEN).ok(),
    })
}

fn enrolled(cloud: &Cloud) -> bool {
    !cloud.installation.is_empty()
        && CREDENTIALS
            .iter()
            .all(|name| security::secret_source(name).is_ok_and(|s| s.is_some()))
}

fn origin(endpoint: &str) -> String {
    endpoint.trim_end_matches('/').to_owned()
}

/// What this installation knows about its link, checked live against the Worker.
async fn status(host: &Arc<Host>) -> Result<Value> {
    let config = host.state.settings.config()?;
    let cloud = &config.cloud;
    let checked_at = now();
    if cloud.endpoint.is_empty() {
        return Ok(json!({
            "mode": cloud.mode,
            "endpoint": null,
            "enrolled": false,
            "installation": null,
            "standing": "disconnected",
            "checked_at": checked_at,
        }));
    }
    let endpoint = origin(&cloud.endpoint);
    let enrolled = enrolled(cloud);
    let mut view = json!({
        "mode": cloud.mode,
        "endpoint": endpoint,
        "enrolled": enrolled,
        "installation": (!cloud.installation.is_empty()).then_some(&cloud.installation),
        "checked_at": checked_at,
    });
    let health = match health(&endpoint).await {
        Ok(health) => health,
        Err(failure) => {
            view["standing"] = json!(failure.standing());
            view["problem"] = json!(problem(&failure));
            return Ok(view);
        }
    };
    view["worker"] = json!({
        "environment": health["environment"],
        "version": health["version"],
        "deployment": health["deployment"],
        "protocols": health["protocols"],
        "schema": health["schema"],
    });
    let credentials = stored_credentials().filter(|c| c.token.is_some());
    let Some(credentials) = credentials.filter(|_| enrolled) else {
        view["standing"] = json!("not_enrolled");
        return Ok(view);
    };
    let whoami = exchange(
        &endpoint,
        reqwest::Method::GET,
        "/v1/whoami",
        Some(&credentials),
        None,
    )
    .await;
    let store = exchange(
        &endpoint,
        reqwest::Method::GET,
        "/v1/store",
        Some(&credentials),
        None,
    )
    .await;
    match (whoami, store) {
        (Ok(whoami), Ok(store)) => {
            let same = whoami["installation"]["id"] == json!(cloud.installation);
            view["standing"] = json!(if same { "live" } else { "revoked" });
            view["principal"] = json!({
                "label": whoami["installation"]["label"],
                "enrolled_at": whoami["installation"]["enrolled_at"],
                "account_installations": whoami["account"]["installations"],
            });
            view["store"] = store;
        }
        (Err(failure), _) | (_, Err(failure)) => {
            view["standing"] = json!(failure.standing());
            view["problem"] = json!(problem(&failure));
        }
    }
    Ok(view)
}

fn problem(failure: &Failure) -> String {
    match failure {
        Failure::Offline => {
            "The account store did not answer; this installation keeps working locally.".into()
        }
        Failure::Revoked => {
            "The account store refused this installation's credentials. Connect again.".into()
        }
        Failure::Quota => {
            "The account's daily share of the account store is used up; it resets at 00:00 UTC."
                .into()
        }
        Failure::Unconfigured => {
            "The account store's Access settings are missing; see docs/delivery.md.".into()
        }
        Failure::Refused(code) if code == "incompatible_protocol" => {
            "The account store does not speak this host's protocol; update the Worker first.".into()
        }
        Failure::Refused(code) => format!("The account store refused the request ({code})."),
    }
}

fn refused(failure: &Failure) -> Reply {
    match failure {
        Failure::Offline | Failure::Quota => {
            Reply::error("dependency_or_network", 5, &problem(failure))
        }
        Failure::Revoked => Reply::error(
            "not_ready",
            3,
            "Access refused the service token: check its client ID and secret, and that the account store's Access policy lists it.",
        ),
        Failure::Unconfigured | Failure::Refused(_) => {
            Reply::error("not_ready", 3, &problem(failure))
        }
    }
}

fn valid_client_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_client_secret(value: &str) -> bool {
    (16..=512).contains(&value.len()) && value.chars().all(|c| c.is_ascii_graphic())
}

fn boxed(code: &str, exit_code: i32, message: &str) -> Box<Reply> {
    Box::new(Reply::error(code, exit_code, message))
}

/// Enroll this installation with the account store and keep its credentials.
async fn connect(host: &Arc<Host>, args: ConnectArgs) -> std::result::Result<Reply, Box<Reply>> {
    let invalid = |message: &str| Box::new(Reply::error("invalid_input", 2, message));
    Cloud::check_endpoint(args.endpoint.trim()).map_err(|_| {
        invalid("Give the account store's HTTPS origin, like https://store.example.com.")
    })?;
    let endpoint = origin(args.endpoint.trim());
    let label = match args.label.trim() {
        "" => "This installation".to_owned(),
        label => label.to_owned(),
    };
    if label.chars().count() > LABEL_CHARS || label.chars().any(char::is_control) {
        return Err(invalid("The label is one line of up to 80 characters."));
    }
    let given = match (args.client_id, args.client_secret) {
        (Some(id), Some(secret)) => {
            let (id, secret) = (id.trim().to_owned(), secret.trim().to_owned());
            if !valid_client_id(&id) || !valid_client_secret(&secret) {
                return Err(invalid(
                    "The service token's client ID or secret is malformed.",
                ));
            }
            Some(Credentials {
                client_id: id,
                client_secret: secret,
                token: None,
            })
        }
        (None, None) => None,
        _ => {
            return Err(invalid(
                "Give both the client ID and the client secret, or neither.",
            ));
        }
    };
    let fresh = given.is_some();
    let mut credentials = given
        .or_else(stored_credentials)
        .ok_or_else(|| boxed(
            "not_ready",
            3,
            "No Access service token is stored on this installation: give its client ID and secret.",
        ))?;
    health(&endpoint)
        .await
        .map_err(|failure| Box::new(refused(&failure)))?;
    let config = host.state.settings.config().map_err(|_| {
        boxed(
            "operation_failed",
            1,
            "The configuration could not be read.",
        )
    })?;
    // Already connected with these credentials: nothing to issue again.
    if !fresh && origin(&config.cloud.endpoint) == endpoint && enrolled(&config.cloud) {
        let whoami = exchange(
            &endpoint,
            reqwest::Method::GET,
            "/v1/whoami",
            Some(&credentials),
            None,
        )
        .await;
        if whoami.is_ok_and(|w| w["installation"]["id"] == json!(config.cloud.installation)) {
            let data = status(host).await.unwrap_or_default();
            return Ok(Reply::success(json!({"changed": false, "status": data})));
        }
    }
    credentials.token = None;
    let enrollment = exchange(
        &endpoint,
        reqwest::Method::POST,
        "/v1/enroll",
        Some(&credentials),
        Some(json!({"label": label, "host_version": env!("CARGO_PKG_VERSION")})),
    )
    .await
    .map_err(|failure| match failure {
        Failure::Refused(code) if code == "conflict" => boxed(
            "state_conflict",
            6,
            "The account has too many installations in the account store; revoke one first.",
        ),
        failure => Box::new(refused(&failure)),
    })?;
    let installation = enrollment["installation"].as_str().unwrap_or_default();
    let token = enrollment["token"].as_str().unwrap_or_default();
    let valid = uuid::Uuid::parse_str(installation).is_ok_and(|id| id.to_string() == installation)
        && token.len() == 48
        && token.starts_with("ptac_");
    if !valid {
        return Err(boxed(
            "operation_failed",
            1,
            "The account store answered an invalid enrollment.",
        ));
    }
    // Credentials first: a configuration without them reads as not enrolled, never the reverse.
    for (name, value) in [
        (ACCESS_ID, credentials.client_id.as_str()),
        (ACCESS_SECRET, credentials.client_secret.as_str()),
        (TOKEN, token),
    ] {
        security::put_desktop_secret("default", name, value).map_err(|_| {
            boxed(
                "operation_failed",
                1,
                "The credential store refused the account store credentials.",
            )
        })?;
    }
    {
        let _guard = host.state.operations.lock().await;
        let mut config = host.state.settings.config().map_err(|_| {
            boxed(
                "operation_failed",
                1,
                "The configuration could not be read.",
            )
        })?;
        config.cloud = Cloud {
            endpoint,
            installation: installation.to_owned(),
            mode: CloudMode::Off,
        };
        config
            .cloud
            .validate()
            .and_then(|_| host.state.settings.save_config(&config))
            .map_err(|_| {
                boxed(
                    "operation_failed",
                    1,
                    "The configuration could not be saved; connect again.",
                )
            })?;
    }
    let data = status(host).await.unwrap_or_default();
    Ok(Reply::success(
        json!({"changed": true, "installation": installation, "status": data}),
    ))
}

#[cfg(test)]
mod tests;
