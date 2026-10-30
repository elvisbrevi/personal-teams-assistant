//! Account readiness: one report that tells a person (or the agent helping them) how far this
//! installation is from being a working member of their assistant account, and what exactly to
//! do next.
//!
//! The report only *reads*. Every fact comes from an operation that already exists (standing,
//! account configuration, variable catalog, migration journal, machines, update status), so there
//! is no second implementation of sign-in, storage, synchronization or updates here. It never
//! signs in, trusts a machine, enters or reveals a value, grants a directory, installs a provider,
//! changes an audience, starts Teams or touches a binary: a step that needs one of those names the
//! person or the operation that does it. Nothing in it is a secret: variables appear by name,
//! version and presence, never by value.
//!
//! Each step is one of the areas a setup has to get right, in the order a person meets them, and
//! says where it applies: to the account on the coordinator, or to this computer only. Different
//! states are never merged: a machine that is *configured* for Teams is not *running* it, and a
//! running assistant has not necessarily *received* or *sent* anything.
use super::{
    authority::{self, Transport},
    control::{self, Reply},
    implementation, maintenance, portable, variables as vars, *,
};
use crate::{state::Store, variables as vault};
use serde_json::{Value, json};

const MAX_EXPECTED: usize = 20;

pub(super) fn handles(method: &str) -> bool {
    method == "setup_readiness"
}

// Fixed texts: the GUI translates them by exact match, so a change here needs its catalog entry.
const STANDALONE: &str = "This installation works on its own. To use it with your assistant account, choose your coordinator and sign in once.";
const SIGNED_OUT: &str = "This computer is not signed in to the coordinator it used before. Sign in again and choose Trust this machine.";
const PENDING: &str = "A sign-in is waiting for your approval in the browser. Approve it there, then finish it here. Nothing is trusted until you do.";
const ENDED: &str = "The last sign-in request ended without trusting this computer (it was denied or it expired). Start it again.";
const CONNECTED: &str = "This computer is signed in to your account's coordinator.";
const PORTAL: &str = "This is a portal account. Coding machines are added from your own computer; opening the portal in a browser never registers this device as one.";
const STANDING_LIVE: &str = "The coordinator answered and accepts this computer.";
const STANDING_UNKNOWN: &str = "The coordinator has not been checked since this host started.";
const LISTED: &str =
    "This computer is on your account's machine list, which every signed-in client shows.";
const NOT_LISTED: &str =
    "This computer is not on the coordinator's machine list. Sign in again to register it.";
const REVOKED_HERE: &str =
    "The coordinator lists this computer as revoked. Sign in again to trust it.";
const LIST_UNKNOWN: &str = "The machine list could not be read from the coordinator right now.";
const BROWSER_ONLY: &str =
    "This browser only views the account: it is not a coding machine and gets no directory access.";
const CONFIG_SYNCED: &str =
    "Your account configuration is restored and up to date on this computer.";
const CONFIG_NEVER: &str = "Your account configuration has not been restored on this computer yet. Synchronize to restore it.";
const CONFIG_STALE: &str = "The account has a newer configuration than this computer loaded. Synchronize to bring it here.";
const CONFIG_DIVERGED: &str = "This computer has configuration changes that were never saved to the account. Nothing was replaced; review them before synchronizing.";
const CONFIG_LOCAL: &str = "This computer was configured before it signed in, so the account's configuration was not loaded over it. Replace this computer's settings with the account's (a private backup is kept) to restore them.";
const CONFIG_CONFLICT: &str = "A configuration change conflicted with a newer one. Synchronize, review the result and try again.";
const CONFIG_ERROR: &str = "The last configuration synchronization failed. Synchronize again; the account and this computer were left as they were.";
const CONFIG_BUSY: &str = "The account configuration is synchronizing now.";
const CONFIG_OWN: &str = "This installation holds the account configuration itself.";
const VARIABLES_OK: &str = "The account's variables are available by name.";
const VARIABLES_NONE: &str = "No variable has been stored yet. Add one when something needs it; the value is typed by you, never by the agent.";
const VARIABLES_ERROR: &str = "The variable catalog could not be read or synchronized. Synchronize again; no value was needed for this check.";
const VARIABLES_MISSING: &str = "Some variables you need are not stored yet. Type each value in the protected entry (Settings > Variables, or `pta variables entry complete`); it never goes through chat.";
const VARIABLES_WAITING: &str = "A protected entry is waiting for you to type the value. Finish it in Settings > Variables or with `pta variables entry complete`.";
const VARIABLES_BROKEN: &str =
    "A consumer uses a variable that no longer exists. Choose another variable for it.";
