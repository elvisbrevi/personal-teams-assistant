//! This installation's link to the account store Worker (`cloud/`, phase 3a of the cloud storage
//! design): enrollment and status. The mirror (phase 3b, `mirror`), the account store as the
//! source of truth of the configuration (phase 3c, `mirror::primary`) and the migration, rollback
//! and erasure (phase 4a, `mirror::migration`) use this link.
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
use crate::mirror::engine::Failure;
use serde_json::{Value, json};
use std::time::Duration;

mod mirror;
pub(in crate::app) use mirror::{State, migration, primary, run};

pub(super) const ACCESS_ID: &str = "CLOUD_ACCESS_CLIENT_ID";
pub(super) const ACCESS_SECRET: &str = "CLOUD_ACCESS_CLIENT_SECRET";
pub(super) const TOKEN: &str = "CLOUD_STORE_TOKEN";
/// The installation-local credentials of the link, listed (never required) by `credentials list`.
pub(super) const CREDENTIALS: [&str; 3] = [ACCESS_ID, ACCESS_SECRET, TOKEN];
/// The operator's permission for a portal account to use the account store, in its profile.
pub(in crate::app) const GRANT_FILE: &str = "cloud-grant.json";
/// The protocol version this host speaks.
const PROTOCOL: u64 = 1;

/// What the operator enabled for a portal account (`pta web users cloud enable`): the account
/// store it may use. Only operator-local administration writes it, next to the Access service
/// token it stored in the account's own credential store; the account's host only reads it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Grant {
    pub endpoint: String,
    pub granted_at: i64,
    /// When the service token expires, as the operator gave it (0: unknown).
    #[serde(default)]
    pub token_expires_at: i64,
}

/// The operator's grant in a profile, if any and readable.
pub(in crate::app) fn grant(profile: &Path) -> Option<Grant> {
    let text = std::fs::read_to_string(profile.join(GRANT_FILE)).ok()?;
    let grant: Grant = serde_json::from_str(&text).ok()?;
    Cloud::check_endpoint(&grant.endpoint).ok()?;
    Some(grant)
}

/// Who may use the account store from here: the owner of an unshared profile, or a portal
/// account the operator enabled it for. Never a machine credential. The refusal, if any.
fn refused_here(host: &Host, invocation: &authority::Invocation) -> Option<Reply> {
    let scope = &invocation.scope;
    if invocation.transport == authority::Transport::Machine {
        return Some(Reply::error(
            "authority_denied",
            6,
            "This operation is not available from this client.",
        ));
    }
    if (scope.owner && !scope.shared)
        || (scope.shared && grant(host.state.settings.profile()).is_some())
    {
        return None;
    }
    Some(Reply::error("authority_denied", 6, NOT_ENABLED))
}

pub(in crate::app) const NOT_ENABLED: &str =
    "The portal operator has not enabled the account store for this account.";
const LABEL_CHARS: usize = 80;

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        "cloud_status"
            | "cloud_connect"
            | "cloud_mirror"
            | "cloud_verify"
            | "cloud_disconnect"
            | "cloud_promote"
            | "cloud_pull"
            | "cloud_migrate"
            | "cloud_rollback"
            | "cloud_erase"
            | "cloud_installations"
            | "cloud_revoke"
            | "cloud_rotate_key"
    )
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
    /// When the service token expires, `YYYY-MM-DD`; kept to warn before it does.
    #[serde(default)]
    token_expires: Option<String>,
}

fn standing_of(failure: &Failure) -> &'static str {
    match failure {
        Failure::Offline | Failure::Quota => "offline",
        Failure::Revoked => "revoked",
        Failure::Unconfigured => "unconfigured",
        Failure::Refused(_) => "offline",
    }
}

