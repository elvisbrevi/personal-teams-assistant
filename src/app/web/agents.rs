//! The portal's remote MCP endpoint (`POST /mcp`): the tool catalog of `pta mcp` over Streamable
//! HTTP, for an agent with nothing installed (a cloud session, a phone, a computer without the
//! host).
//!
//! Admission is the browser API's: exactly one verified edge assertion (`access.rs`) bound to an
//! enabled, admitted account. Non-browser clients obtain it through the identity provider's
//! managed OAuth for the portal's application; the edge resolves the client's opaque token and
//! sends the same signed assertion, so the portal runs no OAuth server and keeps no client
//! secret. A request that carries a browser `Origin`, a cookie or browser fetch metadata is
//! refused: the portal's session cookie never authenticates an agent.
//!
//! The endpoint is stateless. `initialize` returns an `Mcp-Session-Id` that is a sealed note of
//! the account and whether the client can ask the user (elicitation); nothing is stored. Only a
//! confirmation in flight is held in memory, until the user answers or five minutes pass.
//!
//! Each tool runs through `tools::run`, the same code as `pta mcp`, with its host operations
//! forwarded to the signed-in account's host with `Transport::Agent`: the portal's per-account
//! allowlist, `agent::METHODS` and every check of that host apply. Requests are bounded in body
//! size, time and rate per account, and logged by operation, code and duration only.
use super::super::{
    agent,
    mcp::{self as protocol, Answer},
    tools::{self, Effect},
};
use super::*;
use axum::body::Body;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use tokio::sync::mpsc;

/// A request body larger than this is refused before it is parsed.
pub(super) const BODY_LIMIT: usize = 256_000;
/// Requests per minute for one account, and for the whole endpoint.
const ACCOUNT_RATE: usize = 120;
const ENDPOINT_RATE: usize = 600;
/// How long a tool may run; one that waits for a model gets longer.
const RUN: Duration = Duration::from_secs(120);
const MODEL_RUN: Duration = Duration::from_secs(300);
/// How long a confirmation waits for the user's answer.
const ASK: Duration = Duration::from_secs(300);
/// Confirmations in flight, in total and per account.
const PENDING: usize = 64;
const PENDING_PER_ACCOUNT: usize = 8;
/// An `Mcp-Session-Id` lasts at most this long; the client then initializes again.
const SESSION_SECONDS: u64 = 12 * 3600;
const SESSION_DOMAIN: &[u8] = b"personal-teams-assistant/remote-mcp-session/v1";

/// What the server tells a remote agent beyond the shared instructions.
const REMOTE: &str = " This is the account's remote endpoint on its portal: tools/list holds what an agent may use here. Revealing or typing a value, approving a launcher or a delivery destination, launchers, updates, key custody, enrolling a computer and operator administration are not available here; ask the user to do them in the GUI. Tools that act on a computer (implementation runs, machine management, directories) go once to the enrolled computer the arguments name, within its allowed roots; after a timeout, read machine_detail or implementation_runs before deciding to repeat anything.";

/// A confirmation in flight, waiting for the client's answer.
type Waiting = oneshot::Sender<Value>;

pub(super) struct Agents {
    key: [u8; 32],
    pending: std::sync::Mutex<HashMap<String, Waiting>>,
}
impl Agents {
    pub(super) fn new() -> Self {
        use rand::RngCore;
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);
        Self {
            key,
            pending: std::sync::Mutex::new(HashMap::new()),
        }
    }
    fn wait(&self, account: &str, id: &str, waiting: Waiting) -> bool {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|_, waiting| !waiting.is_closed());
        let prefix = format!("{account}\n");
        if pending.len() >= PENDING
            || pending.keys().filter(|k| k.starts_with(&prefix)).count() >= PENDING_PER_ACCOUNT
        {
            return false;
        }
        pending.insert(format!("{account}\n{id}"), waiting);
        true
    }
    fn forget(&self, account: &str, id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&format!("{account}\n{id}"));
    }
    /// Deliver the client's answer to the confirmation `id` of this account only.
    fn answer(&self, account: &str, id: &str, reply: Value) -> bool {
        let waiting = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&format!("{account}\n{id}"));
        waiting.is_some_and(|waiting| waiting.send(reply).is_ok())
    }
    fn seal(&self, session: &Session) -> String {
        let payload = serde_json::to_vec(session).unwrap_or_default();
        let mac = crate::mirror::seal::hmac_sha256(&self.key, &[SESSION_DOMAIN, &payload]);
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(mac)
        )
    }
    fn open(&self, token: &str) -> Option<Session> {
        if token.len() > 1024 {
            return None;
        }
        let (payload, mac) = token.split_once('.')?;
        let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
        let expected = crate::mirror::seal::hmac_sha256(&self.key, &[SESSION_DOMAIN, &payload]);
        if !security::constant_eq(mac, &URL_SAFE_NO_PAD.encode(expected)) {
            return None;
        }
        let session: Session = serde_json::from_slice(&payload).ok()?;
        (session.expires > access::now()).then_some(session)
    }
}

