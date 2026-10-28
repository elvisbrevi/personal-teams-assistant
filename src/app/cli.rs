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
  Requires Cloudflare Access and a dedicated GitHub OAuth App; see pta skill show.
  web status              Web URL, settings and account profiles (never passwords).
  web configure           JSON on stdin: bind, public_url, session_hours, owner_github_id and access.
                           access: issuer, audience, account_id, github_idp_id from Cloudflare.
  web callbacks           Exact Graph callback paths for Cloudflare Access bypass applications.
  web machines authorize MACHINE_ID [USER] --output PRIVATE_FILE
                           Locally approve your machine for 10 minutes; writes a private single-use
                           pairing JSON file for `pta tools call machine_connect` on that machine's stdin.
  web users list          Lists web accounts and their isolated profile directories.
  web users add USER      Creates an isolated profile, blocked until an Access identity is associated locally.
                           --current-profile attaches it to the existing desktop profile; identity binding is still required.
  web users authorize USER  Explicitly admits an additional isolated account; bind and enable separately.
  web users bind USER     Verified Access get-identity JSON on stdin; associates the GitHub provider subject.
  web users unbind|revoke USER  Removes the association or revokes all local sessions; preserves the profile.
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
  llm providers            Detected CLIs (Codex, Claude, OpenCode 2), their models and efforts, and DeepSeek through its API.
                           Order and fallback: pta config set llm.chain '[{\"provider\":\"codex\",\"model\":\"gpt-6.1-sol\",\"effort\":\"medium\"},...]'
  test providers           Tests each active model with synthetic data (uses quota or API credit).

Messages and diagnostics
  audit list [--limit N] [--content]   Received messages, their outcome and the step log.
  audit show RESOURCE [--content]      One message; --content includes question and answer.
  answer refine RESOURCE [--context-stdin] [--source ID]...  Writes the answer to a message that was not
                           answered (informational, a request for you) or rewrites an unsent one (withheld,
                           observed), with your context from stdin (up to 4000 characters) and only the
                           chosen sources (default: those of the conversation), in the background; not sent.
  answer send RESOURCE     Sends an unsent answer once, after your review (never retried).
  answer feedback RESOURCE JSON on stdin: rating good|bad (null clears), comment, example (a good answer
                           guides later answers to similar requests, by structure and tone) and context.
  logs [--limit N]         Host events.
  chat | test simulate     Real pipeline with a SimulationRequest JSON on stdin; never sends to Teams.
  test connectivity        Reads the connected Microsoft account (without sending).
                           If Microsoft requires renewed verification: auth microsoft login, then finish.

Accounts
  auth microsoft status|login|finish|cancel|logout
                           login [--no-browser]; finish [--wait] [--redirect URL]
                           (--redirect: the http://localhost:PORT/?code=... URL opened on another device)
  auth github status|login CLIENT_ID|finish|cancel|logout
  auth azure status|login|finish|cancel
                           login returns a Microsoft device code; finish [--wait] verifies the Azure DevOps session.
  auth machine status|login [URL]|finish|cancel
                           Adds this computer to your account on your own coordinator (HTTPS origin; omitted reuses
                           the remembered one). login [--no-browser] returns the approval URL and code; sign in there
                           and choose Trust this machine, then finish [--wait]. No credential is printed.
  github repos             Repositories authorized for the GitHub App.

Account configuration
  account status              Loaded and saved revisions of the account's portable configuration (preferences, model choices,
                           sources and audiences, contexts, questions, verified notes), unsaved local changes, what is
                           unavailable on this computer and the local setup still required. Reads local state only.
  account sync [--reset]      Restores and refreshes that configuration from the coordinator, which is authoritative. Paths,
                           repository folders, roots, tunnels, credentials and provider sessions are never synchronized and
                           nothing is started. Unsaved local changes are kept: --reset replaces them (a private backup is kept).
  account records status|history [DAY|FROM..TO]  Reads the coordinator's saved daily task record (read-only).

