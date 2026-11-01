//! Delivery destinations: another system a person approved to receive chosen account variables.
//!
//! The host performs the delivery itself: it resolves the value at that moment, builds the request
//! from the approved definition, sends it and keeps only the status. Nothing in a definition can
//! come from a value, and a definition's digest names exactly what the person approved; anything
//! different is a new, unapproved definition. Responses are read up to a bound and discarded:
//! only the HTTP status and a boolean computed here leave.
use crate::variables::{Error, Secret};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Error>;

pub const MAX_DESTINATIONS: usize = 32;
const MAX_VARIABLES: usize = 8;
const MAX_HEADERS: usize = 16;
const MAX_BODY: usize = 8 * 1024;
const MAX_RESPONSE: usize = 64 * 1024;
/// The one placeholder a body may contain, replaced by the delivered value.
pub const PLACEHOLDER: &str = "{{value}}";
const CLOUDFLARE_API: &str = "https://api.cloudflare.com";

fn input(message: impl Into<String>) -> Error {
    Error::Input(message.into())
}

/// Where and how a value is delivered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    /// A fixed HTTPS request whose JSON body holds the value in one place.
    Http {
        url: String,
        method: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        body: Value,
        #[serde(default)]
        auth: Option<Auth>,
        /// JSON pointer to a field of the response that must equal the value's last four
        /// characters for the destination to count as having confirmed it.
        #[serde(default)]
        confirm_field: Option<String>,
    },
    /// A Cloudflare Worker secret, set through Cloudflare's API like `wrangler secret put`.
    CloudflareWorkerSecret {
        account_id: String,
        script: String,
        secret_name: String,
        /// Entry ID of the variable holding a Cloudflare API token allowed to edit the Worker.
        token: String,
    },
}

/// A header carrying another variable (the destination's own credential).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    pub header: String,
    #[serde(default)]
    pub prefix: String,
    /// Entry ID of the variable sent in the header.
    pub variable: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub id: String,
    pub description: String,
    pub target: Target,
    /// Entry IDs of the variables that may be delivered here.
    pub variables: Vec<String>,
    pub created_at: i64,
    /// When a person approved the definition whose digest is `approved_digest`.
    #[serde(default)]
    pub approved_at: Option<i64>,
    #[serde(default)]
    pub approved_digest: Option<String>,
}

