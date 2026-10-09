use crate::app::control::{self, Reply, Request};
use crate::{config::Config, knowledge::KnowledgeMap};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{IsTerminal, Read},
    time::Duration,
};

const HELP: &str = "pta — Personal Teams Assistant administration

Usage: pta [--json] [--non-interactive] COMMAND [ARGUMENTS]
       pta help | pta --help | pta -h     shows this help
       pta --version                       CLI and contract version

Assistant (Teams service)
  start                    Starts the assistant; opens the hidden host if it is not running.
  status                   Shows whether the host and the assistant are running, the mode, Teams and the models.
  stop                     Stops the assistant (the host stays open).
  restart                  Stops and starts the assistant with the saved configuration.
  mode show|observe|active Current mode; observe = never sends to Teams, active = answers.
  doctor [--offline]       Checks configuration, credentials and account (--offline: local files only).

Application (host)
  app open|hide|quit       Shows or hides the window, or closes the host (stopping the assistant and tunnel first).

Web portal
  web status              Web URL, settings and account profiles (never passwords).
  web configure           JSON on stdin: bind, public_url (HTTPS behind Cloudflare Tunnel), session_hours.
  web users list          Lists web accounts and their isolated profile directories.
  web users add USER      Password on protected stdin (12+ characters); creates an isolated profile.
                           --current-profile instead grants this account access to the existing desktop profile.
  web users password USER Password on protected stdin; revokes existing sessions.
  web users disable|enable USER  Revokes sessions and disables or enables web access; preserves data.
                           Run the portal with personal-teams-assistant --web; no public registration.