Account variables
  variables list [--limit N]  Names, descriptions and versions of this account's stored variables; never values.
  variables search TEXT       Variables whose name or description contains TEXT.
  variables sync [--reset]    Brings this installation's copy of the account's catalog up to date with the coordinator (names,
                           descriptions, versions; never values) and prints the revision. --reset discards the copy first,
                           the only way to accept a coordinator that was restored to an older state.
  variables status            Last synchronized revision and time, pending/error/conflict state; reads local state only.
  variables create NAME --description TEXT [--id UUID]
                           Stores a variable; its value is read from a protected pipe on stdin (never an argument
                           or a terminal). Names: letters, digits and _ (up to 128); descriptions are required (up to
                           500 characters); values are up to 16 KiB of UTF-8 text with newlines. It never overwrites:
                           after a timeout, inspect with list, or repeat with the printed --id.
  variables update ID --expected-version N [--description TEXT] [--value]
                           Edits a description and/or replaces the value (read from a protected pipe on stdin when
                           --value is given). ID and version come from list; a changed variable is a conflict.
  variables delete ID --expected-version N  Deletes a variable; its value cannot be recovered (the provider's credential is not revoked).
  variables entry start NAME --description TEXT  Asks for a value to be typed by the user (GUI or complete).
  variables entry replace ID  Asks for a replacement value to be typed by the user (GUI or complete).
  variables entry status|cancel|complete REQUEST  complete reads the value from stdin like create.
  variables consumers         Built-in consumers, the variable each uses (name and version only) and approved launchers.
  variables bind CONSUMER ID [--expected-version N] --confirmed
                           Makes a built-in consumer (deepseek) use the variable; its value is checked, never shown.
  variables unbind CONSUMER   Stops that use; the variable is kept.
  variables check CONSUMER [--expected-version N] --confirmed
                           Uses the bound variable once against the consumer's fixed destination; prints status only.
  variables migrate inspect   Audits the configured application credentials (names, origins and eligibility only, never values),
                           the destination account to confirm and the account settings state.
  variables migrate run CREDENTIAL --destination KEY --description TEXT [--name NAME] [--bind]
                           [--existing use|replace --expected-version N] [--settings] --confirmed
                           Copies one credential read by the host from this installation's own store into the confirmed
                           account (the value never passes through the CLI), reads it back and, with --bind, switches
                           its consumer. A name already in the account needs --existing at the reviewed version.
                           --settings also replaces this computer's portable settings with the account's (backed up).
                           Repeat the same command to resume an interrupted migration; originals are never changed.
  variables migrate status    The durable outcome of the migration for the signed-in account.
  variables launcher list     Approved local launchers (owner's trusted machine only).
  variables launcher define NAME --description TEXT --program PATH --directory DIR --env ENV=ID [--env ...]
                           [--arg ARG ...] [--timeout SECONDS] --confirmed
                           Approves a program to receive the named variables as environment variables of that one
                           process (never arguments). Protected settings such as PATH or LD_* are refused.
  variables launcher run NAME [--pin ENV=VERSION ...] --confirmed
                           Runs it with the values resolved now; prints status and references, never output or values.
  variables launcher remove NAME  Removes an approved launcher.
  variables audit [--limit N]  Operator-local: recent variable edits, deletions and reveals (never values).
  variables verify            Checks that every stored variable authenticates with the current keys.
  variables key backup --output PRIVATE_FILE  Operator-local: writes the master keys (not values) to a new 0600 file.
  variables key restore       Operator-local: master-key backup JSON on stdin; never replaces an existing key.
  variables key rotate        Operator-local: new master and data keys; keeps older masters for older backups.

Updates (this computer only; the shared server's operator updates it separately)
  update status            Installed, running and available versions, how this installation was built and the state of any
                           prepared update; reads local state only. Staging is not installation: the installed version
                           keeps running unchanged.
  update check             Looks up the newest stable, not yanked release of this package on crates.io (read-only).
  update stage VERSION --confirmed
                           Builds that exact release with Cargo (locked dependencies, same features and target as this
                           installation) in this profile's maintenance directory and verifies the application and pta
                           pair. It installs nothing and needs the owner of this computer. Progress: update status.
  update apply VERSION --confirmed
                           Installs the prepared version: waits for work in progress (it never interrupts an answer, a send,
                           an implementation or a migration), then replaces the application and pta together, restarts the
                           application the way it was running and restores the assistant. If the new version does not start
                           and report healthy, the previous pair is put back. Success is reported only after the health
                           check; poll update status (it reads the update journal while the application restarts).
  update cancel            Cancels an apply request that is waiting for work to finish.
  update dismiss VERSION   Hides the notice for that release.

Personal chat
  self-chat status|enable [CHAT_ID]|disable
  self-chat reconcile NONCE MESSAGE_ID  Resolves a pending output.
  test self-chat           Checks the personal chat membership (does not send).

Daily task record
  activity status          Current snapshot run and recorded days.
  activity run [DAY]        Saves existing Azure tasks locally (today by default); never creates tasks.
  activity history [DAY|FROM..TO]  Recorded days and the selected day's tasks (YYYY-MM-DD).
  activity pending         Retired; always empty.
  activity resolve ID      Retired; refuses task creation.
  activity open [ID]        Opens Activities in the app.
                           Configure with config set activity_registration.FIELD JSON.

Knowledge
  repos list | repos add|edit ALIAS PATH | repos remove ALIAS
  repos clone OWNER/REPO ALIAS | repos sync ALIAS
  sources list | sources show ID | sources add | sources edit ID | sources remove ID
  sources enable|disable ID
  sources audience ID      JSON on stdin: allowed_conversations, allowed_senders, external_processing.
                           A whole repository is a source too: {\"kind\":\"repository\",\"repository\":\"ALIAS\",...};
                           the verified notes: {\"kind\":\"notes\",...} (optional \"contexts\" keeps only theirs).
  contexts list | contexts show ID | contexts add | contexts edit ID | contexts remove ID
                           Subjects such as «cobros»: JSON on stdin with id, name, description, keywords,
                           sources (IDs) and instructions. A request naming one reads its sources.
  questions list | questions show ID     Configured questions, their selected sources and associated requests.
  questions add | questions edit ID      JSON on stdin: id, text, sources and optional examples.
  questions remove ID                   Remove a question.
  questions associate RESOURCE          JSON on stdin: question_id (existing) or text (new), and sources.
  cases list | cases show ID | cases edit ID | cases reset ID  Retired; use questions.
  people list | people show ID | people add | people edit ID | people remove ID
                           How you address each person: JSON with id, name, senders (Teams user IDs),
                           aliases, address and notes; used only when answering that person.
  notes list [--context ID] [--limit N]  Verified notes, newest first.
  notes search TEXT [--context ID]       Notes by relevance.
  notes add | notes edit ID              JSON on stdin: title, text, contexts, keywords, origin, reference.
  notes remove ID
  knowledge search TEXT [--source ID]... [--context ID]
                           The sections the assistant would read from repositories and notes (no model).
  index status | index refresh [SOURCE]  What each indexed source holds; compare repositories with their files.
  azure wiki list|search|read SOURCE_ID  (search/read: JSON on stdin; local read only)

Agents
  tools list               Shared application tools, their filters and effects (no host required).
  tools call NAME          Calls a shared tool with JSON on stdin; external actions require confirmed:true.
                           list_azure_hus: source, project, state, kind (hu|bug), limit (default 10, maximum 100).
                           list_github_epics: project (OWNER/REPO), state (open|closed), limit.
                           get_azure_hu: id, optional source and project (HU or bug detail).
                           list_azure_tasks: day (YYYY-MM-DD), optional user, source and project.
                           get_azure_task: id, optional source and project (read-only task detail).
                           get_github_epic: id and project (OWNER/REPO).
  capabilities             Contract and commands as JSON.
  mcp                      Model Context Protocol server over stdio for agents (Codex, Claude Code): every
                           operation of the app as a tool; sending, access and mode changes
                           need the user's confirmation. mcp config prints how to add it to each agent.
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
/// A value comes only from a protected pipe: never an argument, never a terminal. One final line
/// break (added by `echo`) is not part of the value.
fn protected_value() -> Result<String> {
    ensure!(
        !std::io::stdin().is_terminal(),
        "the value must come from a protected pipe on stdin, never an argument or a terminal"
    );
    let mut value = stdin_text()?;
    ensure!(value.len() <= 16_386, "value too long (at most 16 KiB)");
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    Ok(value)
}
/// The words of a command without its flags and their values.
fn words(args: &[String]) -> Vec<&str> {
    let mut words = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
        } else if matches!(
            arg.as_str(),
            "--description"
                | "--id"
                | "--expected-version"
                | "--output"
                | "--limit"
                | "--context"
                | "--source"
                | "--redirect"
                | "--program"
                | "--directory"
                | "--env"
                | "--arg"
                | "--timeout"
                | "--pin"
                | "--destination"
                | "--name"
                | "--existing"
        ) {
            skip = true;
        } else if !arg.starts_with("--") {
            words.push(arg.as_str());
        }
    }
    words
}
fn option<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}
/// Every value of a repeatable flag, in order.
fn options<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
    args.iter()
        .enumerate()
        .filter(|(_, a)| *a == flag)
        .filter_map(|(i, _)| args.get(i + 1))
        .map(String::as_str)
        .collect()
}
/// Access-granting steps need the user's confirmation, like `tools call ... confirmed:true`.
fn unconfirmed(args: &[String], question: String) -> Option<Reply> {
    (!args.iter().any(|a| a == "--confirmed")).then(|| {
        Reply::error(
            "invalid_input",
            2,
            &format!(
                "Confirm this exact action with the user, then repeat with --confirmed: {question}"
            ),
        )
    })
}
fn pairs<T>(
    values: Vec<&str>,
    parse: impl Fn(&str) -> Option<T>,
    what: &str,
) -> Result<std::collections::BTreeMap<String, T>> {
    let mut map = std::collections::BTreeMap::new();
    for value in values {
        let (name, rest) = value
            .split_once('=')
            .with_context(|| format!("{what} must look like NAME=VALUE"))?;
        let parsed = parse(rest).with_context(|| format!("invalid {what}"))?;
        ensure!(
            map.insert(name.to_owned(), parsed).is_none(),
            "{what} repeats {name}"
        );
    }
    Ok(map)
}
/// The account's portable configuration: synchronize it, see what is loaded, read retained records.
async fn update_command(args: &[String]) -> Result<Reply> {
    let words = words(args);
    let version = || -> Result<&str> {
        words
            .get(2)
            .copied()
            .context("missing version; consult --help")
    };
    match words.get(1).copied() {
        // While an update is applied no host may be running: read its journal instead of starting one.
        Some("status") => match applying_status() {
            Some(reply) => Ok(reply),
            None => call("crate_update_status", json!({}), None, true).await,
        },
        Some("check") => call("crate_update_check", json!({}), None, true).await,
        Some("cancel") => call("crate_update_cancel", json!({}), None, true).await,
        Some("dismiss") => {
            call(
                "crate_update_dismiss",
                json!({"version":version()?}),
                None,
                true,
            )
            .await
        }
        Some("stage") => {
            let version = version()?;
            if let Some(reply) = unconfirmed(
                args,
                format!(
                    "Build version {version} of this package with Cargo (it is not installed)?"
                ),
            ) {
                return Ok(reply);
            }
            call("crate_update_stage", json!({"version":version}), None, true).await
        }
        Some("apply") => {
            let version = version()?;
            if let Some(reply) = unconfirmed(
                args,
                format!(
                    "Install the prepared version {version} now? The application and pta are replaced and restarted; the previous version is restored if the new one does not start."
                ),
            ) {
                return Ok(reply);
            }
            call("crate_update_apply", json!({"version":version}), None, true).await
        }
        _ => anyhow::bail!("unknown command or invalid argument count"),
    }
}

/// The state of an update that is being applied, from its journal. The host is stopped for part of
/// it, so this never contacts or starts one.
fn applying_status() -> Option<Reply> {
    use crate::maintenance::apply as journal;
    let dir = control::profile_dir()
        .ok()?
        .join(crate::maintenance::DIRECTORY);
    let entry = journal::read(&dir).filter(|j| !j.phase.finished())?;
    // A journal whose driver died is undone first, then the host answers as usual.
    if !matches!(
        journal::recover(&dir, journal::now()),
        Ok(journal::Recovery::Active)
    ) {
        return None;
    }
    Some(Reply::success(json!({
        "managed": "local", "state": "applying", "running": env!("CARGO_PKG_VERSION"),
        "installed": entry.from, "available": entry.to, "can_stage": false, "can_apply": false,
        "apply": journal::describe(&entry),
    })))
}

async fn account_command(args: &[String]) -> Result<Reply> {
    let words = words(args);
    match (words.get(1).copied(), words.get(2).copied()) {
        (Some("status"), _) => call("portable_status", json!({}), None, true).await,
        (Some("sync"), _) => {
            let reset = args.iter().any(|a| a == "--reset");
            call(
                "portable_sync",
                if reset {
                    json!({"reset":true})
                } else {
                    json!({})
                },
                None,
                true,
            )
            .await
        }
        (Some("records"), Some(kind)) => {
            let (kind, values) = match kind {
                "status" => ("activity_status", json!({})),
                _ => match words.get(3).and_then(|a| a.split_once("..")) {
                    Some((from, to)) => ("activity_history", json!({"day":to,"from":from})),
                    None => ("activity_history", json!({"day":words.get(3)})),
                },
            };
            let mut body = json!({"kind":kind});
            body["day"] = values["day"].clone();
            body["from"] = values["from"].clone();
            call("portable_records", body, None, true).await
        }
        _ => anyhow::bail!("missing argument; consult --help"),
    }
}

async fn variables_command(args: &[String]) -> Result<Reply> {
    let words = words(args);
    let word = |i: usize| -> Result<&str> {
        words
            .get(i)
            .copied()
            .context("missing argument; consult --help")
    };
    let description =
        || -> Result<&str> { option(args, "--description").context("--description is required") };
    match (word(1)?, words.get(2).copied()) {
        ("list", _) => {
            let limit = option(args, "--limit")
                .map(|v| v.parse::<usize>().context("invalid limit"))
                .transpose()?;
            call("variable_list", json!({"limit":limit}), None, true).await
        }
        ("search", _) => call("variable_list", json!({"query":word(2)?}), None, true).await,
        ("sync", _) => {
            let reset = args.iter().any(|a| a == "--reset");
            call(
                "variable_sync",
                if reset {
                    json!({"reset":true})
                } else {
                    json!({})
                },
                None,
                true,
            )
            .await
        }
        ("status", _) => call("variable_sync_status", json!({}), None, true).await,
        ("create", _) => {
            let id = option(args, "--id")
                .map(str::to_owned)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let value = protected_value()?;
            let body = json!({"id":id,"name":word(2)?,"description":description()?,"value":value});
            call("variable_create", body, None, true).await.map_err(|error| {
                // The host may have stored it before the timeout: inspect, then repeat only with this ID.
                error.context(format!("The result is unknown. Inspect with `pta variables list`; if it is absent, repeat with --id {id}."))
            })
        }
        ("update", _) => {
            let version = option(args, "--expected-version")
                .context("--expected-version is required")?
                .parse::<u64>()
                .context("invalid version")?;
            let description = option(args, "--description");
            let replace_value = args.iter().any(|a| a == "--value");
            ensure!(
                description.is_some() || replace_value,
                "give --description, --value or both"
            );
            let mut body = json!({"id":word(2)?,"version":version});
            if let Some(description) = description {
                body["description"] = json!(description);
            }
            if replace_value {
                body["value"] = json!(protected_value()?);
            }
            call("variable_update", body, None, true).await.map_err(|error| {
                error.context("The result is unknown. Inspect with `pta variables list` before repeating.")
            })
        }
        ("delete", _) => {
            let version = option(args, "--expected-version")
                .context("--expected-version is required")?
                .parse::<u64>()
                .context("invalid version")?;
            call(
                "variable_delete",
                json!({"id":word(2)?,"version":version}),
                None,
                true,
            )
            .await
        }
        ("audit", _) => {
            let limit = option(args, "--limit")
                .map(|v| v.parse::<usize>().context("invalid limit"))
                .transpose()?;
            call("variable_audit", json!({"limit":limit}), None, true).await
        }
        ("entry", Some("replace")) => {
            call(
                "variable_rotation_start",
                json!({"id":word(3)?}),
                None,
                true,
            )
            .await
        }
        ("entry", Some("start")) => {
            call(
                "variable_entry_start",
                json!({"name":word(3)?,"description":description()?}),
                None,
                true,
            )
            .await
        }
        ("entry", Some("status")) => {
            call(
                "variable_entry_status",
                json!({"request":word(3)?}),
                None,
                true,
            )
            .await
        }
        ("entry", Some("cancel")) => {
            call(
                "variable_entry_cancel",
                json!({"request":word(3)?}),
                None,
                true,
            )
            .await
        }
        ("entry", Some("complete")) => {
            let value = protected_value()?;
            call(
                "variable_entry_complete",
                json!({"request":word(3)?,"value":value}),
                None,
                true,
            )
            .await
        }
        ("consumers", _) => call("variable_consumer_list", json!({}), None, true).await,
        ("migrate", Some("inspect")) => {
            call("variable_migration_inspect", json!({}), None, true).await
        }
        ("migrate", Some("status")) => {
            call("variable_migration_status", json!({}), None, true).await
        }
        ("migrate", Some("run")) => {
            let credential = word(3)?;
            let destination = option(args, "--destination").context("--destination is required")?;
            if let Some(reply) = unconfirmed(
                args,
                format!(
                    "Copy the application credential {credential} into account {destination} (the original stays untouched)?"
                ),
            ) {
                return Ok(reply);
            }
            let mut item = json!({"credential":credential,"description":description()?,"bind":args.iter().any(|a| a == "--bind")});
            if let Some(name) = option(args, "--name") {
                item["name"] = json!(name);
            }
            if let Some(action) = option(args, "--existing") {
                let version = option(args, "--expected-version")
                    .context("--expected-version is required with --existing")?
                    .parse::<u64>()
                    .context("invalid version")?;
                item["existing"] = json!({"action":action,"version":version});
            }
            let mut body = json!({"destination":destination,"items":[item]});
            if args.iter().any(|a| a == "--settings") {
                body["settings"] = json!("adopt_account");
            }
            call("variable_migration_run", body, None, true).await.map_err(|error| {
                error.context("The result is unknown. Inspect with `pta variables migrate status`; repeating the same command resumes safely.")
            })
        }
        ("bind", _) | ("check", _) => {
            let consumer = word(2)?;
            let version = option(args, "--expected-version")
                .map(|v| v.parse::<u64>().context("invalid version"))
                .transpose()?;
            if word(1)? == "bind" {
                let variable = word(3)?;
                if let Some(reply) = unconfirmed(
                    args,
                    format!("Use variable {variable} as the {consumer} credential?"),
                ) {
                    return Ok(reply);
                }
                let mut body = json!({"consumer":consumer,"variable":variable});
                if let Some(version) = version {
                    body["version"] = json!(version);
                }
                call("variable_consumer_bind", body, None, true).await
            } else {
                if let Some(reply) = unconfirmed(
                    args,
                    format!(
                        "Send the variable bound to {consumer} to its fixed destination to test it?"
                    ),
                ) {
                    return Ok(reply);
                }
                call(
                    "variable_consumer_check",
                    json!({"consumer":consumer,"version":version}),
                    None,
                    true,
                )
                .await
            }
        }
        ("unbind", _) => {
            call(
                "variable_consumer_unbind",
                json!({"consumer":word(2)?}),
                None,
                true,
            )
            .await
        }
        ("launcher", Some("list")) => call("variable_consumer_list", json!({}), None, true).await,
        ("launcher", Some("remove")) => {
            call(
                "variable_launcher_remove",
                json!({"id":word(3)?}),
                None,
                true,
            )
            .await
        }
        ("launcher", Some("define")) => {
            let env = pairs(options(args, "--env"), |v| Some(v.to_owned()), "--env")?;
            let program = option(args, "--program").context("--program is required")?;
            let directory = option(args, "--directory").context("--directory is required")?;
            if let Some(reply) = unconfirmed(
                args,
                format!(
                    "Allow program {program} in {directory} to receive the variables {} as environment variables?",
                    serde_json::to_string(&env)?
                ),
            ) {
                return Ok(reply);
            }
            let mut body = json!({"id":word(3)?,"description":description()?,"program":program,
                "args":options(args, "--arg"),"directory":directory,"env":env});
            if let Some(timeout) = option(args, "--timeout") {
                body["timeout_seconds"] = json!(timeout.parse::<u64>().context("invalid timeout")?);
            }
            call("variable_launcher_define", body, None, true).await
        }
        ("launcher", Some("run")) => {
            let name = word(3)?;
            let versions = pairs(options(args, "--pin"), |v| v.parse::<u64>().ok(), "--pin")?;
            if let Some(reply) = unconfirmed(
                args,
                format!("Run launcher {name} now, giving its program the variables it names?"),
            ) {
                return Ok(reply);
            }
            call(
                "variable_launcher_run",
                json!({"id":name,"versions":versions}),
                None,
                true,
            )
            .await
            .map_err(|error| {
                error.context("The result is unknown. The program may have run; inspect its effects before repeating.")
            })
        }
        ("verify", _) => call("variable_verify", json!({}), None, true).await,
        ("key", Some("backup")) => {
            let output = option(args, "--output").context("--output is required")?;
            call("variable_key_backup", json!({"output":output}), None, true).await
        }
        ("key", Some("restore")) => {
            let backup: Value = serde_json::from_str(&stdin_text()?)
                .context("input must be a variable key backup JSON")?;
            call("variable_key_restore", json!({"backup":backup}), None, true).await
        }
        ("key", Some("rotate")) => call("variable_key_rotate", json!({}), None, true).await,
        _ => anyhow::bail!("unknown command or invalid argument count"),
    }
}
fn positional(args: &[String], index: usize) -> Result<&str> {
    args.get(index)
        .map(String::as_str)
        .context("missing argument; consult --help")
}
pub(super) async fn call(
    method: &str,
    args: Value,
    revision: Option<String>,
    launch: bool,
) -> Result<Reply> {
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
pub(super) async fn snapshot() -> Result<Reply> {
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
            | "--content" | "--current-profile" | "--context-stdin" | "--value" | "--confirmed"
            | "--reset" | "--bind" | "--settings" => {}
            "--destination" | "--name" | "--existing" => {
                i += 1;
                ensure!(
                    args.get(i)
                        .is_some_and(|v| !v.is_empty() && !v.starts_with("--")),
                    "invalid value for this flag"
                );
            }
            "--program" | "--directory" => {
                i += 1;
                ensure!(
                    args.get(i)
                        .is_some_and(|v| std::path::Path::new(v).is_absolute()),
                    "invalid absolute path"
                );
            }
            "--env" | "--pin" => {
                i += 1;
                ensure!(
                    args.get(i).is_some_and(|v| v
                        .split_once('=')
                        .is_some_and(|(name, rest)| !name.is_empty() && !rest.is_empty())),
                    "invalid NAME=VALUE pair"
                );
            }
            "--arg" => {
                // A fixed argument may itself look like a flag (`--check`); it is never a value.
                i += 1;
                ensure!(args.get(i).is_some(), "invalid argument");
            }
            "--timeout" => {
                i += 1;
                ensure!(
                    args.get(i).is_some_and(|v| v.parse::<u64>().is_ok()),
                    "invalid timeout"
                );
            }
            "--expected-version" => {
                i += 1;
                ensure!(
                    args.get(i)
                        .is_some_and(|v| v.parse::<u64>().is_ok_and(|n| n > 0)),
                    "invalid version"
                );
            }
            "--description" => {
                i += 1;
                ensure!(
                    args.get(i).is_some_and(|v| !v.is_empty()),
                    "invalid description"
                );
            }
            "--id" => {
                i += 1;
                ensure!(
                    args.get(i)
                        .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok()),
                    "invalid entry ID"
                );
            }
            "--context" => {
                i += 1;
                ensure!(
                    args.get(i)
                        .is_some_and(|v| !v.is_empty() && !v.starts_with("--")),
                    "invalid context ID"
                );
            }
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
            "--source" => {
                i += 1;
                ensure!(
                    args.get(i)
                        .is_some_and(|v| !v.is_empty() && !v.starts_with("--")),
                    "invalid source ID"
                );
            }
            "--output" => {
                i += 1;
                ensure!(
                    args.get(i)
                        .is_some_and(|v| std::path::Path::new(v).is_absolute()),
                    "use an absolute private output path"
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
            | ["mcp"]
            | ["mcp", "config"]
            | ["tools", "list"]
            | ["tools", "call", _]
            | ["update", "status" | "check" | "cancel"]
            | ["update", "stage" | "apply" | "dismiss", _]
            | ["account", "status" | "sync"]
            | ["account", "records", "status" | "history"]
            | ["account", "records", "history", _]
            | ["variables", "list" | "verify" | "audit" | "sync" | "status"]
            | ["variables", "search" | "create" | "update" | "delete", _]
            | ["variables", "consumers"]
            | ["variables", "migrate", "inspect" | "status"]
            | ["variables", "migrate", "run", _]
            | ["variables", "unbind" | "check", _]
            | ["variables", "bind", _, _]
            | ["variables", "launcher", "list"]
            | ["variables", "launcher", "define" | "remove" | "run", _]
            | [
                "variables",
                "entry",
                "start" | "status" | "cancel" | "complete" | "replace",
                _
            ]
            | ["variables", "key", "backup" | "restore" | "rotate"]
            | ["web", "status" | "configure" | "callbacks"]
            | ["web", "users", "list"]
            | ["web", "machines", "authorize", _]
            | ["web", "machines", "authorize", _, _]
            | [
                "web",
                "users",
                "add" | "authorize" | "bind" | "unbind" | "revoke" | "disable" | "enable",
                _
            ]
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
            | ["auth", "azure", "status" | "login" | "finish" | "cancel"]
            | ["auth", "machine", "status" | "login" | "finish" | "cancel"]
            | ["auth", "machine", "login", _]
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
            | ["answer", "refine" | "send" | "feedback", _]
            | ["people", "list" | "add"]
            | ["people", "show" | "edit" | "remove", _]
            | ["questions", "list" | "add"]
            | ["questions", "show" | "edit" | "remove" | "associate", _]
            | ["cases", "list"]
            | ["cases", "show" | "edit" | "reset", _]
            | ["contexts", "list" | "add"]
            | ["contexts", "show" | "edit" | "remove", _]
            | ["notes", "list" | "add"]
            | ["notes", "search" | "edit" | "remove", _]
            | ["knowledge", "search", _]
            | ["index", "status" | "refresh"]
            | ["index", "refresh", _]
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
            "--context-stdin",
            p.as_slice().starts_with(&["activity", "run"])
                || p.as_slice().starts_with(&["answer", "refine"]),
        ),
        (
            "--current-profile",
            p.as_slice().starts_with(&["web", "users", "add"]),
        ),
        (
            "--output",
            p.as_slice().starts_with(&["web", "machines", "authorize"])
                || p.as_slice() == ["variables", "key", "backup"],
        ),
        (
            "--description",
            p.as_slice().starts_with(&["variables", "create"])
                || p.as_slice().starts_with(&["variables", "update"])
                || p.as_slice().starts_with(&["variables", "entry", "start"])
                || p.as_slice().starts_with(&["variables", "migrate", "run"])
                || p.as_slice()
                    .starts_with(&["variables", "launcher", "define"]),
        ),
        (
            "--destination",
            p.as_slice().starts_with(&["variables", "migrate", "run"]),
        ),
        (
            "--name",
            p.as_slice().starts_with(&["variables", "migrate", "run"]),
        ),
        (
            "--existing",
            p.as_slice().starts_with(&["variables", "migrate", "run"]),
        ),
        (
            "--bind",
            p.as_slice().starts_with(&["variables", "migrate", "run"]),
        ),
        (
            "--settings",
            p.as_slice().starts_with(&["variables", "migrate", "run"]),
        ),
        (
            "--expected-version",
            p.as_slice().starts_with(&["variables", "update"])
                || p.as_slice().starts_with(&["variables", "delete"])
                || p.as_slice().starts_with(&["variables", "bind"])
                || p.as_slice().starts_with(&["variables", "migrate", "run"])
                || p.as_slice().starts_with(&["variables", "check"]),
        ),
        (
            "--confirmed",
            p.as_slice().starts_with(&["update", "stage"])
                || p.as_slice().starts_with(&["update", "apply"])
                || p.as_slice().starts_with(&["variables", "migrate", "run"])
                || p.as_slice().starts_with(&["variables", "bind"])
                || p.as_slice().starts_with(&["variables", "check"])
                || p.as_slice()
                    .starts_with(&["variables", "launcher", "define"])
                || p.as_slice().starts_with(&["variables", "launcher", "run"]),
        ),
        (
            "--program",
            p.as_slice()
                .starts_with(&["variables", "launcher", "define"]),
        ),
        (
            "--directory",
            p.as_slice()
                .starts_with(&["variables", "launcher", "define"]),
        ),
        (
            "--env",
            p.as_slice()
                .starts_with(&["variables", "launcher", "define"]),
        ),
        (
            "--arg",
            p.as_slice()
                .starts_with(&["variables", "launcher", "define"]),
        ),
        (
            "--timeout",
            p.as_slice()
                .starts_with(&["variables", "launcher", "define"]),
        ),
        (
            "--pin",
            p.as_slice().starts_with(&["variables", "launcher", "run"]),
        ),
        (
            "--value",
            p.as_slice().starts_with(&["variables", "update"]),
        ),
        ("--id", p.as_slice().starts_with(&["variables", "create"])),
        (
            "--reset",
            p.as_slice() == ["variables", "sync"] || p.as_slice() == ["account", "sync"],
        ),
        (
            "--limit",
            ["audit", "logs", "notes", "variables"].contains(&p[0]),
        ),
        (
            "--source",
            p.as_slice().starts_with(&["answer", "refine"])
                || p.as_slice().starts_with(&["knowledge", "search"]),
        ),
        ("--context", ["notes", "knowledge"].contains(&p[0])),
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
    if !matches!(command, "capabilities" | "skill" | "tools") && !offline {
        retire_outdated_host(command, action, !non_interactive && !json_output).await?;
    }
    if command == "variables" {
        return variables_command(&args).await;
    }
    if command == "account" {
        return account_command(&args).await;
    }
    if command == "update" {
        return update_command(&args).await;
    }
    match command {
        "tools" => {
            if action == "list" {
                return Ok(Reply::success(super::tools::catalog()));
            }
            let name = positional(&args, 2)?;
            let value = input()?;
            if super::tools::validate(name, &value).is_err() {
                return Ok(Reply::error(
                    "invalid_input",
                    2,
                    "Invalid tool arguments. Consult pta tools list for the input schema.",
                ));
            }
            if let Some(question) = super::tools::confirmation(name, &value)
                && value["confirmed"] != true
            {
                return Ok(Reply::error(
                    "invalid_input",
                    2,
                    &format!(
                        "Confirm this exact action before calling with confirmed:true: {question}"
                    ),
                ));
            }
            retire_outdated_host(command, action, !non_interactive && !json_output).await?;
            return match super::tools::run(name, &value).await {
                Ok(data) => Ok(Reply::success(data)),
                Err(message) => Ok(Reply::error("operation_failed", 1, &message)),
            };
        }
        "capabilities" => {
            return Ok(Reply::success(
                json!({"contract":control::CONTRACT,"commands":HELP,"skill_version":env!("CARGO_PKG_VERSION"),"activity_registration_support":true,"web_support":true,"web_access_support":true,"knowledge_index_support":true,"mcp_support":true,"tools_support":true,"todo_support":true,"implementation_support":true,"machine_pairing_support":true,"machine_login_support":true,"machine_management_support":true,"account_variables_support":true,"account_portable_support":true,"account_authority_support":true,"shared_accounts_support":true}),
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
            "run" => json!({"day":args.iter().skip(2).find(|value| !value.starts_with("--")),
                "context":if args.iter().any(|a| a == "--context-stdin") { stdin_text()? } else { String::new() }}),
            "history" => match args.get(2).and_then(|a| a.split_once("..")) {
                Some((from, to)) => json!({"day":to,"from":from}),
                None => json!({"day":args.get(2)}),
            },
            "open" => json!({"id":args.get(2)}),
            "resolve" => json!({"id":positional(&args,2)?,"resolution":input()?}),
            _ => json!({}),
        };
        return call(method, values, None, true).await;
    }
    if command == "cases" {
        anyhow::bail!(
            "Cases were replaced by Questions. Use pta questions list, add, edit, remove or associate."
        );
    }
    if command == "questions" && action == "associate" {
        let mut value = input()?;
        ensure!(value.is_object(), "association is a JSON object");
        value["resource"] = json!(positional(&args, 2)?);
        return call("question_associate", value, None, true).await;
    }
    if command == "answer" {
        let resource = positional(&args, 2)?;
        return match action {
            "refine" => {
                let context = if args.iter().any(|a| a == "--context-stdin") {
                    stdin_text()?
                } else {
                    String::new()
                };
                let sources: Vec<&String> = args
                    .windows(2)
                    .filter(|pair| pair[0] == "--source")
                    .map(|pair| &pair[1])
                    .collect();
                call(
                    "answer_refine",
                    json!({"resource":resource,"context":context,"sources":sources}),
                    None,
                    true,
                )
                .await
            }
            "feedback" => {
                let mut value = input()?;
                ensure!(value.is_object(), "feedback is a JSON object");
                value["resource"] = json!(resource);
                call("answer_feedback", value, None, true).await
            }
            _ => call("answer_send", json!({"resource":resource}), None, true).await,
        };
    }
    if ["notes", "knowledge", "index"].contains(&command) {
        let flag = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let (method, value) = match (command, action) {
            ("notes", "list") => (
                "notes_list",
                json!({"context":flag("--context"),"limit":flag("--limit").map(|l| l.parse::<u64>()).transpose()?}),
            ),
            ("notes", "search") => (
                "notes_list",
                json!({"query":positional(&args,2)?,"context":flag("--context")}),
            ),
            ("notes", "add") => ("notes_save", json!({"note":input()?})),
            ("notes", "edit") => {
                let mut note = input()?;
                note["id"] = json!(positional(&args, 2)?);
                ("notes_save", json!({"note":note}))
            }
            ("notes", "remove") => ("notes_remove", json!({"id":positional(&args,2)?})),
            ("knowledge", "search") => {
                let sources: Vec<&String> = args
                    .windows(2)
                    .filter(|pair| pair[0] == "--source")
                    .map(|pair| &pair[1])
                    .collect();
                (
                    "knowledge_search",
                    json!({"query":positional(&args,2)?,"context":flag("--context"),
                        "sources":(!sources.is_empty()).then_some(sources)}),
                )
            }
            ("index", "status") => ("index_status", json!({})),
            ("index", "refresh") => ("index_refresh", json!({"source":args.get(2)})),
            _ => anyhow::bail!("unknown command; consult --help"),
        };
        return call(method, value, None, true).await;
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
            ["github", "microsoft", "azure", "machine"].contains(&provider),
            "unknown provider"
        );
        let action = positional(&args, 2)?;
        if provider == "machine" {
            let method = match action {
                "status" => "machine_login_status",
                "login" => "machine_login",
                "finish" => "machine_login_finish",
                "cancel" => "machine_login_cancel",
                _ => anyhow::bail!("unknown auth action"),
            };
            let value = if action == "login" {
                let mut value =
                    json!({"open":!non_interactive && !args.iter().any(|a|a=="--no-browser")});
                if let Some(url) = args.iter().skip(3).find(|a| !a.starts_with("--")) {
                    value["url"] = json!(url);
                }
                value
            } else {
                json!({})
            };
            let mut reply = call(method, value, None, true).await?;
            if action == "login" && reply.ok {
                reply.ok = false;
                reply.code = "authorization_pending".into();
                reply.exit_code = 4;
                reply.message = Some("Sign in at the approval URL, choose Trust this machine, then run pta auth machine finish --wait.".into());
            }
            if action == "finish" && args.iter().any(|a| a == "--wait") {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
                while reply.code == "authorization_pending"
                    && tokio::time::Instant::now() < deadline
                {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    reply = call(method, json!({}), None, true).await?;
                }
            }
            return Ok(reply);
        }
        if provider == "azure" && action == "status" {
            return call("azure_cli_status", json!({}), None, true).await;
        }
        if action == "status" {
            let mut s = snapshot().await?;
            if s.ok {
                s.data = json!({"provider":provider,"connected":s.data[format!("{provider}_connected")],"revocation":"logout deletes local tokens only"});
            }
            return Ok(s);
        }
        let method = match (provider, action) {
            ("azure", "login") => "begin_azure_cli_login",
            ("azure", "finish") => "finish_azure_cli_login",
            ("azure", "cancel") => "cancel_azure_cli_login",
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
        if ["github", "azure"].contains(&provider) && action == "login" && reply.ok {
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
        ("people" | "questions", "list") => {
            snap.data = data["map"][command].clone();
            if snap.data.is_null() {
                snap.data = json!([]);
            }
            return Ok(snap);
        }
        ("people" | "questions", "add") => {
            let person = input()?;
            let map = data["map"].as_object_mut().context("invalid map")?;
            map.entry(command)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .context("invalid people")?
                .push(person);
        }
        ("people" | "questions", "show" | "edit" | "remove") => {
            let id = positional(&args, 2)?;
            let list = data["map"][command]
                .as_array_mut()
                .context("unknown person")?;
            let i = list
                .iter()
                .position(|p| p["id"] == id)
                .context("unknown person")?;
            match action {
                "show" => {
                    snap.data = list[i].clone();
                    return Ok(snap);
                }
                "edit" => {
                    let person = input()?;
                    ensure!(person["id"] == id, "person ID cannot change");
                    list[i] = person;
                }
                _ => {
                    list.remove(i);
                }
            }
        }
        ("cases", "list") => {
            snap.data = json!({"built_in": snap.data["cases"], "changes": data["map"]["cases"]});
            return Ok(snap);
        }
        ("cases", "show") => {
            let id = positional(&args, 2)?;
            let built = snap.data["cases"]
                .as_array()
                .and_then(|cases| cases.iter().find(|c| c["id"] == id))
                .cloned()
                .context("unknown case")?;
            let change = data["map"]["cases"]
                .as_array()
                .and_then(|cases| cases.iter().find(|c| c["id"] == id))
                .cloned();
            snap.data = json!({"built_in": built, "change": change});
            return Ok(snap);
        }
        ("cases", "edit" | "reset") => {
            let id = positional(&args, 2)?;
            let map = data["map"].as_object_mut().context("invalid map")?;
            let list = map
                .entry("cases")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .context("invalid cases")?;
            list.retain(|c| c["id"] != id);
            if action == "edit" {
                let mut case = input()?;
                ensure!(case.is_object(), "a case change is a JSON object");
                ensure!(
                    case.get("id").is_none_or(|v| v == id),
                    "case ID cannot change"
                );
                case["id"] = json!(id);
                list.push(case);
            }
        }
        ("contexts", "list") => {
            snap.data = data["map"]["contexts"].clone();
            if snap.data.is_null() {
                snap.data = json!([]);
            }
            return Ok(snap);
        }
        ("contexts", "add") => {
            let context = input()?;
            let map = data["map"].as_object_mut().context("invalid map")?;
            let list = map.entry("contexts").or_insert_with(|| json!([]));
            list.as_array_mut()
                .context("invalid contexts")?
                .push(context);
        }
        ("contexts", "show" | "edit" | "remove") => {
            let id = positional(&args, 2)?;
            let list = data["map"]["contexts"]
                .as_array_mut()
                .context("unknown context")?;
            let i = list
                .iter()
                .position(|c| c["id"] == id)
                .context("unknown context")?;
            match action {
                "show" => {
                    snap.data = list[i].clone();
                    return Ok(snap);
                }
                "edit" => {
                    let context = input()?;
                    ensure!(context["id"] == id, "context ID cannot change");
                    list[i] = context;
                }
                _ => {
                    list.remove(i);
                }
            }
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
    if args.iter().any(|a| a == "--version") && !args.iter().any(|a| a == "variables") {
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
    // The MCP server owns stdout for its protocol: it never prints a reply.
    let positional: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();
    if positional.first() == Some(&"mcp") {
        match positional.get(1) {
            None => {
                // Like any command: a host left from an update is replaced, keeping the
                // assistant's state (its notes go to stderr, never to the protocol).
                let _ = retire_outdated_host("mcp", "", false).await;
                return super::mcp::Server::new().serve().await;
            }
            Some(&"config") => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&super::mcp::config()).unwrap()
                );
                return 0;
            }
            _ => {
                eprintln!("Usage: pta mcp | pta mcp config");
                return 2;
            }
        }
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
            if input.starts_with("incompatible") || input.starts_with("Incompatible") {
                Reply::error(
                    "state_conflict",
                    6,
                    "Update the host and CLI together before continuing. Required security or schema capabilities are unavailable.",
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
    fn machine_pairing_output_flag_accepts_only_private_absolute_paths_for_authorization() {
        let output = std::env::temp_dir().join("synthetic-pairing.json");
        let mut args: Vec<String> = [
            "web",
            "machines",
            "authorize",
            "11111111-1111-4111-8111-111111111111",
            "--output",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        args.push(output.to_string_lossy().into_owned());
        assert!(validate_args(&args).is_ok());
        *args.last_mut().unwrap() = "relative.json".into();
        assert!(validate_args(&args).is_err());
        assert!(
            validate_args(&[
                "status".into(),
                "--output".into(),
                output.to_string_lossy().into_owned()
            ])
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
            &["mcp"],
            &["mcp", "config"],
            &["tools", "list"],
            &["tools", "call", "list_azure_hus"],
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
            &["web", "callbacks"],
            &["web", "users", "bind", "alice"],
            &["web", "users", "unbind", "alice"],
            &["web", "users", "revoke", "alice"],
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
            &["auth", "azure", "status"],
            &["auth", "azure", "login"],
            &["auth", "azure", "finish"],
            &["auth", "azure", "cancel"],
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
            &["answer", "refine", "resource"],
            &["answer", "send", "resource"],
            &["answer", "feedback", "resource"],
            &["people", "list"],
            &["people", "edit", "ana"],
            &["questions", "list"],
            &["questions", "add"],
            &["questions", "associate", "resource"],
            &["cases", "list"],
            &["cases", "edit", "status_round"],
            &["cases", "reset", "recap"],
            &["contexts", "list"],
            &["contexts", "edit", "cobros"],
            &["notes", "list"],
            &["notes", "search", "hotfix"],
            &["notes", "add"],
            &["notes", "remove", "note-id"],
            &["knowledge", "search", "conciliación"],
            &["index", "status"],
            &["index", "refresh", "handbook"],
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
    fn activity_context_is_opt_in_stdin_only_for_runs() {
        for command in [
            vec!["activity", "run", "--context-stdin"],
            vec!["activity", "run", "2026-10-13", "--context-stdin"],
            vec!["answer", "refine", "chats/x/messages/y", "--context-stdin"],
            vec![
                "answer",
                "refine",
                "chats/x/messages/y",
                "--source",
                "manuals",
                "--source",
                "azure-devops-status",
            ],
            vec!["notes", "list", "--context", "cobros", "--limit", "5"],
            vec![
                "knowledge",
                "search",
                "pagos",
                "--source",
                "handbook",
                "--context",
                "cobros",
            ],
        ] {
            assert!(
                validate_args(&command.into_iter().map(String::from).collect::<Vec<_>>()).is_ok()
            );
        }
        for command in [
            vec!["activity", "history", "--context-stdin"],
            vec!["activity", "run", "--context", "prompt"],
            vec!["answer", "send", "chats/x/messages/y", "--context-stdin"],
            vec![
                "answer",
                "send",
                "chats/x/messages/y",
                "--source",
                "manuals",
            ],
            vec!["answer", "refine", "chats/x/messages/y", "--source"],
        ] {
            assert!(
                validate_args(&command.into_iter().map(String::from).collect::<Vec<_>>()).is_err()
            );
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