impl Destination {
    /// The SHA-256 of everything the person reviews: name, description, target and variables.
    pub fn digest(&self) -> String {
        let canonical = json!({
            "id": self.id,
            "description": self.description,
            "target": self.target,
            "variables": self.variables,
        });
        Sha256::digest(canonical.to_string().as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    pub fn approved(&self) -> bool {
        self.approved_at.is_some() && self.approved_digest.as_deref() == Some(&self.digest())
    }
    /// The entry ID of the destination's own credential, if it uses one.
    pub fn credential(&self) -> Option<&str> {
        match &self.target {
            Target::Http { auth, .. } => auth.as_ref().map(|a| a.variable.as_str()),
            Target::CloudflareWorkerSecret { token, .. } => Some(token),
        }
    }
    /// Where the request goes, for the person reviewing it and for results.
    pub fn endpoint(&self) -> String {
        match &self.target {
            Target::Http { url, .. } => url.clone(),
            Target::CloudflareWorkerSecret {
                account_id, script, ..
            } => format!(
                "{CLOUDFLARE_API}/client/v4/accounts/{account_id}/workers/scripts/{script}/secrets"
            ),
        }
    }
}

fn entry_id(id: &str) -> Result<String> {
    Ok(uuid::Uuid::parse_str(id)
        .map_err(|_| input("A variable is named by its entry ID (a UUID) from variable_list."))?
        .hyphenated()
        .to_string())
}

fn header_name(name: &str) -> Result<()> {
    let lower = name.to_ascii_lowercase();
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(input("A header name uses letters, digits, - and _."));
    }
    if [
        "host",
        "cookie",
        "content-length",
        "content-type",
        "transfer-encoding",
        "connection",
        "proxy-authorization",
        "te",
        "upgrade",
    ]
    .contains(&lower.as_str())
    {
        return Err(input(format!("The {name} header is set by the host.")));
    }
    Ok(())
}

/// Checks a body template: a JSON object with exactly one `{{value}}` string and no other
/// placeholder.
fn body(template: &Value) -> Result<()> {
    fn count(value: &Value, found: &mut usize) -> Result<()> {
        match value {
            Value::String(text) if text == PLACEHOLDER => *found += 1,
            Value::String(text) if text.contains("{{") => {
                return Err(input(
                    "The body holds the value only as a whole string \"{{value}}\".",
                ));
            }
            Value::Array(items) => items.iter().try_for_each(|v| count(v, found))?,
            Value::Object(map) => map.values().try_for_each(|v| count(v, found))?,
            _ => {}
        }
        Ok(())
    }
    if !template.is_object() || template.to_string().len() > MAX_BODY {
        return Err(input("The body is a JSON object of at most 8 KiB."));
    }
    let mut found = 0;
    count(template, &mut found)?;
    if found != 1 {
        return Err(input(
            "The body must contain the string \"{{value}}\" exactly once.",
        ));
    }
    Ok(())
}

/// What a definition fixes; checked before it is stored, and again before every delivery.
pub fn validate(destination: &Destination) -> Result<()> {
    crate::consumers::validate_launcher_id(&destination.id)?;
    crate::variables::validate_description(&destination.description)?;
    if destination.variables.is_empty() || destination.variables.len() > MAX_VARIABLES {
        return Err(input(
            "A destination names 1 to 8 variables it may receive.",
        ));
    }
    let mut seen = BTreeSet::new();
    for variable in &destination.variables {
        if entry_id(variable)? != *variable || !seen.insert(variable) {
            return Err(input(
                "Name each variable once, by its entry ID from variable_list.",
            ));
        }
    }
    match &destination.target {
        Target::Http {
            url,
            method,
            headers,
            body: template,
            auth,
            confirm_field,
        } => {
            crate::knowledge::validate_url(url).map_err(|_| {
                input("The URL must be https:// on port 443, without credentials or a fragment.")
            })?;
            if url.len() > 2048 || url.contains(PLACEHOLDER) {
                return Err(input("The URL is fixed text of at most 2048 characters."));
            }
            if !["PUT", "POST", "PATCH"].contains(&method.as_str()) {
                return Err(input("The method is PUT, POST or PATCH."));
            }
            if headers.len() > MAX_HEADERS {
                return Err(input("At most 16 fixed headers."));
            }
            for (name, value) in headers {
                header_name(name)?;
                if name.eq_ignore_ascii_case("authorization") {
                    return Err(input(
                        "A credential never goes in a fixed header: use auth with a variable.",
                    ));
                }
                if value.len() > 1024
                    || !value.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
                    || value.contains(PLACEHOLDER)
                {
                    return Err(input(
                        "A header value is fixed visible text of at most 1024 characters.",
                    ));
                }
            }
            body(template)?;
            if let Some(auth) = auth {
                header_name(&auth.header)?;
                if auth.prefix.len() > 32
                    || !auth
                        .prefix
                        .bytes()
                        .all(|b| b == b' ' || b.is_ascii_graphic())
                {
                    return Err(input("An auth prefix is at most 32 visible characters."));
                }
                if entry_id(&auth.variable)? != auth.variable {
                    return Err(input("Name the auth variable by its entry ID."));
                }
                if headers.keys().any(|h| h.eq_ignore_ascii_case(&auth.header)) {
                    return Err(input("The auth header is also a fixed header."));
                }
            }
            if let Some(field) = confirm_field
                && (!field.starts_with('/') || field.len() > 128)
            {
                return Err(input(
                    "confirm_field is a JSON pointer such as /last4, at most 128 characters.",
                ));
            }
        }
        Target::CloudflareWorkerSecret {
            account_id,
            script,
            secret_name,
            token,
        } => {
            if account_id.len() != 32 || !account_id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(input(
                    "A Cloudflare account ID has 32 hexadecimal characters.",
                ));
            }
            if script.is_empty()
                || script.len() > 63
                || !script
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            {
                return Err(input(
                    "A Worker name uses lowercase letters, digits, - and _ (at most 63).",
                ));
            }
            crate::variables::validate_name(secret_name)?;
            if entry_id(token)? != *token {
                return Err(input("Name the Cloudflare token variable by its entry ID."));
            }
        }
    }
    Ok(())
}