Configuration
  config show              Saved configuration.
  config get FIELD         Reads a field by dotted path (e.g. policy.dry_run).
  config set FIELD JSON    Changes a field; the value is JSON (true, \"text\", [...], {...}).
  config apply|validate    Applies or validates {config,map,tunnel_config} as JSON or TOML on stdin.
  config import FILE       Imports an existing config.toml (with the assistant stopped).
  credentials list         Credentials and their origin, without values.
  credentials set NAME     Stores a credential read from stdin (never as an argument).
  credentials delete NAME
  tunnel status|validate   Status and local validation of the Cloudflare tunnel.
  tunnel configure token|external|file PATH

Language models
  llm providers            Detected CLIs (Codex, Claude), their models and efforts, and DeepSeek through its API.
                           Order and fallback: pta config set llm.chain '[{\"provider\":\"codex\",\"model\":\"gpt-6.1-sol\",\"effort\":\"medium\"},...]'
  test providers           Tests each active model with synthetic data (uses quota or API credit).

Messages and diagnostics
  audit list [--limit N] [--content]   Received messages, their outcome and the step log.
  audit show RESOURCE [--content]      One message; --content includes question and answer.
  logs [--limit N]         Host events.
  chat | test simulate     Real pipeline with a SimulationRequest JSON on stdin; never sends to Teams.
  test connectivity        Reads the connected Microsoft account (without sending).

Accounts
  auth microsoft status|login|finish|cancel|logout
                           login [--no-browser]; finish [--wait] [--redirect URL]
                           (--redirect: the http://localhost:PORT/?code=... URL opened on another device)
  auth github status|login CLIENT_ID|finish|cancel|logout
  github repos             Repositories authorized for the GitHub App.

Personal chat
  self-chat status|enable [CHAT_ID]|disable
  self-chat reconcile NONCE MESSAGE_ID  Resolves a pending output.
  test self-chat           Checks the personal chat membership (does not send).

Activity registration
  activity status|pending  Current run, or activities needing context or an HU.
  activity run [DAY]        Reviews and registers work (YYYY-MM-DD, today by default).
  activity history [DAY]    Tasks and runs filtered by day.
  activity resolve ID       JSON on stdin: action=create|refine|dismiss, parent, title, description,
                           hours, context (Other), fields (additional required field values).
  activity open [ID]        Opens the activity review in a separate app window.
                           Configure with config set activity_registration.FIELD JSON.

Knowledge
  repos list | repos add|edit ALIAS PATH | repos remove ALIAS
  repos clone OWNER/REPO ALIAS | repos sync ALIAS
  sources list | sources show ID | sources add | sources edit ID | sources remove ID
  sources enable|disable ID
  sources audience ID      JSON on stdin: allowed_conversations, allowed_senders, external_processing.
  azure wiki list|search|read SOURCE_ID  (search/read: JSON on stdin; local read only)

Agents
  capabilities             Contract and commands as JSON.
  skill show|path|install DIRECTORY     Operating manual for agents.

Options
  --json                   JSON reply {contract, version, ok, code, exit_code, message, data, revision}.
  --non-interactive        Never prompts or opens the browser.

Update: cargo install personal-teams-assistant --locked. The first pta command (or opening the app) stops
the previous host (assistant and tunnel) and asks whether to keep the assistant running with the new version.
New sources start disabled and without audiences. Credentials only on stdin.
Exit: 0 ok, 1 failed, 2 invalid input, 3 not ready, 4 authorization pending, 5 dependency or network, 6 conflict or version.
";
fn stdin_text() -> Result<String> {
    let mut value = String::new();
    std::io::stdin()
        .take(1_000_001)
        .read_to_string(&mut value)?;
    ensure!(value.len() <= 1_000_000, "input is too large");
    Ok(value)
}
fn input() -> Result<Value> {
    let text = stdin_text()?;
    serde_json::from_str(&text)
        .or_else(|_| toml::from_str::<toml::Value>(&text).map(|v| serde_json::to_value(v).unwrap()))
        .context("input must be JSON or TOML")
}
fn positional(args: &[String], index: usize) -> Result<&str> {
    args.get(index)
        .map(String::as_str)
        .context("missing argument; consult --help")
}
async fn call(method: &str, args: Value, revision: Option<String>, launch: bool) -> Result<Reply> {
    control::client(
        Request {
            contract: control::CONTRACT,
            method: method.into(),
            args,
            revision,
        },
        launch,
    )
    .await
}
async fn snapshot() -> Result<Reply> {
    call("snapshot", json!({}), None, true).await
}
fn lookup<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    let mut found = value;
    for part in key.split('.') {
        found = found.get(part).context("unknown field")?;
    }
    Ok(found)
}
fn assign(value: &mut Value, key: &str, next: Value) -> Result<()> {
    let parts: Vec<_> = key.split('.').collect();
    let mut found = value;
    for part in &parts[..parts.len() - 1] {
        found = found.get_mut(*part).context("unknown field")?;
    }
    *found
        .get_mut(parts[parts.len() - 1])
        .context("unknown field")? = next;
    Ok(())
}
fn safe_status_stopped() -> Result<Reply> {
    let dir = control::profile_dir()?;
    if !dir.join("config.toml").exists() {
        return Ok(Reply::success(
            json!({"host_running":false,"running":false,"configured":false,"profile":dir}),
        ));
    }
    let config: Config = toml::from_str(&std::fs::read_to_string(dir.join("config.toml"))?)?;
    Ok(Reply::success(
        json!({"host_running":false,"running":false,"persisted_dry_run":config.policy.dry_run,"loaded_config":null,"profile":dir,"config":config,"credential_metadata":"requires host"}),
    ))
}
fn validate_args(args: &[String]) -> Result<()> {
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--json" | "--non-interactive" | "--offline" | "--no-browser" | "--wait"
            | "--content" | "--current-profile" => {}
            "--limit" => {
                i += 1;
                ensure!(
                    args.get(i).is_some_and(|v| v.parse::<usize>().is_ok()),
                    "invalid limit"
                );
            }
            "--redirect" => {
                i += 1;
                ensure!(
                    args.get(i)
                        .is_some_and(|v| v.starts_with("http://localhost:")),
                    "invalid redirect URL"
                );
            }
            _ => {
                ensure!(!a.starts_with("--"), "unknown flag");
                positional.push(a);
            }
        }
        i += 1;
    }
    let p = &positional;
    let valid = matches!(
        p.as_slice(),
        ["capabilities"
            | "status"
            | "doctor"
            | "start"
            | "stop"
            | "restart"
            | "logs"
            | "chat"
            | "help"]
            | ["app", "open" | "hide" | "quit"]
            | ["web", "status" | "configure"]
            | ["web", "users", "list"]
            | ["web", "users", "add" | "password" | "disable" | "enable", _]
            | ["skill", "show" | "path"]
            | ["skill", "install", _]
            | ["config", "show" | "apply" | "validate"]
            | ["config", "get" | "import", _]
            | ["config", "set", _, _]
            | ["mode", "show" | "observe" | "active"]
            | ["self-chat", "status" | "enable" | "disable"]
            | ["self-chat", "enable", _]
            | ["self-chat", "reconcile", _, _]
            | [
                "activity",
                "status" | "pending" | "run" | "history" | "open"
            ]
            | ["activity", "run" | "history" | "open" | "resolve", _]
            | ["credentials", "list"]
            | ["credentials", "set" | "delete", _]
            | [
                "auth",
                "microsoft",
                "status" | "login" | "finish" | "cancel" | "logout"
            ]
            | ["auth", "github", "status" | "finish" | "cancel" | "logout"]
            | ["auth", "github", "login", _]
            | ["github", "repos"]
            | ["llm", "providers"]
            | ["repos", "list"]
            | ["repos", "add" | "edit" | "clone", _, _]
            | ["repos", "remove" | "sync", _]
            | ["sources", "list" | "add"]
            | [
                "sources",
                "show" | "edit" | "remove" | "enable" | "disable" | "audience",
                _
            ]
            | ["azure", "wiki", "list" | "search" | "read", _]
            | ["tunnel", "status" | "validate"]
            | ["tunnel", "configure", "token" | "external"]
            | ["tunnel", "configure", "file", _]
            | [
                "test",
                "simulate" | "self-chat" | "providers" | "connectivity"
            ]
            | ["audit", "list"]
            | ["audit", "show", _]
    );
    ensure!(valid, "unknown command or invalid argument count");
    for (flag, allowed) in [
        ("--offline", p[0] == "doctor"),
        ("--no-browser", p[0] == "auth" && p.get(2) == Some(&"login")),
        ("--wait", p[0] == "auth" && p.get(2) == Some(&"finish")),
        (
            "--redirect",
            p[0] == "auth" && p.get(1) == Some(&"microsoft") && p.get(2) == Some(&"finish"),
        ),
        ("--content", p[0] == "audit"),
        (
            "--current-profile",
            p.as_slice().starts_with(&["web", "users", "add"]),
        ),
        ("--limit", ["audit", "logs"].contains(&p[0])),
    ] {
        ensure!(
            !args.iter().any(|a| a == flag) || allowed,
            "invalid flag for this command"
        );
    }
    Ok(())
}
/// After `cargo install`, the previous host keeps running from the replaced binary. Stop it
/// (assistant and tunnel first) before the command, and ask whether to keep the assistant
/// running with the new build.
async fn retire_outdated_host(command: &str, action: &str, interactive: bool) -> Result<()> {
    let Ok(installed) = control::installed_host() else {
        return Ok(());
    };
    if !control::outdated_host(&installed) {
        return Ok(());
    }
    eprintln!(
        "A new version of Personal Teams Assistant was installed: stopping the previous one (assistant and tunnel)..."
    );
    let was_running = control::retire_outdated_host().await?;
    eprintln!("Previous version stopped.");
    // These commands decide the assistant's state themselves.
    if matches!(command, "start" | "restart" | "stop") || (command, action) == ("app", "quit") {
        return Ok(());
    }
    if !was_running {
        eprintln!("The assistant was stopped; start it with `pta start` whenever you want.");
        return Ok(());
    }
    if !crate::app::keep_running_after_update(interactive) {
        eprintln!("The assistant stays stopped; start it with `pta start` whenever you want.");
        return Ok(());
    }
    let reply = call("start_assistant", json!({}), None, true).await?;
    if reply.ok {
        eprintln!("The assistant keeps running with the new version.");
    } else {
        eprintln!(
            "The assistant could not start with the new version: {}",
            reply.message.as_deref().unwrap_or(&reply.code)
        );
    }
    Ok(())
}
async fn execute(mut args: Vec<String>) -> Result<Reply> {
    validate_args(&args)?;
    let non_interactive = args.iter().any(|a| a == "--non-interactive");
    let json_output = args.iter().any(|a| a == "--json");
    let offline = args.iter().any(|a| a == "--offline");
    args.retain(|a| a != "--json" && a != "--non-interactive");
    let command = positional(&args, 0)?;
    let action = args.get(1).map(String::as_str).unwrap_or("");
    if command == "web" {
        return super::web::admin(&args);
    }
    if !matches!(command, "capabilities" | "skill") && !offline {
        retire_outdated_host(command, action, !non_interactive && !json_output).await?;
    }
    match command {
        "capabilities" => {
            return Ok(Reply::success(
                json!({"contract":control::CONTRACT,"commands":HELP,"skill_version":env!("CARGO_PKG_VERSION"),"activity_registration_support":true,"web_support":true}),
            ));
        }
        "skill" => return crate::app::skill::command(action, args.get(2).map(String::as_str)),
        "status" if control::existing_host().is_err() => return safe_status_stopped(),
        "stop" if control::existing_host().is_err() => {
            return Ok(Reply::success(json!({"running":false})));
        }
        "app" if action == "quit" && control::existing_host().is_err() => {
            return Ok(Reply::success(
                json!({"quitting":false,"host_running":false}),
            ));
        }
        _ => {}
    }
    if command == "activity" {
        let method = match action {
            "status" => "activity_status",
            "pending" => "activity_pending",
            "run" => "activity_run",
            "history" => "activity_history",
            "resolve" => "activity_resolve",
            "open" => "activity_open",
            _ => unreachable!(),
        };
        let values = match action {
            "run" | "history" => json!({"day":args.get(2)}),
            "open" => json!({"id":args.get(2)}),
            "resolve" => json!({"id":positional(&args,2)?,"resolution":input()?}),
            _ => json!({}),
        };
        return call(method, values, None, true).await;
    }
    if command == "doctor" && args.iter().any(|a| a == "--offline") {
        let dir = control::profile_dir()?;
        let config: Config = toml::from_str(&std::fs::read_to_string(dir.join("config.toml"))?)?;
        let map = KnowledgeMap::load(&config.knowledge_map)?;
        crate::app::validate_local(&config, &map)?;
        return Ok(Reply::success(
            json!({"offline_valid":true,"network_checked":false,"host_running":control::existing_host().is_ok()}),
        ));
    }
    if command == "azure" {
        let operation = positional(&args, 2)?;
        let value = if operation == "list" {
            json!({})
        } else {
            serde_json::from_str::<Value>(&stdin_text()?).context("input must be Wiki JSON")?
        };
        match operation {
            "search" => serde_json::from_value::<crate::ado::wiki::SearchInput>(value.clone())
                .context("invalid Wiki search input")?
                .validate()?,
            "read" => serde_json::from_value::<crate::ado::wiki::ReadInput>(value.clone())
                .context("invalid Wiki read input")?
                .validate()?,
            _ => {}
        }
        return call(
            "azure_wiki",
            json!({"operation":operation,"source":positional(&args,3)?,"input":value}),
            None,
            true,
        )
        .await;
    }
    let direct = match (command, action) {
        ("status", _) => Some("snapshot"),
        ("start", _) => Some("start_assistant"),
        ("stop", _) => Some("stop_assistant"),
        ("restart", _) => Some("restart"),
        ("app", "open") => Some("app_open"),
        ("app", "hide") => Some("app_hide"),
        ("app", "quit") => Some("app_quit"),
        ("github", "repos") => Some("github_repositories"),
        ("llm", "providers") => Some("llm_providers"),
        ("chat", _) | ("test", "simulate") => Some("chat"),
        ("self-chat", "status") => Some("self_chat_status"),
        ("self-chat", "enable") => Some("self_chat_enable"),
        ("self-chat", "disable") => Some("self_chat_disable"),
        ("self-chat", "reconcile") => Some("self_chat_reconcile"),
        ("test", "self-chat") => Some("test_self_chat"),
        ("test", "providers") => Some("test_providers"),
        ("test", "connectivity") => Some("test_connectivity"),
        ("config", "import") => Some("import_existing"),
        ("credentials", "set") => Some("set_credential"),
        ("credentials", "delete") => Some("delete_credential"),
        ("repos", "clone") => Some("clone_github_repository"),
        ("repos", "sync") => Some("update_github_repository"),
        ("audit", "list" | "show") => Some("audit"),
        ("logs", _) => Some("logs"),
        _ => None,
    };
    if let Some(method) = direct {
        let value = match method {
            "chat" => json!({"input":input()?}),
            "self_chat_enable" => json!({"id":args.get(2)}),
            "self_chat_reconcile" => {
                json!({"nonce":positional(&args,2)?,"id":positional(&args,3)?})
            }
            "import_existing" => json!({"path":std::fs::canonicalize(positional(&args,2)?)?}),
            "set_credential" => {
                ensure!(
                    !std::io::stdin().is_terminal(),
                    "input must use a protected pipe for credentials"
                );
                let value = stdin_text()?;
                ensure!(value.len() <= 16_384, "credential too long");
                json!({"name":positional(&args,2)?,"value":value.trim_end_matches(['\r','\n'])})
            }
            "delete_credential" => json!({"name":positional(&args,2)?}),
            "clone_github_repository" => {
                json!({"full_name":positional(&args,2)?,"alias":positional(&args,3)?})
            }
            "update_github_repository" => json!({"alias":positional(&args,2)?}),
            "audit" | "logs" => {
                let limit = args
                    .iter()
                    .position(|a| a == "--limit")
                    .map(|i| {
                        positional(&args, i + 1)?
                            .parse::<usize>()
                            .context("invalid limit")
                    })
                    .transpose()?
                    .unwrap_or(50);
                json!({"limit":limit,"resource":if action=="show" {Some(positional(&args,2)?)} else {None},"content":args.iter().any(|a|a=="--content")})
            }
            _ => json!({}),
        };
        return call(method, value, None, true).await;
    }
    if command == "auth" {
        let provider = action;
        ensure!(
            ["github", "microsoft"].contains(&provider),
            "unknown provider"
        );
        let action = positional(&args, 2)?;
        if action == "status" {
            let mut s = snapshot().await?;
            if s.ok {
                s.data = json!({"provider":provider,"connected":s.data[format!("{provider}_connected")],"revocation":"logout deletes local tokens only"});
            }
            return Ok(s);
        }
        let method = match (provider, action) {
            ("github", "login") => "begin_github_login",
            ("github", "finish") => "finish_github_login",
            ("github", "cancel") => "cancel_github",
            ("github", "logout") => "disconnect_github",
            ("microsoft", "login") => "connect_microsoft",
            ("microsoft", "finish") => "finish_microsoft",
            ("microsoft", "cancel") => "cancel_microsoft",
            ("microsoft", "logout") => "microsoft_logout",
            _ => anyhow::bail!("unknown auth action"),
        };
        let redirect = args
            .iter()
            .position(|a| a == "--redirect")
            .map(|i| positional(&args, i + 1))
            .transpose()?;
        let value = if provider == "github" && action == "login" {
            json!({"client_id":positional(&args,3)?})
        } else if let Some(url) = redirect {
            json!({"redirect_url":url})
        } else {
            json!({"open":!non_interactive && !args.iter().any(|a|a=="--no-browser")})
        };
        let mut reply = call(method, value, None, true).await?;
        if provider == "github" && action == "login" && reply.ok {
            reply.ok = false;
            reply.code = "authorization_pending".into();
            reply.exit_code = 4;
        }
        // A pasted redirect completes in the background; wait for its result.
        if action == "finish" && (args.iter().any(|a| a == "--wait") || redirect.is_some()) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
            while reply.code == "authorization_pending" && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_secs(2)).await;
                reply = call(method, json!({}), None, true).await?;
            }
        }
        return Ok(reply);
    }
    let mut snap = snapshot().await?;
    if !snap.ok {
        return Ok(snap);
    }
    if command == "doctor" {
        let missing = snap.data["credentials"]
            .as_object()
            .context("invalid snapshot")?
            .values()
            .any(|v| v == "missing");
        if missing || !snap.data["microsoft_connected"].as_bool().unwrap_or(false) {
            snap.ok = false;
            snap.code = "not_ready".into();
            snap.exit_code = 3;
        }
        snap.data["network_checked"] = json!(false);
        return Ok(snap);
    }
    let mut data = snap.data.clone();
    match (command, action) {
        ("config", "show") => {
            snap.data = data["config"].clone();
            return Ok(snap);
        }
        ("config", "get") => {
            snap.data = lookup(&data["config"], positional(&args, 2)?)?.clone();
            return Ok(snap);
        }
        ("config", "set") => assign(
            &mut data["config"],
            positional(&args, 2)?,
            serde_json::from_str(positional(&args, 3)?)?,
        )?,
        ("config", "apply" | "validate") => {
            data = input()?;
            if action == "validate" {
                return call("validate", data, None, true).await;
            }
        }
        ("mode", "show") => {
            snap.data = json!({"persisted_dry_run":data["config"]["policy"]["dry_run"],"loaded_dry_run":data["loaded_config"]["policy"]["dry_run"],"running":data["running"]});
            return Ok(snap);
        }
        ("mode", "observe" | "active") => {
            data["config"]["policy"]["dry_run"] = json!(action == "observe")
        }
        ("credentials", "list") => {
            snap.data = data["credentials"].clone();
            return Ok(snap);
        }
        ("repos", "list") => {
            snap.data = data["map"]["repositories"].clone();
            return Ok(snap);
        }
        ("repos", "add" | "edit") => {
            let alias = positional(&args, 2)?;
            ensure!(
                regex::Regex::new(r"^[a-z][a-z0-9_-]{0,63}$")?.is_match(alias),
                "invalid alias"
            );
            let repos = data["map"]["repositories"]
                .as_object_mut()
                .context("invalid map")?;
            ensure!(
                (action == "add") == !repos.contains_key(alias),
                "alias already exists or missing"
            );
            repos.insert(
                alias.into(),
                json!(std::fs::canonicalize(positional(&args, 3)?)?),
            );
        }
        ("repos", "remove") => {
            ensure!(
                data["map"]["repositories"]
                    .as_object_mut()
                    .unwrap()
                    .remove(positional(&args, 2)?)
                    .is_some(),
                "unknown alias"
            );
        }
        ("sources", "list") => {
            snap.data = data["map"]["resources"].clone();
            return Ok(snap);
        }
        ("sources", "add") => {
            let resource = input()?;
            ensure!(
                resource["allowed_senders"]
                    .as_array()
                    .is_none_or(|a| a.is_empty())
                    && resource["allowed_conversations"]
                        .as_array()
                        .is_some_and(Vec::is_empty)
                    && resource["enabled"] == false
                    && resource["external_processing"] == false,
                "add sources disabled, without audiences or external processing; authorize explicitly afterward"
            );
            data["map"]["resources"]
                .as_array_mut()
                .unwrap()
                .push(resource);
        }
        ("sources", "show" | "edit" | "remove" | "enable" | "disable" | "audience") => {
            let id = positional(&args, 2)?;
            let list = data["map"]["resources"].as_array_mut().unwrap();
            let i = list
                .iter()
                .position(|r| r["id"] == id)
                .context("unknown source")?;
            match action {
                "show" => {
                    snap.data = list[i].clone();
                    return Ok(snap);
                }
                "edit" => {
                    let resource = input()?;
                    ensure!(resource["id"] == id, "source ID cannot change");
                    list[i] = resource;
                }
                "remove" => {
                    list.remove(i);
                }
                "enable" | "disable" => list[i]["enabled"] = json!(action == "enable"),
                "audience" => {
                    let fields = input()?;
                    for (key, value) in fields.as_object().context("audience must be an object")? {
                        ensure!(
                            [
                                "allowed_conversations",
                                "allowed_senders",
                                "external_processing"
                            ]
                            .contains(&key.as_str()),
                            "unknown audience field"
                        );
                        list[i][key] = value.clone();
                    }
                }
                _ => unreachable!(),
            }
        }
        ("tunnel", "status") => {
            snap.data = json!({"token_mode":data["config"]["server"]["cloudflare_tunnel"],"file":data["tunnel_config"],"running":data["running"]});
            return Ok(snap);
        }
        ("tunnel", "configure") => {
            let mode = positional(&args, 2)?;
            ensure!(
                ["token", "file", "external"].contains(&mode),
                "unknown tunnel mode"
            );
            data["config"]["server"]["cloudflare_tunnel"] = json!(mode == "token");
            data["tunnel_config"] = if mode == "file" {
                json!(std::fs::canonicalize(positional(&args, 3)?)?)
            } else {
                json!("")
            };
        }
        ("tunnel", "validate") => {
            return call("validate_tunnel", json!({}), None, true).await;
        }
        _ => anyhow::bail!("unknown command; consult --help"),
    }
    if data.get("tunnel_config").is_none() {
        data["tunnel_config"] = snap.data["tunnel_config"].clone();
    }
    call(
        "save_settings",
        json!({"config":data["config"],"map":data["map"],"tunnel_config":data["tunnel_config"]}),
        snap.revision,
        true,
    )
    .await
}
/// Readable output for the lifecycle commands; `--json` keeps the full data.
fn human(command: &str, data: &Value) -> Option<String> {
    match command.split(' ').next()? {
        "status" => Some(status_text(data)),
        "start" | "restart" if data["running"] == true => Some("Assistant running.".into()),
        "stop" if data["running"] == false => Some("Assistant stopped.".into()),
        "app" if command == "app quit" && data["quitting"] == true => {
            Some("Host closed; the assistant and the tunnel stopped.".into())
        }
        "app" if command == "app quit" && data["host_running"] == false => {
            Some("The host was not running.".into())
        }
        _ => None,
    }
}
fn status_text(data: &Value) -> String {
    let yes = |key: &str| data[key].as_bool() == Some(true);
    let mut lines = Vec::new();
    lines.push(format!(
        "Host: {}",
        if !yes("host_running") {
            "stopped".to_owned()
        } else {
            format!(
                "running{}{}",
                data["host_version"]
                    .as_str()
                    .map(|v| format!(", version {v}"))
                    .unwrap_or_default(),
                if yes("headless") { ", headless" } else { "" }
            )
        }
    ));
    lines.push(format!(
        "Assistant: {}",
        if yes("running") { "running" } else { "stopped" }
    ));
    let dry_run = data["config"]["policy"]["dry_run"]
        .as_bool()
        .or(data["persisted_dry_run"].as_bool());
    if let Some(dry_run) = dry_run {
        lines.push(format!(
            "Mode: {}",
            if dry_run {
                "observe (never sends to Teams)"
            } else {
                "active (answers in Teams)"
            }
        ));
    }
    if yes("host_running") {
        lines.push(format!(
            "Microsoft account: {}",
            if yes("microsoft_connected") {
                "connected"
            } else {
                "not connected"
            }
        ));
        if yes("running") {
            let subscriptions = data["active_subscriptions"].as_u64().unwrap_or(0);
            lines.push(format!(
                "Teams: {subscriptions} active subscription(s){}; tunnel {}",
                data["subscription_issue"]
                    .as_str()
                    .map(|issue| format!(" (last problem: {issue})"))
                    .unwrap_or_default(),
                if yes("tunnel_running") {
                    "running"
                } else {
                    "stopped"
                }
            ));
        }
    }
    let llm = &data["config"]["llm"];
    let chain: Vec<String> = llm["chain"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["enabled"] != false)
        .map(|c| {
            format!(
                "{}:{}:{}",
                c["provider"].as_str().unwrap_or("?"),
                c["model"].as_str().unwrap_or("?"),
                c["effort"].as_str().unwrap_or("?")
            )
        })
        .collect();
    if !chain.is_empty() {
        lines.push(format!("Models: {}", chain.join(" > ")));
    } else if let Some(model) = llm["model"].as_str() {
        lines.push(format!("Models: deepseek:{model}:max"));
    }
    lines.push("Full detail: pta --json status".into());
    lines.join("\n")
}
pub async fn run() -> i32 {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h")
        || args.is_empty()
        || args
            .iter()
            .find(|a| !a.starts_with("--"))
            .map(String::as_str)
            == Some("help")
    {
        if args.iter().any(|a| a == "--json") {
            println!(
                "{}",
                serde_json::to_string(&Reply::success(json!({"help":HELP}))).unwrap()
            );
        } else {
            println!("{HELP}");
        }
        return 0;
    }
    if args.iter().any(|a| a == "--version") {
        if args.iter().any(|a| a == "--json") {
            println!(
                "{}",
                serde_json::to_string(&Reply::success(
                    json!({"cli_version":env!("CARGO_PKG_VERSION"),"contract":control::CONTRACT})
                ))
                .unwrap()
            );
        } else {
            println!(
                "pta {} (contract {})",
                env!("CARGO_PKG_VERSION"),
                control::CONTRACT
            );
        }
        return 0;
    }
    let json_output = args.iter().any(|a| a == "--json");
    let command: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .take(2)
        .cloned()
        .collect();
    let command = command.join(" ");
    let reply = match execute(args).await {
        Ok(reply) => reply,
        Err(error) => {
            // Never serialize dependency/provider error chains or user input (which may contain secrets).
            let input = error.to_string();
            if input.starts_with("incompatible") {
                Reply::error(
                    "state_conflict",
                    6,
                    "CLI/host incompatibles. Use a Wiki-capable host before applying the Wiki schema.",
                )
            } else if [
                "missing argument",
                "unknown",
                "invalid",
                "input must",
                "input is",
                "destination",
                "add sources",
                "source ID",
            ]
            .iter()
            .any(|s| input.starts_with(s))
            {
                Reply::error(
                    "invalid_input",
                    2,
                    "Invalid input. Consult pta --help and validate before applying.",
                )
            } else {
                Reply::error(
                    "dependency_or_state",
                    5,
                    "Operation could not complete. Check host installation, configuration and connectivity.",
                )
            }
        }
    };
    if json_output {
        println!("{}", serde_json::to_string(&reply).unwrap());
    } else if reply.ok {
        match human(&command, &reply.data) {
            Some(text) => println!("{text}"),
            None => println!("{}", serde_json::to_string_pretty(&reply.data).unwrap()),
        }
    } else {
        if !reply.data.is_null() {
            println!("{}", serde_json::to_string_pretty(&reply.data).unwrap());
        }
        eprintln!(
            "{}: {}",
            reply.code,
            reply
                .message
                .as_deref()
                .unwrap_or("Operation pending or failed.")
        );
    }
    reply.exit_code
}