/// The sealed content of an `Mcp-Session-Id`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    account: String,
    version: String,
    subject: String,
    elicitation: bool,
    expires: u64,
}
impl Session {
    fn belongs_to(&self, account: &Account, identity: &access::Verified) -> bool {
        self.account == account.id
            && self.version == account.version
            && self.subject == identity.binding.provider_user_id
    }
}

/// One log line: the operation (a fixed name, never caller text), its code and its duration.
fn record(operation: &str, code: &str, started: Instant) {
    eprintln!("{}", line(operation, code, started.elapsed()));
}
pub(super) fn line(operation: &str, code: &str, elapsed: Duration) -> String {
    format!(
        "remote_mcp operation={operation} code={code} ms={}",
        elapsed.as_millis()
    )
}
/// The name logged for a JSON-RPC method: only methods this endpoint knows, never caller text.
fn method_label(method: &str) -> &'static str {
    match method {
        "initialize" => "initialize",
        "ping" => "ping",
        "tools/list" => "tools/list",
        "tools/call" => "tools/call",
        _ if method.starts_with("notifications/") => "notification",
        _ => "unknown",
    }
}
/// The catalog's own name for a tool, never the caller's text.
fn tool_label(name: &str) -> &'static str {
    tools::TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .map_or("unknown_tool", |tool| tool.name)
}

fn status(code: StatusCode, operation: &str, started: Instant) -> Response {
    record(operation, code.as_str(), started);
    code.into_response()
}
fn rpc(value: Value) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_vec(&value).unwrap_or_default(),
    )
        .into_response()
}
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
fn event(message: &Value) -> Bytes {
    Bytes::from(format!(
        "event: message\ndata: {}\n\n",
        serde_json::to_string(message).unwrap_or_default()
    ))
}

/// A browser marks its requests; an agent's never carry these.
pub(super) fn from_browser(headers: &HeaderMap) -> bool {
    headers.contains_key(header::ORIGIN)
        || headers.contains_key(header::COOKIE)
        || headers.contains_key("sec-fetch-site")
        || headers.contains_key("sec-fetch-mode")
}
fn json_content(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split(';').next())
        .is_some_and(|h| h.trim().eq_ignore_ascii_case("application/json"))
}
fn accepts(headers: &HeaderMap, kind: &str) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|h| h.split(','))
        .map(|h| h.split(';').next().unwrap_or_default().trim())
        .any(|h| h.eq_ignore_ascii_case(kind) || h == "*/*")
}

/// The enabled, admitted account bound to this verified identity, or none.
fn account_of(portal: &Portal, identity: &access::Verified) -> Option<Account> {
    accounts(&portal.root).ok()?.into_iter().find(|e| {
        portal.settings.admits(e)
            && e.access_binding.as_ref() == Some(&identity.binding)
            && identity.issued_at >= e.access_valid_after
    })
}

/// Host operations of a tool, sent to this account's host with the agent's authority.
fn dispatch(portal: &Arc<Portal>, account: &Account) -> tools::Dispatch {
    let portal = portal.clone();
    let account = account.clone();
    Arc::new(move |request: control::Request| {
        let portal = portal.clone();
        let account = account.clone();
        Box::pin(async move {
            if agent::check_method(&request.method).is_err() {
                return Ok(control::Reply::error("authority_denied", 6, agent::DENIED));
            }
            forward(&portal, &account, request, authority::Transport::Agent)
                .await
                .map_err(|status| anyhow::anyhow!("The account's host is unavailable ({status})."))
        })
    })
}

/// Run an offered tool once, within its time bound. A timeout is reported, never replayed.
async fn execute(dispatch: tools::Dispatch, name: &str, args: &Value) -> (Value, &'static str) {
    let limit = if matches!(name, "chat_simulate" | "answer_draft") {
        MODEL_RUN
    } else {
        RUN
    };
    match tokio::time::timeout(limit, tools::run_with(dispatch, name, args)).await {
        Ok(Ok(value)) => (protocol::outcome(Ok(value)), "ok"),
        Ok(Err(message)) => (protocol::outcome(Err(message)), "tool_error"),
        Err(_) => (
            protocol::outcome(Err("The operation did not finish in time and may still complete. It was not repeated: read its state (for example machine_detail, implementation_runs or the item itself) before deciding to call it again.".into())),
            "timeout",
        ),
    }
}