/// The request a delivery sends, built in code from the approved definition.
pub struct Request {
    pub url: String,
    pub method: reqwest::Method,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

fn fill(template: &Value, value: &str) -> Value {
    match template {
        Value::String(text) if text == PLACEHOLDER => Value::String(value.to_owned()),
        Value::Array(items) => Value::Array(items.iter().map(|v| fill(v, value)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), fill(v, value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Builds the request for an approved definition. `credential` is the destination's own
/// credential when it has one.
pub fn request(
    destination: &Destination,
    value: &Secret,
    credential: Option<&Secret>,
) -> Result<Request> {
    let header_value = |prefix: &str, secret: &Secret| -> Result<String> {
        let text = format!("{prefix}{}", secret.expose());
        if !text
            .bytes()
            .all(|b| b == b' ' || b == b'\t' || b.is_ascii_graphic())
        {
            return Err(input(
                "The destination's credential must be visible text on one line.",
            ));
        }
        Ok(text)
    };
    match &destination.target {
        Target::Http {
            url,
            method,
            headers,
            body,
            auth,
            ..
        } => {
            let mut sent: Vec<(String, String)> = headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if let Some(auth) = auth {
                let credential =
                    credential.ok_or_else(|| input("The destination's credential is missing."))?;
                sent.push((auth.header.clone(), header_value(&auth.prefix, credential)?));
            }
            Ok(Request {
                url: url.clone(),
                method: method
                    .parse()
                    .map_err(|_| input("The method is PUT, POST or PATCH."))?,
                headers: sent,
                body: fill(body, value.expose()).to_string().into_bytes(),
            })
        }
        Target::CloudflareWorkerSecret { secret_name, .. } => {
            let credential =
                credential.ok_or_else(|| input("The Cloudflare token variable is missing."))?;
            Ok(Request {
                url: destination.endpoint(),
                method: reqwest::Method::PUT,
                headers: vec![("Authorization".into(), header_value("Bearer ", credential)?)],
                body: json!({"name": secret_name, "text": value.expose(), "type": "secret_text"})
                    .to_string()
                    .into_bytes(),
            })
        }
    }
}

/// What a delivery established. Never the response body or headers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Outcome {
    /// `ok`, `rejected` (the destination refused the credential), `failed` or `unreachable`.
    pub status: &'static str,
    pub http_status: Option<u16>,
    /// The destination's reply confirms the value it stored (computed here; the reply itself is
    /// never forwarded).
    pub confirmed_by_destination: bool,
}

fn last_four(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    chars[chars.len().saturating_sub(4)..].iter().collect()
}

/// Whether a response body confirms the delivered value, by the destination's rule.
pub fn confirmed(destination: &Destination, body: &[u8], value: &Secret) -> bool {
    let Ok(reply) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    match &destination.target {
        Target::Http {
            confirm_field: Some(field),
            ..
        } => reply
            .pointer(field)
            .and_then(Value::as_str)
            .is_some_and(|seen| {
                value.expose().chars().count() >= 8
                    && crate::security::constant_eq(seen, &last_four(value.expose()))
            }),
        Target::Http { .. } => false,
        Target::CloudflareWorkerSecret { secret_name, .. } => {
            reply["success"] == true && reply["result"]["name"] == secret_name.as_str()
        }
    }
}

/// Sends a request with a client that refuses redirects, and classifies the answer. `client`
/// is built by the caller (pinned to public addresses in production).
pub async fn send(
    client: &reqwest::Client,
    destination: &Destination,
    request: Request,
    value: &Secret,
) -> Outcome {
    let mut builder = client
        .request(request.method, &request.url)
        .header("Content-Type", "application/json")
        .header("User-Agent", "personal-teams-assistant-delivery")
        .body(request.body);
    for (name, value) in request.headers {
        builder = builder.header(name, value);
    }
    let unreachable = Outcome {
        status: "unreachable",
        http_status: None,
        confirmed_by_destination: false,
    };
    let Ok(mut response) = builder.send().await else {
        return unreachable;
    };
    let status = response.status();
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if body.len() + chunk.len() > MAX_RESPONSE {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Outcome {
        status: if status.is_success() {
            "ok"
        } else if status.as_u16() == 401 || status.as_u16() == 403 {
            "rejected"
        } else {
            "failed"
        },
        http_status: Some(status.as_u16()),
        confirmed_by_destination: status.is_success() && confirmed(destination, &body, value),
    }
}

/// The destinations of one installation (or of the account it is signed in to, qualified by
/// `owner` so another account's list is never reused).
pub struct Destinations {
    path: PathBuf,
    owner: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    destinations: Vec<Destination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
}

impl Destinations {
    pub fn new(profile: &Path) -> Self {
        Self {
            path: profile.join("deliveries.json"),
            owner: None,
        }
    }
    pub fn account(profile: &Path, owner: &str) -> Self {
        Self {
            path: profile.join("deliveries-account.json"),
            owner: Some(owner.into()),
        }
    }
    fn load(&self) -> Result<File> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<File>(&bytes)
                .map_err(|_| {
                    Error::Storage(
                        "The delivery destinations are unreadable; nothing was sent.".into(),
                    )
                })
                .map(|file| {
                    if file.owner == self.owner {
                        file
                    } else {
                        File::default()
                    }
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
            Err(error) => Err(error.into()),
        }
    }
    fn save(&self, file: &File) -> Result<()> {
        let content = serde_json::to_string_pretty(&File {
            destinations: file.destinations.clone(),
            owner: self.owner.clone(),
        })
        .map_err(|_| Error::Storage("The delivery destinations could not be saved.".into()))?;
        use std::io::Write;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| input("Invalid profile."))?;
        crate::security::private_dir(parent)?;
        let temp = parent.join(format!(".deliveries-{}", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut handle = options.open(&temp)?;
            crate::security::protect_file(&temp)?;
            handle.write_all(content.as_bytes())?;
            handle.sync_all()?;
            crate::security::replace_private_file(&temp, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
    pub fn list(&self) -> Result<Vec<Destination>> {
        let mut list = self.load()?.destinations;
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }
    pub fn get(&self, id: &str) -> Result<Destination> {
        self.load()?
            .destinations
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(|| input("There is no delivery destination with that name."))
    }
    /// Adds a proposal; an existing name is a conflict, never an overwrite.
    pub fn add(&self, destination: Destination) -> Result<()> {
        validate(&destination)?;
        let mut file = self.load()?;
        if file.destinations.iter().any(|d| d.id == destination.id) {
            return Err(Error::Conflict(
                "A delivery destination with that name already exists. Remove it first to define it again.".into(),
            ));
        }
        if file.destinations.len() >= MAX_DESTINATIONS {
            return Err(input("At most 32 delivery destinations can be defined."));
        }
        file.destinations.push(Destination {
            approved_at: None,
            approved_digest: None,
            ..destination
        });
        self.save(&file)
    }
    /// Records a person's approval of the definition they saw, named by its digest.
    pub fn approve(&self, id: &str, digest: &str, now: i64) -> Result<Destination> {
        let mut file = self.load()?;
        let destination = file
            .destinations
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| input("There is no delivery destination with that name."))?;
        validate(destination)?;
        if destination.digest() != digest {
            return Err(Error::Conflict(
                "This destination changed since it was shown. Review it again before approving."
                    .into(),
            ));
        }
        destination.approved_at = Some(now);
        destination.approved_digest = Some(digest.to_owned());
        let approved = destination.clone();
        self.save(&file)?;
        Ok(approved)
    }
    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut file = self.load()?;
        let before = file.destinations.len();
        file.destinations.retain(|d| d.id != id);
        let removed = file.destinations.len() != before;
        if removed {
            self.save(&file)?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests;