const MIGRATION_DONE: &str = "The credentials you chose were moved to your account and verified.";
const MIGRATION_OPEN: &str = "Optional: review which existing credentials of this installation to move into your account. Nothing is moved until you confirm each one.";
const PROVIDERS_OK: &str =
    "Every model provider in the configuration is installed and signed in on this computer.";
const PROVIDERS_MISSING: &str = "A model provider in the configuration is not installed or signed in on this computer. Nothing is installed or used in its place; install it and sign in yourself, then check again.";
const PROVIDERS_NONE: &str =
    "No model provider is turned on. Turn one on in Settings > Language models.";
const ROOTS_NONE: &str = "No directory is selected for coding agents on this computer. Registration never grants access; choose the folders yourself with the directory selector.";
const ROOTS_SET: &str =
    "Coding agents on this computer may work only in the directories you selected.";
const ROOTS_PORTAL: &str =
    "Your portal account does not run coding agents. Add your personal computer to run them there.";
const TEAMS_OTHER: &str =
    "Another machine of your account is selected to answer Teams, so this one does not.";
const TEAMS_SIGN_IN: &str =
    "Microsoft is not signed in on this computer. Sign in to read and answer Teams as you.";
const TEAMS_IDLE: &str = "Teams is configured here but the assistant is stopped. Start it yourself when you are ready; nothing starts automatically.";
const TEAMS_RUNNING: &str = "The assistant is running here. Running is not the same as having received or sent anything; see the counts.";
const VERSIONS_OK: &str = "This installation runs the newest known release.";
const VERSIONS_NEWER: &str =
    "A newer release is available. Updating is a separate, confirmed action.";
const VERSIONS_RESTART: &str = "A newer version is installed than the one running. Restart the application to use it, then reconnect agents that use `pta mcp`.";
const VERSIONS_MANAGED: &str =
    "The server's updates are applied by its operator; there is nothing to install from here.";
const VERSIONS_BUSY: &str =
    "An update is being prepared or applied. Check its status before doing anything else.";
const FAILED_READ: &str = "This could not be read right now.";

fn gui(place: &str) -> Value {
    json!({"kind":"gui","where":place})
}
fn cli(command: &str) -> Value {
    json!({"kind":"cli","command":command})
}
fn tool(name: &str, arguments: Value) -> Value {
    json!({"kind":"tool","name":name,"arguments":arguments})
}

struct Step {
    id: &'static str,
    /// `coordinator` (the account) or `this_machine` (only this computer).
    scope: &'static str,
    required: bool,
    state: &'static str,
    detail: &'static str,
    facts: Value,
    actions: Vec<Value>,
}

impl Step {
    fn new(id: &'static str, scope: &'static str, required: bool) -> Self {
        Self {
            id,
            scope,
            required,
            state: "unknown",
            detail: FAILED_READ,
            facts: json!({}),
            actions: Vec::new(),
        }
    }
    fn is(mut self, state: &'static str, detail: &'static str) -> Self {
        self.state = state;
        self.detail = detail;
        self
    }
    fn facts(mut self, facts: Value) -> Self {
        self.facts = facts;
        self
    }
    fn act(mut self, action: Value) -> Self {
        self.actions.push(action);
        self
    }
    /// Whether the step still needs somebody.
    fn open(&self) -> bool {
        matches!(self.state, "todo" | "waiting" | "attention" | "unknown")
    }
    fn view(&self) -> Value {
        json!({
            "id": self.id, "scope": self.scope, "required": self.required,
            "state": self.state, "detail": self.detail, "facts": self.facts, "actions": self.actions,
        })
    }
}