#[derive(Clone)]
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
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            let Ok(args) = serde_json::from_value::<ConnectArgs>(request.args.clone()) else {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            };
            connect(host, args).await.unwrap_or_else(|reply| *reply)
        }
        "cloud_mirror" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            let Ok(args) = serde_json::from_value::<mirror::MirrorArgs>(request.args.clone())
            else {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            };
            mirror::operate_mirror(host, args).await
        }
        "cloud_disconnect" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            let Ok(args) = serde_json::from_value::<DisconnectArgs>(request.args.clone()) else {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            };
            disconnect(host, args).await
        }
        "cloud_verify" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            if request.args != json!({}) && !request.args.is_null() {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            }
            mirror::operate_verify(host).await
        }
        "cloud_promote" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            let Ok(args) = serde_json::from_value::<primary::PromoteArgs>(request.args.clone())
            else {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            };
            primary::operate_promote(host, args).await
        }
        "cloud_pull" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            let Ok(args) = serde_json::from_value::<primary::PullArgs>(request.args.clone()) else {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            };
            primary::operate_pull(host, args).await
        }
        "cloud_migrate" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            let Ok(args) = serde_json::from_value::<migration::MigrateArgs>(request.args.clone())
            else {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            };
            migration::operate_migrate(host, args).await
        }
        "cloud_rollback" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            let Ok(args) = serde_json::from_value::<migration::RollbackArgs>(request.args.clone())
            else {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            };
            migration::operate_rollback(host, args).await
        }
        "cloud_erase" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            if request.args != json!({}) && !request.args.is_null() {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            }
            migration::operate_erase(host).await
        }
        "cloud_rotate_key" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            if request.args != json!({}) && !request.args.is_null() {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            }
            migration::operate_rotate(host).await
        }
        "cloud_installations" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            if request.args != json!({}) && !request.args.is_null() {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            }
            installations(host).await
        }
        "cloud_revoke" => {
            if let Some(reply) = refused_here(host, &invocation) {
                return reply;
            }
            let Ok(args) = serde_json::from_value::<RevokeArgs>(request.args.clone()) else {
                return Reply::error("invalid_input", 2, "Invalid arguments.");
            };
            revoke(host, args).await
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
    exchange_limited(endpoint, method, path, credentials, body, 2_000_000).await
}