#[cfg(test)]
mod wiki_tests {
    use super::*;
    #[test]
    fn wiki_commands_require_exact_local_source_argument_and_closed_stdin_schema() {
        for operation in ["list", "search", "read"] {
            assert!(
                validate_args(&["--json", "azure", "wiki", operation, "source"].map(str::to_owned))
                    .is_ok()
            );
            assert!(validate_args(&["azure", "wiki", operation].map(str::to_owned)).is_err());
            assert!(
                validate_args(&["azure", "wiki", operation, "source", "url"].map(str::to_owned))
                    .is_err()
            );
        }
        assert!(
            serde_json::from_value::<crate::ado::wiki::SearchInput>(
                json!({"query":"deploy","organization":"https://evil.test"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<crate::ado::wiki::SearchInput>(
                json!({"query":"deploy","author_mode":"infer_mine"})
            )
            .is_err()
        );
    }
    #[test]
    fn microsoft_redirect_flag_is_scoped_to_microsoft_finish() {
        let check =
            |args: &[&str]| validate_args(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>());
        for args in [
            &[
                "auth",
                "microsoft",
                "finish",
                "--redirect",
                "http://localhost:1234/?code=c",
            ][..],
            &[
                "auth",
                "microsoft",
                "finish",
                "--wait",
                "--redirect",
                "http://localhost:1/?code=c",
            ],
        ] {
            assert!(check(args).is_ok(), "{args:?}");
        }
        for args in [
            &["auth", "microsoft", "finish", "--redirect"][..],
            &[
                "auth",
                "microsoft",
                "finish",
                "--redirect",
                "https://evil.example/?code=c",
            ],
            &[
                "auth",
                "github",
                "finish",
                "--redirect",
                "http://localhost:1/?code=c",
            ],
            &["status", "--redirect", "http://localhost:1/?code=c"],
        ] {
            assert!(check(args).is_err(), "{args:?}");
        }
    }
}
#[cfg(test)]
mod help_tests {
    use super::*;
    #[test]
    fn help_documents_every_accepted_command() {
        let commands: &[&[&str]] = &[
            &["help"],
            &["capabilities"],
            &["status"],
            &["doctor"],
            &["start"],
            &["stop"],
            &["restart"],
            &["logs"],
            &["chat"],
            &["app", "open"],
            &["web", "status"],
            &["web", "configure"],
            &["web", "users", "list"],
            &["skill", "show"],
            &["config", "show"],
            &["config", "set", "policy.dry_run", "true"],
            &["mode", "observe"],
            &["self-chat", "status"],
            &["activity", "status"],
            &["activity", "run"],
            &["activity", "history"],
            &["activity", "pending"],
            &["activity", "open"],
            &["activity", "resolve", "activity-id"],
            &["credentials", "list"],
            &["auth", "microsoft", "status"],
            &["auth", "github", "login", "client"],
            &["github", "repos"],
            &["llm", "providers"],
            &["repos", "list"],
            &["sources", "list"],
            &["azure", "wiki", "list", "manuals"],
            &["tunnel", "status"],
            &["test", "providers"],
            &["audit", "list"],
        ];
        for command in commands {
            let args: Vec<String> = command.iter().map(|a| a.to_string()).collect();
            assert!(validate_args(&args).is_ok(), "{command:?}");
            // The command and its action are documented on the same help line.
            assert!(
                HELP.lines().any(|line| {
                    line.contains(&format!("{} ", command[0]))
                        && command.get(1).is_none_or(|action| line.contains(action))
                }),
                "help does not document {command:?}"
            );
        }
        for lifecycle in ["  start ", "  status ", "  stop ", "  restart "] {
            assert!(HELP.contains(lifecycle), "{lifecycle}");
        }
    }
    #[test]
    fn status_is_readable_without_json() {
        let running = json!({
            "host_running": true, "running": true, "headless": false, "host_version": "0.4.0",
            "microsoft_connected": true, "active_subscriptions": 1, "subscription_issue": null,
            "tunnel_running": true,
            "config": {"policy": {"dry_run": false}, "llm": {"model": "deepseek-flash", "chain": [
                {"provider":"codex","model":"gpt-6.1-sol","effort":"medium","enabled":true},
                {"provider":"deepseek","model":"deepseek-flash","effort":"max","enabled":false}
            ]}}
        });
        let text = status_text(&running);
        assert!(text.contains("Host: running, version 0.4.0"));
        assert!(text.contains("Assistant: running"));
        assert!(text.contains("Mode: active (answers in Teams)"));
        assert!(text.contains("Teams: 1 active subscription(s); tunnel running"));
        assert!(text.contains("Models: codex:gpt-6.1-sol:medium\n"));
        let stopped = json!({"host_running": false, "running": false, "persisted_dry_run": true,
            "config": {"llm": {"model": "deepseek-flash", "chain": []}}});
        let text = status_text(&stopped);
        assert!(text.contains("Host: stopped"));
        assert!(text.contains("Assistant: stopped"));
        assert!(text.contains("Mode: observe"));
        assert!(text.contains("Models: deepseek:deepseek-flash:max"));
        assert_eq!(
            human("stop", &json!({"running": false})).as_deref(),
            Some("Assistant stopped.")
        );
        assert_eq!(
            human("start", &json!({"running": true})).as_deref(),
            Some("Assistant running.")
        );
        assert!(human("config show", &json!({})).is_none());
    }
}
