//! Host operations for delivery destinations: an agent may propose one, only a person at the GUI
//! (the owner's local page or an authenticated portal browser) approves it, and a delivery is
//! performed by this host, which resolves the value live, sends it and returns only a status.
//! No operation takes or returns a value, and no response body or header reaches the caller.
use super::{authority, control::Reply, implementation, standing, variables as vars, *};
use crate::{
    deliveries::{self as rules, Destination, Destinations, Target},
    variables::{self as vault, Secret},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::VecDeque;

/// One delivery per destination per minute, and at most this many per hour in all.
const DESTINATION_SECONDS: i64 = 60;
const HOURLY: usize = 20;

pub(super) struct State {
    rate: std::sync::Mutex<Rate>,
    /// Serializes read-modify-write of the destination list.
    edit: std::sync::Mutex<()>,
    /// Where deliveries are sent instead of their destination; tests point it at a local double.
    #[cfg(test)]
    pub(super) origin: std::sync::Mutex<Option<String>>,
}

#[derive(Default)]
struct Rate {
    last: BTreeMap<String, i64>,
    recent: VecDeque<i64>,
}

impl State {
    pub(super) fn new() -> Self {
        Self {
            rate: std::sync::Mutex::new(Rate::default()),
            edit: std::sync::Mutex::new(()),
            #[cfg(test)]
            origin: std::sync::Mutex::new(None),
        }
    }
    /// Counts an attempt, successful or not; refuses one over the limits.
    fn admit(&self, destination: &str, now: i64) -> std::result::Result<(), vault::Error> {
        let mut rate = self.rate.lock().unwrap_or_else(|e| e.into_inner());
        rate.recent.retain(|at| now - at < 3600);
        if rate
            .last
            .get(destination)
            .is_some_and(|at| now - at < DESTINATION_SECONDS)
        {
            return Err(vault::Error::Conflict(
                "This destination received a delivery less than a minute ago. Wait before delivering again.".into(),
            ));
        }
        if rate.recent.len() >= HOURLY {
            return Err(vault::Error::Conflict(
                "The hourly delivery limit was reached. Wait before delivering again.".into(),
            ));
        }
        rate.last.insert(destination.to_owned(), now);
        rate.recent.push_back(now);
        Ok(())
    }
}

pub(super) fn handles(method: &str) -> bool {
    method == "variable_deliver" || method.starts_with("variable_delivery_")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DefineArgs {
    id: String,
    description: String,
    target: Target,
    variables: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveArgs {
    id: String,
    /// The digest of the definition the person was shown.
    digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameArgs {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliverArgs {
    destination: String,
    variable: String,
    #[serde(default)]
    version: Option<u64>,
}

fn parse<T: DeserializeOwned>(args: &Value) -> std::result::Result<T, vault::Error> {
    serde_json::from_value(args.clone())
        .map_err(|_| vault::Error::Input("Invalid arguments for this delivery operation.".into()))
}
fn denied(message: &str) -> vault::Error {
    vault::Error::Conflict(message.into())
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Reply {
    match run(host, &request.method, &request.args).await {
        Ok(data) => Reply::success(data),
        Err(error) => vars::failure(error),
    }
}

fn destinations(host: &Host) -> std::result::Result<Destinations, vault::Error> {
    let profile = host.state.settings.profile();
    Ok(
        match implementation::link(&host.state).map_err(|_| {
            vault::Error::Storage("The coordinator connection is unreadable.".into())
        })? {
            Some(link) => Destinations::account(profile, &standing::qualifier(&link)),
            None => Destinations::new(profile),
        },
    )
}

/// The account's catalog as this installation sees it: metadata only.
fn catalog(host: &Host) -> std::result::Result<Vec<vault::Metadata>, vault::Error> {
    match implementation::link(&host.state)
        .map_err(|_| vault::Error::Storage("The coordinator connection is unreadable.".into()))?
    {
        Some(link) => Ok(vars::cached(host, &link)),
        None => host.state.variables.store(host)?.list(None),
    }
}

/// Audits on this installation's own store; a signed-in installation's uses are audited by the
/// coordinator when it resolves the value.
fn audit(host: &Host, action: &str, entry: Option<&str>, outcome: &str) {
    if implementation::link(&host.state).ok().flatten().is_none()
        && let Ok(store) = host.state.variables.store(host)
    {
        store.audit(action, entry, outcome);
    }
}

fn reference(catalog: &[vault::Metadata], id: &str) -> Value {
    match catalog.iter().find(|v| v.id == id) {
        Some(v) => json!({"id": id, "state": "ok", "name": v.name, "version": v.version}),
        None => json!({"id": id, "state": "missing"}),
    }
}

/// What a person reviews before approving: everything the digest covers, never a value.
fn view(destination: &Destination, catalog: &[vault::Metadata]) -> Value {
    let target = match &destination.target {
        Target::Http {
            method,
            headers,
            body,
            auth,
            confirm_field,
            ..
        } => json!({
            "kind": "http",
            "method": method,
            "headers": headers,
            "body": body,
            "auth": auth.as_ref().map(|a| json!({"header": a.header, "prefix": a.prefix, "variable": reference(catalog, &a.variable)})),
            "confirm_field": confirm_field,
        }),
        Target::CloudflareWorkerSecret {
            account_id,
            script,
            secret_name,
            token,
        } => json!({
            "kind": "cloudflare_worker_secret",
            "method": "PUT",
            "account_id": account_id,
            "script": script,
            "secret_name": secret_name,
            "token": reference(catalog, token),
        }),
    };
    json!({
        "id": destination.id,
        "description": destination.description,
        "url": destination.endpoint(),
        "target": target,
        "variables": destination.variables.iter().map(|v| reference(catalog, v)).collect::<Vec<_>>(),
        "digest": destination.digest(),
        "approved": destination.approved(),
        "created_at": destination.created_at,
        "approved_at": destination.approved_at,
    })
}

async fn run(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> std::result::Result<Value, vault::Error> {
    let invocation = authority::invocation(&host.state)
        .map_err(|_| vault::Error::Conflict(authority::ACCOUNT_UNAVAILABLE.into()))?;
    let transport = invocation.transport;
    if transport == authority::Transport::Machine {
        return Err(denied(
            "This variable operation is not available from this client.",
        ));
    }
    if !invocation.scope.owner {
        return Err(denied(
            "Delivery destinations are available only to the owner of this installation.",
        ));
    }
    let state = &host.state.variables.deliveries;
    let now = chrono::Utc::now().timestamp();
    match method {
        "variable_delivery_list" => {
            let catalog = catalog(host)?;
            let list = destinations(host)?.list()?;
            Ok(json!({
                "destinations": list.iter().map(|d| view(d, &catalog)).collect::<Vec<_>>(),
                "approve_here": matches!(transport, authority::Transport::Window | authority::Transport::Browser),
            }))
        }
        "variable_delivery_define" => {
            let a: DefineArgs = parse(args)?;
            let destination = Destination {
                id: a.id,
                description: a.description,
                target: a.target,
                variables: a.variables,
                created_at: now,
                approved_at: None,
                approved_digest: None,
            };
            rules::validate(&destination)?;
            let catalog = catalog(host)?;
            for id in destination
                .variables
                .iter()
                .map(String::as_str)
                .chain(destination.credential())
            {
                if !catalog.iter().any(|v| v.id == id) {
                    return Err(vault::Error::Input(format!(
                        "There is no variable {id} in this account. Name variables by the IDs from variable_list."
                    )));
                }
            }
            let _edit = state.edit.lock().unwrap_or_else(|e| e.into_inner());
            let result = destinations(host)?.add(destination.clone());
            audit(host, "delivery_define", None, outcome(&result));
            result?;
            Ok(json!({
                "destination": view(&destination, &catalog),
                "next": "Nothing is sent until the user reviews this destination and approves it in Settings > Variables > Where variables are used > Delivery destinations (this computer's application, in its window or the page pta app open shows, or the portal). No tool can approve it.",
            }))
        }
        "variable_delivery_approve" => {
            // Only a person at the GUI approves what an agent may only propose: the same clients
            // that may reveal a value, never the local channel shared with agents.
            if !matches!(
                transport,
                authority::Transport::Window | authority::Transport::Browser
            ) {
                audit(host, "delivery_approve", None, "denied_client");
                return Err(denied(
                    "Only the user approves a delivery destination, in this computer's application (the local page pta app open shows) or the portal.",
                ));
            }
            let a: ApproveArgs = parse(args)?;
            let _edit = state.edit.lock().unwrap_or_else(|e| e.into_inner());
            let result = destinations(host)?.approve(&a.id, &a.digest, now);
            audit(host, "delivery_approve", None, outcome(&result));
            let approved = result?;
            Ok(json!({"destination": approved.id, "approved_at": approved.approved_at}))
        }
        "variable_delivery_remove" => {
            let a: NameArgs = parse(args)?;
            let _edit = state.edit.lock().unwrap_or_else(|e| e.into_inner());
            let removed = destinations(host)?.remove(&a.id)?;
            audit(
                host,
                "delivery_remove",
                None,
                if removed { "ok" } else { "failed" },
            );
            Ok(json!({"destination": a.id, "removed": removed}))
        }
        "variable_deliver" => {
            let a: DeliverArgs = parse(args)?;
            let result = deliver(host, &a, now).await;
            let entry = uuid::Uuid::parse_str(&a.variable)
                .ok()
                .map(|id| id.hyphenated().to_string());
            let word = match &result {
                Ok(data) => match data["result"]["status"].as_str() {
                    Some("ok") => "ok",
                    Some("rejected") => "rejected",
                    Some("unreachable") => "unreachable",
                    _ => "failed",
                },
                Err(error) => refusal(error),
            };
            audit(host, "deliver", entry.as_deref(), word);
            result
        }
        _ => Err(vault::Error::Input("Unknown variable operation.".into())),
    }
}

/// The audit outcome of a result: a short fixed word, never any text from the request.
fn outcome<T>(result: &std::result::Result<T, vault::Error>) -> &'static str {
    result.as_ref().map_or_else(refusal, |_| "ok")
}
fn refusal(error: &vault::Error) -> &'static str {
    match error {
        vault::Error::Input(_) => "rejected",
        vault::Error::Conflict(message) if message.contains("was deleted") => "deleted",
        vault::Error::Conflict(_) => "denied",
        vault::Error::Offline(_) => "offline",
        vault::Error::Revoked(_) => "revoked",
        _ => "failed",
    }
}

async fn deliver(
    host: &Arc<Host>,
    a: &DeliverArgs,
    now: i64,
) -> std::result::Result<Value, vault::Error> {
    let destination = destinations(host)?.get(&a.destination)?;
    rules::validate(&destination)?;
    if !destination.approved() {
        return Err(denied(
            "This destination waits for the user's approval in Settings > Variables (this computer's local page, or the portal); nothing was sent.",
        ));
    }
    let variable = uuid::Uuid::parse_str(&a.variable)
        .map_err(|_| vault::Error::Input("The variable ID must be a UUID.".into()))?
        .hyphenated()
        .to_string();
    if !destination.variables.contains(&variable) {
        return Err(denied(
            "This destination was not approved to receive that variable; nothing was sent.",
        ));
    }
    let state = &host.state.variables.deliveries;
    state.admit(&destination.id, now)?;
    // Every delivery is a new use: resolved now, from this store or live from the coordinator.
    let (metadata, value) = consumers::resolve_entry(host, &variable, a.version).await?;
    let credential: Option<Secret> = match destination.credential() {
        Some(entry) => Some(consumers::resolve_entry(host, entry, None).await?.1),
        None => None,
    };
    let request = rules::request(&destination, &value, credential.as_ref())?;
    drop(credential);
    let result = send(host, &destination, request, &value).await;
    drop(value);
    let result = result?;
    Ok(json!({
        "destination": destination.id,
        "url": destination.endpoint(),
        "variable": {"id": metadata.id, "name": metadata.name, "version": metadata.version},
        "result": result,
        "retracting": "A delivered value stays at the destination. To withdraw it, rotate or revoke it at its provider, then replace the variable's value.",
    }))
}

async fn send(
    host: &Host,
    destination: &Destination,
    mut request: rules::Request,
    value: &Secret,
) -> std::result::Result<rules::Outcome, vault::Error> {
    #[cfg(test)]
    let origin = host
        .state
        .variables
        .deliveries
        .origin
        .lock()
        .unwrap()
        .clone();
    #[cfg(test)]
    if let Some(origin) = origin {
        // Tests answer from a local double: same request, another origin.
        let path = url::Url::parse(&request.url).unwrap();
        request.url = format!("{origin}{}", &path[url::Position::BeforePath..]);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        return Ok(rules::send(&client, destination, request, value).await);
    }
    let _ = (host, &mut request);
    // HTTPS on 443 only, every resolved address public and pinned, no redirects, bounded time.
    match crate::knowledge::public_client(&request.url).await {
        Ok(client) => Ok(rules::send(&client, destination, request, value).await),
        Err(error) if error.to_string().contains("non-public") => Err(denied(
            "The destination's host resolves to a private or reserved address; nothing was sent.",
        )),
        Err(_) => Ok(rules::Outcome {
            status: "unreachable",
            http_status: None,
            confirmed_by_destination: false,
        }),
    }
}

#[cfg(test)]
mod tests;