/// Like `exchange`, reading at most `max_bytes` of the reply (record pages are large).
async fn exchange_limited(
    endpoint: &str,
    method: reqwest::Method,
    path: &str,
    credentials: Option<&Credentials>,
    body: Option<Value>,
    max_bytes: usize,
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
        crate::adapters::bounded_json(response, max_bytes)
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
pub(super) async fn status(host: &Arc<Host>) -> Result<Value> {
    // A portal account says whether its operator enabled the account store for it; without that
    // nothing is asked of the Worker.
    let enabled = !host.state.authority.shared || grant(host.state.settings.profile()).is_some();
    if !enabled {
        let config = host.state.settings.config()?;
        return Ok(json!({
            "mode": config.cloud.mode,
            "endpoint": null,
            "enrolled": false,
            "installation": null,
            "enabled": false,
            "standing": "disconnected",
            "problem": NOT_ENABLED,
            "checked_at": now(),
            "link": host.state.standing.store_view(&config.cloud, false),
            "mirror": mirror::describe(host, &config.cloud),
        }));
    }
    let mut view = linked_status(host).await?;
    view["enabled"] = json!(true);
    if let Some(grant) = grant(host.state.settings.profile()) {
        view["granted_endpoint"] = json!(origin(&grant.endpoint));
    }
    Ok(view)
}

async fn linked_status(host: &Arc<Host>) -> Result<Value> {
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
            "link": host.state.standing.store_view(cloud, false),
            "mirror": mirror::describe(host, cloud),
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
    view["mirror"] = mirror::describe(host, cloud);
    if let Some(token) = token_view(cloud, checked_at) {
        view["service_token"] = token;
    }
    let mut latency = serde_json::Map::new();
    let started = std::time::Instant::now();
    let health = health(&endpoint).await;
    latency.insert("health".into(), json!(started.elapsed().as_millis() as u64));
    view["latency_ms"] = Value::Object(latency.clone());
    let health = match health {
        Ok(health) => health,
        Err(failure) => {
            if enrolled {
                host.state.standing.observe_store(cloud, Some(&failure));
            }
            view["standing"] = json!(standing_of(&failure));
            view["problem"] = json!(problem(&failure));
            view["link"] = host.state.standing.store_view(cloud, enrolled);
            return Ok(view);
        }
    };
    view["worker"] = json!({
        "environment": health["environment"],
        "version": health["version"],
        "deployment": health["deployment"],
        "protocols": health["protocols"],
        "features": health["features"].as_array().cloned().unwrap_or_default(),
        "schema": health["schema"],
    });
    let credentials = stored_credentials().filter(|c| c.token.is_some());
    let Some(credentials) = credentials.filter(|_| enrolled) else {
        view["standing"] = json!("not_enrolled");
        view["link"] = host.state.standing.store_view(cloud, false);
        return Ok(view);
    };
    let started = std::time::Instant::now();
    let whoami = exchange(
        &endpoint,
        reqwest::Method::GET,
        "/v1/whoami",
        Some(&credentials),
        None,
    )
    .await;
    latency.insert("whoami".into(), json!(started.elapsed().as_millis() as u64));
    let started = std::time::Instant::now();
    let store = exchange(
        &endpoint,
        reqwest::Method::GET,
        "/v1/store",
        Some(&credentials),
        None,
    )
    .await;
    latency.insert("store".into(), json!(started.elapsed().as_millis() as u64));
    view["latency_ms"] = Value::Object(latency);
    match (whoami, store) {
        (Ok(whoami), Ok(store)) => {
            let same = whoami["installation"]["id"] == json!(cloud.installation);
            view["standing"] = json!(if same { "live" } else { "revoked" });
            host.state
                .standing
                .observe_store(cloud, (!same).then_some(&Failure::Revoked));
            view["principal"] = json!({
                "label": whoami["installation"]["label"],
                "enrolled_at": whoami["installation"]["enrolled_at"],
                "account_installations": whoami["account"]["installations"],
            });
            // Another computer migrated first: migrating here adopts the account's configuration.
            let held = store["collections"]["settings"]["records"]
                .as_u64()
                .unwrap_or(0);
            view["adopt"] = json!(held > 0 && mirror::adopts(host));
            view["store"] = store;
        }
        (Err(failure), _) | (_, Err(failure)) => {
            host.state.standing.observe_store(cloud, Some(&failure));
            view["standing"] = json!(standing_of(&failure));
            view["problem"] = json!(problem(&failure));
        }
    }
    view["link"] = host.state.standing.store_view(cloud, true);
    Ok(view)
}

/// Days a service token has left before status warns.
pub(super) const TOKEN_WARN_DAYS: i64 = 30;

/// The service token's expiry as the user gave it: when, how many days are left, and whether to
/// renew it now. Never the token.
fn token_view(cloud: &Cloud, now: i64) -> Option<Value> {
    (cloud.token_expires_at > 0).then(|| {
        let days = (cloud.token_expires_at - now).div_euclid(86_400);
        json!({
            "expires_at": cloud.token_expires_at,
            "days_left": days,
            "renew": days <= TOKEN_WARN_DAYS,
        })
    })
}

/// A date the user typed (`YYYY-MM-DD`) as Unix seconds at 00:00 UTC.
pub(in crate::app) fn expiry(text: &str) -> Option<i64> {
    let date = chrono::NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()?;
    let at = date.and_hms_opt(0, 0, 0)?.and_utc().timestamp();
    Cloud {
        token_expires_at: at,
        ..Cloud::default()
    }
    .validate()
    .ok()
    .map(|()| at)
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

pub(in crate::app) fn valid_client_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub(in crate::app) fn valid_client_secret(value: &str) -> bool {
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
    let token_expires = match args.token_expires.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(text) => Some(expiry(text).ok_or_else(|| {
            invalid("Give the service token's expiry as a date, like 2027-11-03.")
        })?),
    };
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
    // A portal account uses the account store and the service token its operator enabled.
    let mut token_expires = token_expires;
    if host.state.authority.shared {
        let grant = grant(host.state.settings.profile())
            .ok_or_else(|| boxed("authority_denied", 6, NOT_ENABLED))?;
        if given.is_some() {
            return Err(invalid(
                "The portal operator manages this account's service token.",
            ));
        }
        if origin(&grant.endpoint) != endpoint {
            return Err(invalid(
                "Use the account store the portal operator enabled for this account.",
            ));
        }
        token_expires =
            token_expires.or((grant.token_expires_at > 0).then_some(grant.token_expires_at));
    }
    let fresh = given.is_some();
    let mut credentials = given
        .or_else(stored_credentials)
        .ok_or_else(|| boxed(
            "not_ready",
            3,
            "No Access service token is stored on this installation: give its client ID and secret.",
        ))?;
    let worker = health(&endpoint)
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
            // Only the recorded expiry of the service token can change here.
            let changed = token_expires.is_some_and(|at| at != config.cloud.token_expires_at);
            if let Some(at) = token_expires.filter(|_| changed) {
                let _guard = host.state.operations.lock().await;
                let mut config = host.state.settings.config().map_err(|_| {
                    boxed(
                        "operation_failed",
                        1,
                        "The configuration could not be read.",
                    )
                })?;
                config.cloud.token_expires_at = at;
                host.state.settings.save_config(&config).map_err(|_| {
                    boxed(
                        "operation_failed",
                        1,
                        "The configuration could not be saved.",
                    )
                })?;
            }
            let data = status(host).await.unwrap_or_default();
            return Ok(Reply::success(json!({"changed": changed, "status": data})));
        }
    }
    // A new service token for an enrolled installation (given now, or stored by the portal
    // operator) moves the account to it, a renewal or a replacement, keeping the enrollment, the
    // store and the mode, instead of enrolling anew.
    if origin(&config.cloud.endpoint) == endpoint
        && enrolled(&config.cloud)
        && offers(&worker, "rebind")
        && let Ok(token) = security::secret(TOKEN)
        && let Some(reply) = rebind(host, &endpoint, &credentials, token, token_expires).await?
    {
        return Ok(reply);
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
        // A new service token without a date has no known expiry; the same token keeps its own.
        let token_expires_at = token_expires.unwrap_or(if fresh {
            0
        } else {
            config.cloud.token_expires_at
        });
        config.cloud = Cloud {
            endpoint,
            installation: installation.to_owned(),
            mode: CloudMode::Off,
            token_expires_at,
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

/// Whether the Worker's health lists an additive feature of protocol 1.
fn offers(health: &Value, feature: &str) -> bool {
    health["features"]
        .as_array()
        .is_some_and(|f| f.iter().any(|v| v == feature))
}

/// `POST /v1/rebind` with the new service token and this installation's credential. `None`: the
/// Worker no longer knows this installation's credential, so the caller enrolls anew.
async fn rebind(
    host: &Arc<Host>,
    endpoint: &str,
    credentials: &Credentials,
    token: String,
    token_expires: Option<i64>,
) -> std::result::Result<Option<Reply>, Box<Reply>> {
    let presented = Credentials {
        token: Some(token),
        ..credentials.clone()
    };
    let reply = match exchange(
        endpoint,
        reqwest::Method::POST,
        "/v1/rebind",
        Some(&presented),
        Some(json!({"confirm": "rebind"})),
    )
    .await
    {
        Ok(reply) => reply,
        Err(Failure::Revoked) => return Ok(None),
        Err(Failure::Refused(code)) if code == "conflict" => {
            return Err(boxed(
                "state_conflict",
                6,
                "This service token already belongs to another account in the account store. To join that account, disconnect this computer first; its data stays.",
            ));
        }
        Err(failure) => return Err(Box::new(refused(&failure))),
    };
    let config = host.state.settings.config().map_err(|_| {
        boxed(
            "operation_failed",
            1,
            "The configuration could not be read.",
        )
    })?;
    if reply["installation"] != json!(config.cloud.installation) {
        return Err(boxed(
            "operation_failed",
            1,
            "The account store answered for another installation.",
        ));
    }
    for (name, value) in [
        (ACCESS_ID, credentials.client_id.as_str()),
        (ACCESS_SECRET, credentials.client_secret.as_str()),
    ] {
        security::put_desktop_secret("default", name, value).map_err(|_| {
            boxed(
                "operation_failed",
                1,
                "The account store moved to the new service token, but this computer could not keep it: connect again with it.",
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
        // A new service token without a date has no known expiry.
        config.cloud.token_expires_at = token_expires.unwrap_or(0);
        host.state.settings.save_config(&config).map_err(|_| {
            boxed(
                "operation_failed",
                1,
                "The configuration could not be saved.",
            )
        })?;
    }
    let data = status(host).await.unwrap_or_default();
    Ok(Some(Reply::success(json!({
        "changed": true,
        "rebound": reply["rebound"] == true,
        "installation": config.cloud.installation,
        "status": data,
    }))))
}

/// The account's installations in the account store, this one marked `current`.
async fn installations(host: &Arc<Host>) -> Reply {
    let (endpoint, credentials, _) = match linked(host) {
        Ok(linked) => linked,
        Err(reply) => return *reply,
    };
    match exchange(
        &endpoint,
        reqwest::Method::GET,
        "/v1/installations",
        Some(&credentials),
        None,
    )
    .await
    {
        Ok(reply) => {
            let listed: Vec<Value> = reply["installations"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|i| {
                            json!({
                                "id": i["id"],
                                "label": i["label"],
                                "host_version": i["host_version"],
                                "enrolled_at": i["enrolled_at"],
                                "last_seen_at": i["last_seen_at"],
                                "revoked_at": i["revoked_at"],
                                "current": i["current"] == true,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let active = listed.iter().filter(|i| i["revoked_at"].is_null()).count();
            Reply::success(json!({"installations": listed, "active": active}))
        }
        Err(failure) => refused(&failure),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeArgs {
    installation: String,
}

/// Revoke another installation of the account in the account store: its credential stops
/// working at once. That computer keeps its data; it can connect again with the service token.
async fn revoke(host: &Arc<Host>, args: RevokeArgs) -> Reply {
    let Ok(id) = uuid::Uuid::parse_str(args.installation.trim()) else {
        return Reply::error(
            "invalid_input",
            2,
            "Give the installation's ID from cloud installations.",
        );
    };
    let (endpoint, credentials, current) = match linked(host) {
        Ok(linked) => linked,
        Err(reply) => return *reply,
    };
    if id.to_string() == current {
        return Reply::error(
            "invalid_input",
            2,
            "That is this computer: disconnect it instead.",
        );
    }
    match exchange(
        &endpoint,
        reqwest::Method::DELETE,
        &format!("/v1/installations/{id}"),
        Some(&credentials),
        None,
    )
    .await
    {
        Ok(_) => Reply::success(json!({"revoked": id.to_string()})),
        Err(Failure::Refused(code)) if code == "not_found" => {
            Reply::error("invalid_input", 2, "The account has no such installation.")
        }
        Err(failure) => refused(&failure),
    }
}

/// Revoke a revoked computer's installation in the account store (`machine_revoke`). It still
/// holds the account's content key and service token, so the reply says what else to do.
pub(in crate::app) async fn revoke_installation(host: &Arc<Host>, id: &str) -> Value {
    let next = "That computer still has the account's content key and service token: for a lost or stolen computer, replace the service token (the portal operator) and rotate the content key.";
    let reply = revoke(
        host,
        RevokeArgs {
            installation: id.to_owned(),
        },
    )
    .await;
    json!({
        "revoked": reply.ok || reply.message.as_deref() == Some("The account has no such installation."),
        "problem": (!reply.ok).then(|| reply.message.unwrap_or_default()),
        "next": next,
    })
}

/// The endpoint, both credentials and this installation's ID of an enrolled installation.
fn linked(host: &Arc<Host>) -> std::result::Result<(String, Credentials, String), Box<Reply>> {
    let config = host.state.settings.config().map_err(|_| {
        boxed(
            "operation_failed",
            1,
            "The configuration could not be read.",
        )
    })?;
    let credentials = stored_credentials()
        .filter(|c| c.token.is_some() && enrolled(&config.cloud))
        .ok_or_else(|| {
            boxed(
                "not_ready",
                3,
                "This installation is not connected to the account store.",
            )
        })?;
    Ok((
        origin(&config.cloud.endpoint),
        credentials,
        config.cloud.installation,
    ))
}

/// What a trusted computer receives to use the account store (decision 18 of the design): the
/// account's content key and service token, and where the store is. Never logged or shown.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handoff {
    endpoint: String,
    client_id: String,
    client_secret: String,
    #[serde(default)]
    token_expires_at: i64,
    content_key: crate::mirror::key::ContentBackup,
}

/// On the coordinator, while it registers a computer the person trusted with the account store:
/// the handoff sealed to the key that computer's sign-in started with (`context`: that sign-in),
/// or why there is none. The portal and the tunnel only carry the result.
pub(in crate::app) fn handoff(host: &Host, recipient: &str, context: &str) -> Value {
    let unavailable = |reason: &str| json!({"unavailable": reason});
    let Ok(config) = host.state.settings.config() else {
        return unavailable("The coordinator's configuration could not be read.");
    };
    let credentials = stored_credentials();
    let key = crate::mirror::key::load(&host.state.settings.profile().join("keys"));
    let (Some(credentials), Ok(Some(content))) = (credentials, key) else {
        return unavailable(
            "This account has no account store on its coordinator yet: migrate it there first.",
        );
    };
    if !enrolled(&config.cloud) {
        return unavailable("The coordinator is not connected to the account store.");
    }
    let bundle = Handoff {
        endpoint: origin(&config.cloud.endpoint),
        client_id: credentials.client_id,
        client_secret: credentials.client_secret,
        token_expires_at: config.cloud.token_expires_at,
        content_key: content.backup(),
    };
    match serde_json::to_vec(&bundle)
        .map_err(anyhow::Error::from)
        .and_then(|plain| crate::mirror::handoff::seal(recipient, context, &plain))
    {
        Ok(sealed) => json!({"sealed": sealed}),
        Err(_) => unavailable("The account store could not be handed to this computer."),
    }
}

/// On the computer that signed in: install a handoff it opened. The content key is written only
/// when it has none (a different one is never replaced), the service token goes to the credential
/// store and the account store's address to `[cloud]`; then it connects (enrolls) with them.
/// Nothing is uploaded: the mode stays as it was.
pub(in crate::app) async fn receive(host: &Arc<Host>, plain: &[u8], label: &str) -> Value {
    receive_with(host, plain, label, |name, value| {
        security::put_desktop_secret("default", name, value)
    })
    .await
}

/// `receive`, keeping the credentials with `store`.
async fn receive_with(
    host: &Arc<Host>,
    plain: &[u8],
    label: &str,
    store: impl Fn(&str, &str) -> Result<()>,
) -> Value {
    let Ok(bundle) = serde_json::from_slice::<Handoff>(plain) else {
        return json!({"installed": false, "problem": "The coordinator sent an unreadable account store handoff."});
    };
    if Cloud::check_endpoint(&bundle.endpoint).is_err()
        || !valid_client_id(&bundle.client_id)
        || !valid_client_secret(&bundle.client_secret)
    {
        return json!({"installed": false, "problem": "The coordinator sent an invalid account store handoff."});
    }
    let keys = host.state.settings.profile().join("keys");
    let key = match crate::mirror::key::import(&keys, &bundle.content_key) {
        Ok(written) => written,
        Err(error) => {
            return json!({"installed": false, "problem": error.to_string()});
        }
    };
    for (name, value) in [
        (ACCESS_ID, bundle.client_id.as_str()),
        (ACCESS_SECRET, bundle.client_secret.as_str()),
    ] {
        if store(name, value).is_err() {
            return json!({"installed": false, "key_written": key, "problem": "The credential store refused the account store credentials."});
        }
    }
    let expires = (bundle.token_expires_at > 0).then(|| {
        chrono::DateTime::from_timestamp(bundle.token_expires_at, 0)
            .map(|at| at.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    });
    let connected = connect(
        host,
        ConnectArgs {
            endpoint: bundle.endpoint.clone(),
            label: label.to_owned(),
            token_expires: expires,
            client_id: None,
            client_secret: None,
        },
    )
    .await
    .unwrap_or_else(|reply| *reply);
    json!({
        "installed": true,
        "key_written": key,
        "endpoint": bundle.endpoint,
        "connected": connected.ok,
        "problem": (!connected.ok).then(|| connected.message.unwrap_or_default()),
        "installation": connected.data["installation"],
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisconnectArgs {
    /// Also remove the account's Access service token and the endpoint from this installation.
    #[serde(default)]
    forget_service_token: bool,
}

/// Leave the account store: revoke this installation's credential in the Worker when it answers,
/// then remove it here and set the mode to `off`. Always succeeds locally and is safe to repeat;
/// the encrypted copy, the content key and the ledger stay, so connecting again continues.
async fn disconnect(host: &Arc<Host>, args: DisconnectArgs) -> Reply {
    let fail = || {
        Reply::error(
            "operation_failed",
            1,
            "The configuration could not be saved.",
        )
    };
    // Always the operations lock before the pass gate, as the loop takes them.
    let _operations = host.state.operations.lock().await;
    let _gate = mirror::hold(host).await;
    let Ok(config) = host.state.settings.config() else {
        return fail();
    };
    let cloud = config.cloud.clone();
    let mut notified = None;
    if !cloud.endpoint.is_empty()
        && enrolled(&cloud)
        && let Some(credentials) = stored_credentials().filter(|c| c.token.is_some())
    {
        let reply = exchange(
            &origin(&cloud.endpoint),
            reqwest::Method::DELETE,
            "/v1/installations/self",
            Some(&credentials),
            None,
        )
        .await;
        // A store that already refuses the credential has nothing left to revoke.
        notified = Some(match reply {
            Ok(_) | Err(Failure::Revoked) => true,
            Err(Failure::Refused(code)) => code == "not_found",
            Err(_) => false,
        });
    }
    let mut names = vec![TOKEN];
    if args.forget_service_token {
        names.extend([ACCESS_ID, ACCESS_SECRET]);
    }
    for name in names {
        if security::secret_source(name).is_ok_and(|s| s.is_some())
            && security::delete_desktop_secret("default", name).is_err()
        {
            return Reply::error(
                "operation_failed",
                1,
                "The credential store refused to remove the account store credential.",
            );
        }
    }
    {
        let Ok(mut config) = host.state.settings.config() else {
            return fail();
        };
        config.cloud = if args.forget_service_token {
            Cloud::default()
        } else {
            Cloud {
                endpoint: config.cloud.endpoint.clone(),
                token_expires_at: config.cloud.token_expires_at,
                ..Cloud::default()
            }
        };
        if config
            .cloud
            .validate()
            .and_then(|_| host.state.settings.save_config(&config))
            .is_err()
        {
            return fail();
        }
    }
    host.state.standing.reset_store();
    let changed = !cloud.installation.is_empty() || args.forget_service_token;
    Reply::success(json!({
        "changed": changed,
        "store_notified": notified,
        "service_token_forgotten": args.forget_service_token,
        "kept": "The encrypted copy, the account content key and the ledger stay; connect again to continue mirroring.",
        "status": mirror::describe(host, &host.state.settings.config().map(|c| c.cloud).unwrap_or_default()),
    }))
}

#[cfg(test)]
mod tests;