fn ask_args(method: &str, args: Value) -> control::Request {
    let mut request = control::Request::new(method);
    request.args = args;
    request
}

/// The names the caller wants to be sure about, checked like any variable name.
fn expected(args: &Value) -> Result<Vec<String>, &'static str> {
    const INVALID: &str = "Invalid arguments.";
    let object = args.as_object().ok_or(INVALID)?;
    if object.keys().any(|key| key != "expect") {
        return Err(INVALID);
    }
    let Some(list) = object.get("expect") else {
        return Ok(Vec::new());
    };
    let list = list.as_array().ok_or(INVALID)?;
    if list.len() > MAX_EXPECTED {
        return Err("Ask about at most 20 variables at once.");
    }
    let mut names: Vec<String> = Vec::new();
    for item in list {
        let name = item.as_str().ok_or(INVALID)?;
        if vault::validate_name(name).is_err() {
            return Err(
                "Variable names are letters, digits and _ (up to 128), not starting with a digit.",
            );
        }
        if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> Reply {
    let invocation = match authority::invocation(&host.state) {
        Ok(invocation) => invocation,
        Err(_) => return Reply::error("state_conflict", 6, authority::ACCOUNT_UNAVAILABLE),
    };
    if invocation.transport == Transport::Machine {
        return Reply::error(
            "authority_denied",
            6,
            "This operation is not available from this client.",
        );
    }
    let expect = match expected(&request.args) {
        Ok(expect) => expect,
        Err(message) => return Reply::error("invalid_input", 2, message),
    };
    let link = match implementation::link(&host.state) {
        Ok(link) => link,
        Err(_) => {
            return Reply::error(
                "operation_failed",
                1,
                "The coordinator connection is unreadable.",
            );
        }
    };
    let shared = invocation.scope.shared && !invocation.scope.owner;
    let browser = invocation.transport == Transport::Browser;
    let local = !browser && invocation.scope.owner;

    // The exchanges with the coordinator come first: they are what tells the standing.
    let configuration = configuration(host, link.is_some()).await;
    let variables = variables(host, &expect).await;
    let machines = machines(host, link.as_ref()).await;
    let steps = vec![
        coordinator(host, link.as_ref(), shared, &machines).await,
        connection(host, link.as_ref()),
        listed(&machines, link.is_some(), browser),
        configuration,
        variables,
        migration_step(host, link.is_some()).await,
        providers(host).await,
        roots(host, shared).await,
        teams(host, &machines, link.is_some()).await,
        versions(host).await,
    ];

    let required_open = steps.iter().any(|s| s.required && s.open());
    let next = steps
        .iter()
        .find(|s| s.required && s.open())
        .or_else(|| steps.iter().find(|s| s.open() && s.state != "unknown"))
        .map(|s| json!({"step":s.id,"state":s.state,"detail":s.detail,"actions":s.actions}));
    // A standalone installation other computers signed in to is a coordinator: it has no
    // account to sign in to.
    let coordinating = link.is_none()
        && !shared
        && machines.list.as_ref().is_some_and(|list| {
            list["machines"].as_array().is_some_and(|all| {
                all.iter()
                    .any(|m| m["local"] != true && m["trust"] != "revoked")
            })
        });
    let mode = if shared {
        "portal_account"
    } else if link.is_some() {
        "signed_in"
    } else {
        "standalone"
    };
    Reply::success(json!({
        "ready": !required_open,
        "mode": mode,
        "client": match invocation.transport {
            Transport::Browser => "browser",
            Transport::Window => "window",
            _ => "local",
        },
        "local_owner": local,
        "coordinator": link.as_ref().map(|l| l.origin.clone()),
        "coordinating": coordinating,
        "steps": steps.iter().map(Step::view).collect::<Vec<_>>(),
        "next": next,
        "agent": {
            "skill_version": env!("CARGO_PKG_VERSION"),
            "values": "No value is ever returned here or accepted in chat; the user types each one in the protected entry.",
            "after_update": "MCP clients keep the tool catalog they loaded: reconnect them after an update.",
        },
    }))
}

// ---------------------------------------------------------------------------------------------
// What the coordinator says about machines.

struct Machines {
    /// `None` when the list could not be read.
    list: Option<Value>,
    login: Value,
}

async fn machines(host: &Arc<Host>, link: Option<&implementation::Link>) -> Machines {
    let list = match link {
        // A live exchange: it is also what records whether the coordinator still accepts us.
        Some(link) => match vars::call_account(&host.state, "machine_list", &json!({})).await {
            Ok(Some(reply)) if reply["ok"] == true && reply["data"]["authoritative"] == true => {
                let mut list = reply["data"].clone();
                list["this_machine"] = json!(link.machine);
                Some(list)
            }
            _ => None,
        },
        None => implementation::operate(host, "machine_list", &json!({}))
            .await
            .ok(),
    };
    // A sign-in in progress is a state to report, whichever way it ends.
    let login = if link.is_some() {
        Value::Null
    } else {
        implementation::operate(host, "machine_login_status", &json!({}))
            .await
            .unwrap_or(Value::Null)
    };
    Machines { list, login }
}

async fn coordinator(
    host: &Arc<Host>,
    link: Option<&implementation::Link>,
    shared: bool,
    machines: &Machines,
) -> Step {
    let step = Step::new("coordinator", "coordinator", !shared);
    if shared {
        return step.is("not_applicable", PORTAL);
    }
    if let Some(link) = link {
        return step
            .is("done", CONNECTED)
            .facts(json!({"coordinator":link.origin}));
    }
    let remembered = implementation::operate(host, "machine_settings", &json!({}))
        .await
        .ok()
        .and_then(|s| s["coordinator_origin"].as_str().map(str::to_owned));
    let login = machines.login["state"].as_str().unwrap_or("none");
    let facts = json!({"coordinator":remembered,"sign_in":login});
    let finish = cli("pta auth machine finish --wait");
    match login {
        "pending" | "approved" => step
            .is("waiting", PENDING)
            .facts(machines.login.clone())
            .act(finish)
            .act(cli("pta auth machine cancel")),
        "denied" | "expired" => step
            .is("todo", ENDED)
            .facts(facts)
            .act(gui("Sidebar > Sign in"))
            .act(cli("pta auth machine login URL")),
        _ if remembered.is_some() => step
            .is("todo", SIGNED_OUT)
            .facts(facts)
            .act(gui("Sidebar > Sign in"))
            .act(cli("pta auth machine login")),
        _ => step
            .is("optional", STANDALONE)
            .facts(facts)
            .act(gui("Sidebar > Sign in"))
            .act(cli("pta auth machine login URL")),
    }
}

fn connection(host: &Arc<Host>, link: Option<&implementation::Link>) -> Step {
    let step = Step::new("connection", "coordinator", link.is_some());
    let Some(link) = link else {
        return step.is("not_applicable", STANDALONE);
    };
    let view = host.state.standing.view(Some(link));
    let facts = json!({
        "standing": view["state"], "checked_at": view["checked_at"], "last_live_at": view["last_live_at"],
        "variables_usable": view["variables_usable"],
    });
    let sign_in = cli("pta auth machine login");
    let step = step.facts(facts);
    match view["state"].as_str().unwrap_or("unknown") {
        "live" => step.is("done", STANDING_LIVE),
        "offline" => step
            .is("attention", standing::OFFLINE)
            .act(cli("pta account sync")),
        "revoked" => step
            .is("todo", standing::REVOKED)
            .act(gui("Sidebar > Sign in"))
            .act(sign_in),
        _ => step
            .is("unknown", STANDING_UNKNOWN)
            .act(cli("pta account sync")),
    }
}

fn listed(machines: &Machines, connected: bool, browser: bool) -> Step {
    let step = Step::new("machine", "coordinator", connected);
    if browser {
        return step.is("not_applicable", BROWSER_ONLY);
    }
    if !connected {
        return step.is("not_applicable", STANDALONE);
    }
    let Some(list) = &machines.list else {
        return step
            .is("unknown", LIST_UNKNOWN)
            .act(cli("pta account sync"));
    };
    let own = list["this_machine"].as_str().unwrap_or_default();
    let entry = list["machines"]
        .as_array()
        .and_then(|all| all.iter().find(|m| m["id"] == own && !own.is_empty()));
    let facts = json!({
        "machines": list["machines"].as_array().map_or(0, Vec::len),
        "online": list["machines"].as_array().map_or(0, |all| all.iter().filter(|m| m["online"] == true).count()),
    });
    let step = step.facts(facts);
    match entry {
        Some(m) if m["trust"] == "revoked" => step
            .is("todo", REVOKED_HERE)
            .act(gui("Sidebar > Sign in"))
            .act(cli("pta auth machine login")),
        Some(_) => step.is("done", LISTED),
        None => step
            .is("todo", NOT_LISTED)
            .act(gui("Sidebar > Sign in"))
            .act(cli("pta auth machine login")),
    }
}

// ---------------------------------------------------------------------------------------------
// Configuration and variables.

async fn configuration(host: &Arc<Host>, connected: bool) -> Step {
    // An installation signed in to an account is not set up until the account's configuration is here.
    let step = Step::new("configuration", "coordinator", connected);
    let reply = portable::operate(host, &ask_args("portable_status", json!({}))).await;
    if !reply.ok {
        return step.act(cli("pta account sync"));
    }
    let sync = &reply.data["sync"];
    let facts = json!({
        "mode": sync["mode"], "state": sync["state"], "loaded": sync["loaded"], "saved": sync["saved"],
        "synced_at": sync["synced_at"], "local_changes": sync["local_changes"],
        "unavailable": {
            "sources": sync["unavailable"]["sources"].as_array().map_or(0, Vec::len),
            "questions": sync["unavailable"]["questions"].as_array().map_or(0, Vec::len),
        },
    });
    let sync_now = cli("pta account sync");
    let step = step.facts(facts);
    match sync["state"].as_str().unwrap_or_default() {
        "authoritative" => step.is("done", CONFIG_OWN),
        "synced" => step.is("done", CONFIG_SYNCED),
        "syncing" => step.is("waiting", CONFIG_BUSY),
        "never" if sync["error"].is_null() && sync["local_changes"] == true => step
            .is("attention", CONFIG_LOCAL)
            .act(cli("pta account sync --reset")),
        "never" if sync["error"].is_null() => step.is("todo", CONFIG_NEVER).act(sync_now),
        "stale" => step.is("attention", CONFIG_STALE).act(sync_now),
        "diverged" => step
            .is("attention", CONFIG_DIVERGED)
            .act(cli("pta account status")),
        "conflict" => step
            .is("attention", CONFIG_CONFLICT)
            .act(cli("pta account status")),
        _ => step.is("attention", CONFIG_ERROR).act(sync_now),
    }
}

async fn variables(host: &Arc<Host>, expect: &[String]) -> Step {
    let step = Step::new("variables", "coordinator", !expect.is_empty());
    let listing = Box::pin(vars::operate(host, &ask_args("variable_list", json!({})))).await;
    if !listing.ok {
        return step
            .is("attention", VARIABLES_ERROR)
            .act(cli("pta variables sync"));
    }
    let stored = listing.data["variables"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let pending = listing.data["pending"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut missing = false;
    let mut waiting = false;
    let named: Vec<Value> = expect
        .iter()
        .map(|name| {
            let found = stored
                .iter()
                .find(|v| v["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)));
            let entry = pending
                .iter()
                .find(|p| p["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)));
            match (found, entry) {
                (Some(v), _) => json!({"name":name,"state":"present","id":v["id"],"version":v["version"]}),
                (None, Some(p)) => {
                    waiting = true;
                    json!({"name":name,"state":"waiting","request":p["request"],"expires_at":p["expires_at"]})
                }
                (None, None) => {
                    missing = true;
                    json!({"name":name,"state":"missing"})
                }
            }
        })
        .collect();
    // A consumer that points at a variable that is gone cannot be used.
    let consumers = Box::pin(vars::operate(
        host,
        &ask_args("variable_consumer_list", json!({})),
    ))
    .await;
    let broken = consumers.ok
        && consumers.data["consumers"].as_array().is_some_and(|all| {
            all.iter().any(|c| {
                c["binding"]["state"]
                    .as_str()
                    .is_some_and(|state| state != "ok")
            })
        });
    let facts = json!({
        "count": listing.data["total"], "sync": listing.data["sync"]["state"],
        "revision": listing.data["sync"]["revision"], "synced_at": listing.data["sync"]["synced_at"],
        "pending_entries": pending.len(), "expected": named,
    });
    let mut step = step.facts(facts);
    for item in &named {
        if item["state"] == "missing" {
            step = step
                .act(tool(
                    "variable_entry_request",
                    json!({"name":item["name"],"description":"<what the variable is for>"}),
                ))
                .act(gui("Settings > Variables"));
            break;
        }
    }
    if waiting {
        step = step.act(tool(
            "variable_entry_status",
            json!({"request":"<request from facts>"}),
        ));
    }
    let sync_state = listing.data["sync"]["state"].as_str().unwrap_or_default();
    if matches!(sync_state, "error" | "conflict") {
        return step
            .is("attention", VARIABLES_ERROR)
            .act(cli("pta variables sync"));
    }
    if missing {
        return step.is("todo", VARIABLES_MISSING);
    }
    if waiting {
        return step.is("waiting", VARIABLES_WAITING);
    }
    if broken {
        return step
            .is("attention", VARIABLES_BROKEN)
            .act(cli("pta variables consumers"));
    }
    if expect.is_empty() && stored.is_empty() {
        return step.is("optional", VARIABLES_NONE);
    }
    step.is("done", VARIABLES_OK)
}

async fn migration_step(host: &Arc<Host>, connected: bool) -> Step {
    let step = Step::new("migration", "this_machine", false);
    let reply = Box::pin(vars::operate(
        host,
        &ask_args("variable_migration_status", json!({})),
    ))
    .await;
    if !reply.ok {
        return step.is("not_applicable", FAILED_READ);
    }
    let items = reply.data["items"].as_array().map_or(0, Vec::len);
    let facts = json!({"moved": items, "complete": reply.data["complete"]});
    let step = step.facts(facts);
    if reply.data["complete"] == true {
        return step.is("done", MIGRATION_DONE);
    }
    if !connected && items == 0 {
        return step.is("not_applicable", STANDALONE);
    }
    step.is("optional", MIGRATION_OPEN)
        .act(cli("pta variables migrate inspect"))
        .act(gui("Settings > Credentials > Move existing credentials"))
}

// ---------------------------------------------------------------------------------------------
// This computer.

async fn providers(host: &Arc<Host>) -> Step {
    let step = Step::new("providers", "this_machine", false);
    let Ok(config) = read_config(&host.state.config_path) else {
        return step;
    };
    if config.llm.active().is_empty() {
        return step
            .is("todo", PROVIDERS_NONE)
            .act(gui("Settings > Language models"));
    }
    let needed = portable::local_setup(host, &crate::portable::Unavailable::default()).await;
    let missing: Vec<&Value> = needed
        .iter()
        .filter(|item| {
            item["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("provider:"))
        })
        .collect();
    if missing.is_empty() {
        return step.is("done", PROVIDERS_OK);
    }
    // The message names the provider; signing in to it is always the person's action.
    let names: Vec<&str> = missing
        .iter()
        .filter_map(|item| item["id"].as_str()?.strip_prefix("provider:"))
        .collect();
    step.is("attention", PROVIDERS_MISSING)
        .facts(json!({"providers": names}))
        .act(gui("Settings > Language models > Detect CLIs again"))
}

async fn roots(host: &Arc<Host>, shared: bool) -> Step {
    let step = Step::new("roots", "this_machine", false);
    if shared {
        return step.is("not_applicable", ROOTS_PORTAL);
    }
    let Ok(roots) = implementation::roots(host).await else {
        return step;
    };
    let facts = json!({"count": roots.len(), "roots": roots});
    if roots.is_empty() {
        return step
            .is("optional", ROOTS_NONE)
            .facts(facts)
            .act(gui("Settings > Execution machines > Choose directories"))
            .act(tool("machine_browse", json!({})));
    }
    step.is("done", ROOTS_SET).facts(facts)
}

async fn teams(host: &Arc<Host>, machines: &Machines, connected: bool) -> Step {
    let step = Step::new("teams", "this_machine", false);
    let Ok(config) = read_config(&host.state.config_path) else {
        return step;
    };
    let db = config.server.data_dir.join("assistant.db");
    let signed_in = Store::has_token(&db).unwrap_or(false);
    let running = host.state.running.lock().await.is_some();
    let (subscriptions, _) = Store::subscription_health(&db).unwrap_or((0, None));
    // «Received» and «sent» come from what was recorded, never from the configuration.
    let mut jobs = Store::inspect(&db, false, 1000, None, true).unwrap_or_default();
    // A simulation or test chat is not a Teams message.
    jobs.retain(|j| {
        !j["resource"]
            .as_str()
            .unwrap_or_default()
            .starts_with("simulation:")
    });
    let received_at = jobs.iter().filter_map(|j| j["created_at"].as_i64()).max();
    let sent: Vec<&Value> = jobs
        .iter()
        .filter(|j| !j["audit"]["sent"].is_null())
        .collect();
    let sent_at = sent.iter().filter_map(|j| j["created_at"].as_i64()).max();
    let own = machines
        .list
        .as_ref()
        .and_then(|l| l["this_machine"].as_str())
        .unwrap_or_default();
    let selected = machines
        .list
        .as_ref()
        .and_then(|l| l["teams_machine"].as_str());
    let facts = json!({
        "microsoft_signed_in": signed_in, "selected_here": selected.is_none_or(|s| s == own || own.is_empty()),
        "running": running, "active_subscriptions": subscriptions,
        "received": jobs.len(), "last_received_at": received_at,
        "sent": sent.len(), "last_sent_at": sent_at,
        "mode": if config.policy.dry_run { "observe" } else { "active" },
    });
    let step = step.facts(facts);
    if connected && selected.is_some_and(|s| s != own) {
        return step
            .is("optional", TEAMS_OTHER)
            .act(gui("Settings > Execution machines > Machine for Teams"));
    }
    if !signed_in {
        return step
            .is("todo", TEAMS_SIGN_IN)
            .act(gui("Settings > Microsoft Teams"))
            .act(cli("pta auth microsoft login"));
    }
    if !running {
        return step.is("todo", TEAMS_IDLE).act(cli("pta start"));
    }
    step.is("done", TEAMS_RUNNING)
}

async fn versions(host: &Arc<Host>) -> Step {
    let step = Step::new("versions", "this_machine", false);
    let reply = maintenance::operate(host, &ask_args("crate_update_status", json!({}))).await;
    if !reply.ok {
        return step;
    }
    let data = &reply.data;
    let facts = json!({
        "running": data["running"], "installed": data["installed"], "available": data["available"],
        "state": data["state"], "managed": data["managed"], "skill": env!("CARGO_PKG_VERSION"),
    });
    let step = step.facts(facts);
    if data["managed"] == "server" {
        return step.is("not_applicable", VERSIONS_MANAGED);
    }
    if matches!(
        data["state"].as_str(),
        Some("staging" | "applying" | "deferred")
    ) {
        return step
            .is("waiting", VERSIONS_BUSY)
            .act(cli("pta update status"));
    }
    if data["restart_required"] == true {
        return step
            .is("attention", VERSIONS_RESTART)
            .act(cli("pta app quit"));
    }
    if data["available"].is_string() {
        return step
            .is("optional", VERSIONS_NEWER)
            .act(cli("pta update status"))
            .act(gui("Settings > Updates"));
    }
    step.is("done", VERSIONS_OK)
}

#[cfg(test)]
mod tests;
