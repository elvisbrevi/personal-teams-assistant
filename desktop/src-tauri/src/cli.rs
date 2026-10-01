use crate::control::{self, Reply, Request};
use anyhow::{Context, Result, ensure};
use personal_teams_assistant::{config::Config, knowledge::KnowledgeMap};
use serde_json::{Value, json};
use std::{
    io::{IsTerminal, Read},
    time::Duration,
};

const HELP: &str = "pta — Personal Teams Assistant administration
Usage: pta [--json] [--non-interactive] COMMAND ...
  --version | capabilities
  status | doctor [--offline]
  start | stop | restart
  app open|hide|quit
  config show|get FIELD|set FIELD JSON|apply|validate|import FILE
  mode show|observe|active
  self-chat status|enable [CHAT_ID]|disable|reconcile OUTPUT_NONCE MESSAGE_ID
  credentials list|set NAME|delete NAME
  auth microsoft|github status|login [CLIENT_ID]|finish|cancel|logout
    login: --no-browser; finish: --wait (maximum 600 seconds)
  github repos
  repos list|add ALIAS PATH|edit ALIAS PATH|remove ALIAS|clone OWNER/REPO ALIAS|sync ALIAS
  sources list|show ID|add|edit ID|remove ID|enable ID|disable ID
  sources audience ID (JSON object on stdin: allowed_conversations, allowed_senders, external_processing)
  azure wiki list|search|read SOURCE_ID (search/read: JSON on stdin; local reads only)
  tunnel status|configure token|configure external|configure file PATH|validate
  chat | test simulate (SimulationRequest JSON on stdin; never sends to Graph)
  test providers (paid API calls, synthetic facts only) | test connectivity (Graph account read)
  test self-chat (validates membership; audit confirms actual reception/sending)
  audit list|show RESOURCE [--content] [--limit N] | logs [--limit N]
  skill show|path|install DIRECTORY
Input: config apply/validate accept JSON or TOML {config,map,tunnel_config}; source add/edit accept Resource JSON on stdin.
Credential values: stdin only; never pass secret values as arguments.
Config fields: existing dot paths, JSON values; config apply exposes all Config/KnowledgeMap fields.
Exit: 0 success, 2 invalid input, 3 not ready, 4 authorization pending, 5 dependency/network, 6 conflict/version, 1 operation failed.
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
            | "--content" => {}
            "--limit" => {
                i += 1;
                ensure!(
                    args.get(i).is_some_and(|v| v.parse::<usize>().is_ok()),
                    "invalid limit"
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
        ["capabilities" | "status" | "doctor" | "start" | "stop" | "restart" | "logs" | "chat"]
            | ["app", "open" | "hide" | "quit"]
            | ["skill", "show" | "path"]
            | ["skill", "install", _]
            | ["config", "show" | "apply" | "validate"]
            | ["config", "get" | "import", _]
            | ["config", "set", _, _]
            | ["mode", "show" | "observe" | "active"]
            | ["self-chat", "status" | "enable" | "disable"]
            | ["self-chat", "enable", _]
            | ["self-chat", "reconcile", _, _]
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
        ("--content", p[0] == "audit"),
        ("--limit", ["audit", "logs"].contains(&p[0])),
    ] {
        ensure!(
            !args.iter().any(|a| a == flag) || allowed,
            "invalid flag for this command"
        );
    }
    Ok(())
}
async fn execute(mut args: Vec<String>) -> Result<Reply> {
    validate_args(&args)?;
    let non_interactive = args.iter().any(|a| a == "--non-interactive");
    args.retain(|a| a != "--json" && a != "--non-interactive");
    let command = positional(&args, 0)?;
    let action = args.get(1).map(String::as_str).unwrap_or("");
    match command {
        "capabilities" => {
            return Ok(Reply::success(
                json!({"contract":control::CONTRACT,"commands":HELP,"skill_version":env!("CARGO_PKG_VERSION")}),
            ));
        }
        "skill" => return crate::skill::command(action, args.get(2).map(String::as_str)),
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
    if command == "doctor" && args.iter().any(|a| a == "--offline") {
        let dir = control::profile_dir()?;
        let config: Config = toml::from_str(&std::fs::read_to_string(dir.join("config.toml"))?)?;
        let map = KnowledgeMap::load(&config.knowledge_map)?;
        crate::validate_local(&config, &map)?;
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
            "search" => serde_json::from_value::<personal_teams_assistant::ado::wiki::SearchInput>(
                value.clone(),
            )
            .context("invalid Wiki search input")?
            .validate()?,
            "read" => serde_json::from_value::<personal_teams_assistant::ado::wiki::ReadInput>(
                value.clone(),
            )
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
        let value = if provider == "github" && action == "login" {
            json!({"client_id":positional(&args,3)?})
        } else {
            json!({"open":!non_interactive && !args.iter().any(|a|a=="--no-browser")})
        };
        let mut reply = call(method, value, None, true).await?;
        if provider == "github" && action == "login" && reply.ok {
            reply.ok = false;
            reply.code = "authorization_pending".into();
            reply.exit_code = 4;
        }
        if action == "finish" && args.iter().any(|a| a == "--wait") {
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
pub async fn run() -> i32 {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") || args.is_empty() {
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
        println!("{}", serde_json::to_string_pretty(&reply.data).unwrap());
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
            serde_json::from_value::<personal_teams_assistant::ado::wiki::SearchInput>(
                json!({"query":"deploy","organization":"https://evil.test"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<personal_teams_assistant::ado::wiki::SearchInput>(
                json!({"query":"deploy","author_mode":"infer_mine"})
            )
            .is_err()
        );
    }
}