pub(super) async fn other() -> Response {
    // Stateless: no server-initiated stream and no session to delete.
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

pub(super) async fn post(
    State(portal): State<Arc<Portal>>,
    Extension(identity): Extension<access::Verified>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started = Instant::now();
    if from_browser(&headers) {
        return status(StatusCode::FORBIDDEN, "admission", started);
    }
    let Some(account) = account_of(&portal, &identity) else {
        return status(StatusCode::FORBIDDEN, "admission", started);
    };
    if !portal
        .limit(&[
            ("mcp", ENDPOINT_RATE),
            (&format!("mcp:{}", account.id), ACCOUNT_RATE),
        ])
        .await
    {
        return status(StatusCode::TOO_MANY_REQUESTS, "admission", started);
    }
    if !json_content(&headers) {
        return status(StatusCode::UNSUPPORTED_MEDIA_TYPE, "admission", started);
    }
    if headers.contains_key(header::ACCEPT) && !accepts(&headers, "application/json") {
        return status(StatusCode::NOT_ACCEPTABLE, "admission", started);
    }
    if let Some(version) = headers.get("mcp-protocol-version")
        && !version
            .to_str()
            .is_ok_and(|v| protocol::PROTOCOLS.contains(&v))
    {
        return status(StatusCode::BAD_REQUEST, "admission", started);
    }
    let session = match headers.get("mcp-session-id") {
        None => None,
        Some(token) => match token
            .to_str()
            .ok()
            .and_then(|token| portal.agents.open(token))
            .filter(|session| session.belongs_to(&account, &identity))
        {
            Some(session) => Some(session),
            // Unknown, expired or another account's: the client initializes again.
            None => return status(StatusCode::NOT_FOUND, "admission", started),
        },
    };
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        record("parse", "400", started);
        return (
            StatusCode::BAD_REQUEST,
            rpc(rpc_error(Value::Null, -32700, "Parse error")),
        )
            .into_response();
    };
    if !message.is_object() {
        record("batch", "400", started);
        return (
            StatusCode::BAD_REQUEST,
            rpc(rpc_error(
                Value::Null,
                -32600,
                "Send one JSON-RPC message per request.",
            )),
        )
            .into_response();
    }
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned();
    match (method, id) {
        // The client's answer to a confirmation this account is waiting for.
        (None, Some(id)) => {
            let key = id.as_str().unwrap_or_default();
            if portal.agents.answer(&account.id, key, message.clone()) {
                status(StatusCode::ACCEPTED, "answer", started)
            } else {
                status(StatusCode::BAD_REQUEST, "answer", started)
            }
        }
        (Some(method), None) => status(StatusCode::ACCEPTED, method_label(method), started),
        (Some(method), Some(id)) => {
            let params = message.get("params").cloned().unwrap_or(json!({}));
            request(
                portal, account, identity, session, &headers, method, id, params, started,
            )
            .await
        }
        (None, None) => {
            record("invalid", "400", started);
            (
                StatusCode::BAD_REQUEST,
                rpc(rpc_error(Value::Null, -32600, "Invalid request")),
            )
                .into_response()
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn request(
    portal: Arc<Portal>,
    account: Account,
    identity: access::Verified,
    session: Option<Session>,
    headers: &HeaderMap,
    method: &str,
    id: Value,
    params: Value,
    started: Instant,
) -> Response {
    let label = method_label(method);
    match method {
        "initialize" => {
            let version = protocol::negotiate(params["protocolVersion"].as_str());
            let sealed = portal.agents.seal(&Session {
                account: account.id.clone(),
                version: account.version.clone(),
                subject: identity.binding.provider_user_id.clone(),
                elicitation: params["capabilities"].get("elicitation").is_some(),
                expires: access::now() + SESSION_SECONDS,
            });
            let instructions = format!("{}{REMOTE}", protocol::INSTRUCTIONS);
            let mut response = rpc(
                json!({"jsonrpc": "2.0", "id": id, "result": protocol::initialized(version, &instructions)}),
            );
            if let Ok(value) = sealed.parse() {
                response.headers_mut().insert("mcp-session-id", value);
            }
            record(label, "ok", started);
            response
        }
        "ping" => {
            record(label, "ok", started);
            rpc(json!({"jsonrpc": "2.0", "id": id, "result": {}}))
        }
        "tools/list" => {
            let offered: Vec<Value> = tools::TOOLS
                .iter()
                .filter(|tool| agent::tool(tool.name) == agent::Remote::Offered)
                .map(protocol::listed)
                .collect();
            record(label, "ok", started);
            rpc(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": offered}}))
        }
        "tools/call" => {
            let elicitation = session.as_ref().is_some_and(|s| s.elicitation)
                && accepts(headers, "text/event-stream");
            call(portal, account, elicitation, id, params, started).await
        }
        _ => {
            record(label, "-32601", started);
            rpc(rpc_error(
                id,
                -32601,
                &format!("Method not found: {}", method_label(method)),
            ))
        }
    }
}

async fn call(
    portal: Arc<Portal>,
    account: Account,
    elicitation: bool,
    id: Value,
    params: Value,
    started: Instant,
) -> Response {
    let name = params["name"].as_str().unwrap_or_default();
    let operation = tool_label(name);
    let mut args = params.get("arguments").cloned().unwrap_or(json!({}));
    let Some(tool) = tools::TOOLS.iter().find(|t| t.name == name) else {
        record(operation, "-32602", started);
        return rpc(rpc_error(id, -32602, "Unknown tool."));
    };
    if let agent::Remote::Denied(reason) = agent::tool(tool.name) {
        record(operation, "denied", started);
        return rpc(
            json!({"jsonrpc": "2.0", "id": id, "result": protocol::outcome(Err(reason.into()))}),
        );
    }
    if !args.is_object() {
        record(operation, "-32602", started);
        return rpc(rpc_error(id, -32602, "arguments must be an object"));
    }
    if authority::reject_authority_arguments(&args).is_err() {
        record(operation, "denied", started);
        return rpc(rpc_error(
            id,
            -32602,
            "Account and invocation authority cannot be supplied as arguments.",
        ));
    }
    if let Err(message) = tools::validate(tool.name, &args) {
        record(operation, "-32602", started);
        return rpc(rpc_error(id, -32602, &message));
    }
    let question = (tool.effect == Effect::External)
        .then(|| tools::confirmation(tool.name, &args))
        .flatten();
    let dispatch = dispatch(&portal, &account);
    let Some(question) = question else {
        let Ok(_permit) = portal.requests.try_acquire() else {
            return status(StatusCode::TOO_MANY_REQUESTS, operation, started);
        };
        let (result, code) = execute(dispatch, tool.name, &args).await;
        record(operation, code, started);
        return rpc(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    };
    if !elicitation {
        if !protocol::approved(&Answer::Unanswered, &args) {
            record(operation, "unconfirmed", started);
            return rpc(
                json!({"jsonrpc": "2.0", "id": id, "result": protocol::unconfirmed(&Answer::Unanswered, &question)}),
            );
        }
        let Ok(_permit) = portal.requests.try_acquire() else {
            return status(StatusCode::TOO_MANY_REQUESTS, operation, started);
        };
        args["confirmed"] = json!(true);
        let (result, code) = execute(dispatch, tool.name, &args).await;
        record(operation, code, started);
        return rpc(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }
    // Ask the user through the client on this request's event stream; the client answers with a
    // POST of its own, delivered to the waiting confirmation.
    let (sender, receiver) = mpsc::channel::<Bytes>(4);
    let name = tool.name;
    tokio::spawn(async move {
        let ask = format!("confirm-{}", uuid::Uuid::new_v4());
        let (waiting, answered) = oneshot::channel();
        let answer = if !portal.agents.wait(&account.id, &ask, waiting) {
            Answer::Unanswered
        } else if sender
            .send(event(&protocol::elicitation(&ask, &question)))
            .await
            .is_err()
        {
            // The client left before it could be asked: nothing runs.
            portal.agents.forget(&account.id, &ask);
            record(operation, "disconnected", started);
            return;
        } else {
            let asked = Instant::now();
            let reply = tokio::time::timeout(ASK, answered).await;
            portal.agents.forget(&account.id, &ask);
            match reply {
                Ok(Ok(reply)) => protocol::answer(&reply, asked.elapsed()),
                _ => Answer::Unanswered,
            }
        };
        let (result, code) = if !protocol::approved(&answer, &args) {
            (protocol::unconfirmed(&answer, &question), "unconfirmed")
        } else {
            match portal.requests.try_acquire() {
                Ok(_permit) => {
                    args["confirmed"] = json!(true);
                    execute(dispatch, name, &args).await
                }
                Err(_) => (
                    protocol::outcome(Err(
                        "The portal is busy; nothing was done. Try again shortly.".into(),
                    )),
                    "busy",
                ),
            }
        };
        record(operation, code, started);
        let _ = sender
            .send(event(
                &json!({"jsonrpc": "2.0", "id": id, "result": result}),
            ))
            .await;
    });
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|bytes| (Ok::<_, std::convert::Infallible>(bytes), receiver))
    });
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

#[cfg(test)]
mod tests;
