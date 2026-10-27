//! A real host and the real `pta` binary with a private synthetic profile: protected entry,
//! metadata-only discovery and the absence of values in argv, replies, logs and files.
#![cfg(unix)]
use personal_teams_assistant::config::Config;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
};
use tokio::io::AsyncWriteExt;

const SECRET: &str = "synthetic-cli-secret-9e8d7c6b";
const MULTILINE: &str = "synthetic-line-one\nsynthetic-line-two";

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    profile: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    _host: Host,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let (profile, home, bin) = (
        root.path().join("profile"),
        root.path().join("home"),
        root.path().join("bin"),
    );
    for dir in [&profile, &home, &bin] {
        fs::create_dir_all(dir).unwrap();
    }
    let mut config = Config::desktop_template().unwrap();
    config.server.data_dir = profile.join("data");
    config.server.cloudflare_tunnel = false;
    config.graph.user_id = uuid::Uuid::nil().to_string();
    config.knowledge_map = profile.join("knowledge-map.toml");
    fs::create_dir_all(profile.join("data/repositories")).unwrap();
    fs::create_dir_all(profile.join("credentials/default")).unwrap();
    fs::write(
        profile.join("credentials/default/STATE_ENCRYPTION_KEY"),
        personal_teams_assistant::security::random_secret(),
    )
    .unwrap();
    fs::write(
        profile.join("config.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    let mut child = Host(
        Command::new(env!("CARGO_BIN_EXE_personal-teams-assistant"))
            .arg("--headless")
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &bin)
            .env("TOKIO_WORKER_THREADS", "2")
            .env("PTA_PROFILE_DIR", &profile)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let started = std::time::Instant::now();
    loop {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "Synthetic host exited"
        );
        if fs::read(profile.join("control.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some()
        {
            break;
        }
        assert!(started.elapsed().as_secs() < 20, "host did not start");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Fixture {
        _root: root,
        profile,
        home,
        bin,
        _host: child,
    }
}

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Fixture {
    async fn pta(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_pta"))
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .env("PTA_PROFILE_DIR", &self.profile)
            .env("TOKIO_WORKER_THREADS", "2")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut pipe = child.stdin.take().unwrap();
        if let Some(input) = stdin {
            pipe.write_all(input.as_bytes()).await.unwrap();
        }
        drop(pipe);
        let output = child.wait_with_output().await.unwrap();
        Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
    async fn json(&self, args: &[&str], stdin: Option<&str>) -> (i32, Value) {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        let output = self.pta(&full, stdin).await;
        assert!(!output.stdout.contains(SECRET) && !output.stderr.contains(SECRET));
        (
            output.code,
            serde_json::from_str(&output.stdout)
                .unwrap_or_else(|_| panic!("not JSON: {} / {}", output.stdout, output.stderr)),
        )
    }
    /// Everything on disk under the profile, as one byte string.
    fn disk(&self) -> Vec<u8> {
        let mut all = Vec::new();
        let mut stack = vec![self.profile.clone()];
        while let Some(path) = stack.pop() {
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(bytes) = fs::read(&path) {
                    all.extend(bytes);
                }
            }
        }
        all
    }
    async fn mcp(&self, calls: &[(&str, Value)]) -> Vec<Value> {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_pta"))
            .arg("mcp")
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .env("PTA_PROFILE_DIR", &self.profile)
            .env("TOKIO_WORKER_THREADS", "2")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut input = String::new();
        for (index, (name, arguments)) in calls.iter().enumerate() {
            input += &json!({"jsonrpc":"2.0","id":index + 1,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string();
            input.push('\n');
        }
        input += &json!({"jsonrpc":"2.0","id":99,"method":"tools/list"}).to_string();
        input.push('\n');
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(input.as_bytes()).await.unwrap();
        drop(pipe);
        let output = child.wait_with_output().await.unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains(SECRET));
        text.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn tool_json(reply: &Value) -> Value {
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool text in {reply}"));
    serde_json::from_str(text).unwrap_or_else(|_| panic!("not JSON: {text}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_creates_variables_from_a_protected_pipe_and_lists_only_metadata() {
    let f = fixture();
    let (code, created) = f
        .json(
            &[
                "variables",
                "create",
                "DEPLOY_ENDPOINT",
                "--description",
                "Where releases go",
            ],
            Some(&format!("{SECRET}\n")),
        )
        .await;
    assert_eq!(code, 0, "{created}");
    assert_eq!(created["data"]["variable"]["name"], "DEPLOY_ENDPOINT");
    assert_eq!(created["data"]["variable"]["version"], 1);

    // Multiline values are accepted and a retry with the printed entry ID is a replay.
    let id = uuid::Uuid::new_v4().to_string();
    let args = [
        "variables",
        "create",
        "signing_key",
        "--description",
        "Signs releases",
        "--id",
        &id,
    ];
    let (code, first) = f.json(&args, Some(MULTILINE)).await;
    assert_eq!(code, 0, "{first}");
    let (code, retry) = f.json(&args, Some(MULTILINE)).await;
    assert_eq!(code, 0, "{retry}");
    assert_eq!(retry["data"]["replayed"], true);

    // A different entry never overwrites an existing name, in any case.
    let (code, conflict) = f
        .json(
            &[
                "variables",
                "create",
                "deploy_endpoint",
                "--description",
                "Another",
            ],
            Some("synthetic-other-value\n"),
        )
        .await;
    assert_eq!(
        (code, conflict["code"].as_str()),
        (6, Some("state_conflict"))
    );

    let (code, list) = f.json(&["variables", "list"], None).await;
    assert_eq!(code, 0);
    assert_eq!(list["data"]["total"], 2);
    assert_eq!(list["data"]["variables"][0]["name"], "DEPLOY_ENDPOINT");
    assert!(list["data"]["variables"][0].get("value").is_none());
    let (_, search) = f.json(&["variables", "search", "signs"], None).await;
    assert_eq!(search["data"]["variables"][0]["name"], "signing_key");
    let (code, verify) = f.json(&["variables", "verify"], None).await;
    assert_eq!(
        (
            code,
            verify["data"]["ok"].as_bool(),
            verify["data"]["checked"].as_u64()
        ),
        (0, Some(true), Some(2))
    );

    // The value is neither on disk in clear nor in the host's event log.
    let disk = f.disk();
    for secret in [SECRET, "synthetic-line-one", "synthetic-other-value"] {
        assert!(
            !disk.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "{secret}"
        );
    }
    let (_, logs) = f.json(&["logs", "--limit", "100"], None).await;
    assert!(!logs.to_string().contains("synthetic-"));
}

#[tokio::test(flavor = "multi_thread")]
async fn values_are_never_command_arguments_and_unsafe_forms_are_refused_before_the_host() {
    let f = fixture();
    // There is no value flag or positional value, so a secret cannot be put in argv.
    for args in [
        vec!["variables", "create", "A", "--description", "d", SECRET],
        vec![
            "variables",
            "create",
            "A",
            "--value",
            SECRET,
            "--description",
            "d",
        ],
        vec!["variables", "entry", "complete", "id", SECRET],
        vec!["variables", "reveal", "A"],
        vec!["variables", "get", "A"],
        vec!["variables", "create", "A", "--description"],
        vec![
            "variables",
            "create",
            "A",
            "--id",
            "not-a-uuid",
            "--description",
            "d",
        ],
        vec!["variables", "key", "backup", "--output", "relative.json"],
    ] {
        let output = f.pta(&args, Some(SECRET)).await;
        assert_ne!(output.code, 0, "{args:?}");
        assert!(
            !output.stdout.contains(SECRET) && !output.stderr.contains(SECRET),
            "{args:?}"
        );
    }
    let (_, list) = f.json(&["variables", "list"], None).await;
    assert_eq!(list["data"]["total"], 0);
    // A missing description is refused without contacting the host.
    let output = f.pta(&["variables", "create", "A"], Some(SECRET)).await;
    assert_ne!(output.code, 0);
    assert!(!output.stderr.contains(SECRET) && !output.stdout.contains(SECRET));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_requests_protected_entry_and_the_user_completes_it_without_the_value_in_mcp() {
    let f = fixture();
    let replies = f
        .mcp(&[
            (
                "variable_entry_request",
                json!({"name":"PARTNER_ENDPOINT","description":"Partner base URL"}),
            ),
            ("variable_list", json!({})),
        ])
        .await;
    let request = tool_json(&replies[0]);
    assert_eq!(request["state"], "pending");
    let id = request["request"].as_str().unwrap().to_owned();
    assert_eq!(tool_json(&replies[1])["pending"][0]["request"], id);
    // The advertised tools have no value-bearing input.
    let tools = replies.last().unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    for tool in &tools {
        let properties = tool["inputSchema"]["properties"].as_object().unwrap();
        assert!(!properties.contains_key("value"), "{}", tool["name"]);
    }
    for name in [
        "variable_list",
        "variable_search",
        "variable_entry_request",
        "variable_entry_status",
        "variable_entry_cancel",
    ] {
        assert!(tools.iter().any(|t| t["name"] == name), "{name}");
    }
    assert!(
        !tools
            .iter()
            .any(|t| t["name"].as_str().unwrap().starts_with("variable_create"))
    );

    // The user completes it on their own channel; the host process holds it for 15 minutes.
    let (code, done) = f
        .json(
            &["variables", "entry", "complete", &id],
            Some(&format!("{SECRET}\n")),
        )
        .await;
    assert_eq!(code, 0, "{done}");
    let replies = f
        .mcp(&[
            ("variable_entry_status", json!({"request":id})),
            ("variable_search", json!({"query":"partner"})),
            (
                "variable_entry_request",
                json!({"name":"PARTNER_ENDPOINT","description":"again"}),
            ),
        ])
        .await;
    assert_eq!(tool_json(&replies[0])["state"], "completed");
    assert_eq!(
        tool_json(&replies[1])["variables"][0]["name"],
        "PARTNER_ENDPOINT"
    );
    assert_eq!(replies[2]["result"]["isError"], true);
    assert!(
        !f.disk()
            .windows(SECRET.len())
            .any(|w| w == SECRET.as_bytes())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_key_commands_back_up_restore_and_rotate_through_the_cli() {
    let f = fixture();
    f.json(
        &["variables", "create", "ONE", "--description", "First"],
        Some(SECRET),
    )
    .await;
    let backup = f.home.join("variable-keys.json");
    let path = backup.to_str().unwrap();
    let (code, written) = f
        .json(&["variables", "key", "backup", "--output", path], None)
        .await;
    assert_eq!(code, 0, "{written}");
    let file = fs::read_to_string(&backup).unwrap();
    assert!(!file.contains("ONE") && !file.contains(SECRET));
    let (code, again) = f
        .json(&["variables", "key", "backup", "--output", path], None)
        .await;
    assert_eq!((code, again["code"].as_str()), (6, Some("state_conflict")));

    let key = f.profile.join("keys/master-1.key");
    fs::remove_file(&key).unwrap();
    let (code, lost) = f
        .json(
            &["variables", "create", "TWO", "--description", "Second"],
            Some("synthetic-two-value"),
        )
        .await;
    assert_eq!((code, lost["code"].as_str()), (3, Some("not_ready")));
    assert!(!key.exists(), "no key was regenerated");
    let (code, restored) = f.json(&["variables", "key", "restore"], Some(&file)).await;
    assert_eq!(code, 0, "{restored}");
    assert_eq!(restored["data"]["ok"], true);
    let (code, rotated) = f.json(&["variables", "key", "rotate"], None).await;
    assert_eq!(code, 0, "{rotated}");
    assert_eq!(rotated["data"]["rotation"]["master_version"], 2);
    let (_, verify) = f.json(&["variables", "verify"], None).await;
    assert_eq!(verify["data"]["ok"], true);
}

const NEXT: &str = "synthetic-rotated-value-4f3e2d1c";

#[tokio::test(flavor = "multi_thread")]
async fn cli_edits_rotates_and_deletes_with_versions_and_a_protected_pipe() {
    let f = fixture();
    let (code, created) = f
        .json(
            &["variables", "create", "EDITED", "--description", "Before"],
            Some(SECRET),
        )
        .await;
    assert_eq!(code, 0, "{created}");
    let entry = created["data"]["variable"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Description only, then value only through a pipe: the ID and the other field stay.
    let (code, described) = f
        .json(
            &[
                "variables",
                "update",
                &entry,
                "--expected-version",
                "1",
                "--description",
                "After",
            ],
            None,
        )
        .await;
    assert_eq!(code, 0, "{described}");
    assert_eq!(described["data"]["variable"]["version"], 2, "{described}");
    assert_eq!(described["data"]["variable"]["description"], "After");
    let (code, rotated) = f
        .json(
            &[
                "variables",
                "update",
                &entry,
                "--expected-version",
                "2",
                "--value",
            ],
            Some(&format!("{NEXT}\n")),
        )
        .await;
    assert_eq!(code, 0, "{rotated}");
    assert_eq!(rotated["data"]["variable"]["version"], 3, "{rotated}");
    assert_eq!(rotated["data"]["variable"]["id"], entry.as_str());
    assert_eq!(rotated["data"]["variable"]["description"], "After");
    assert!(!rotated.to_string().contains(NEXT));

    // A stale version is a conflict that changes nothing; a value is never an argument.
    let (code, stale) = f
        .json(
            &[
                "variables",
                "update",
                &entry,
                "--expected-version",
                "1",
                "--description",
                "Older",
            ],
            None,
        )
        .await;
    assert_eq!((code, stale["code"].as_str()), (6, Some("state_conflict")));
    for args in [
        vec![
            "variables",
            "update",
            &entry,
            "--expected-version",
            "3",
            "--value",
            NEXT,
        ],
        vec!["variables", "update", &entry, "--expected-version", "3"],
        vec!["variables", "update", &entry, "--description", "No version"],
        vec![
            "variables",
            "update",
            &entry,
            "--expected-version",
            "0",
            "--description",
            "Zero",
        ],
        vec![
            "variables",
            "update",
            "not-a-uuid",
            "--expected-version",
            "3",
            "--description",
            "Bad ID",
        ],
        // `--version` is the CLI's own option: it must never look like a successful edit.
        vec![
            "variables",
            "update",
            &entry,
            "--version",
            "3",
            "--description",
            "Wrong flag",
        ],
        vec!["variables", "delete", &entry, "--version", "3"],
        vec!["variables", "reveal", &entry],
        vec!["variables", "copy", &entry],
        vec!["tools", "call", "variable_reveal"],
    ] {
        let output = f.pta(&args, Some(NEXT)).await;
        assert_ne!(output.code, 0, "{args:?}");
        assert!(
            !output.stdout.contains(NEXT) && !output.stderr.contains(NEXT),
            "{args:?}"
        );
    }
    // Replacing from an empty or oversized pipe is refused and the value is unchanged.
    let (code, _) = f
        .json(
            &[
                "variables",
                "update",
                &entry,
                "--expected-version",
                "3",
                "--value",
            ],
            Some(""),
        )
        .await;
    assert_ne!(code, 0);
    let (_, list) = f.json(&["variables", "list"], None).await;
    assert_eq!(list["data"]["variables"][0]["version"], 3);
    assert_eq!(list["data"]["variables"][0]["description"], "After");

    // An agent-style replacement request is completed by the user on their own channel.
    let (code, asked) = f
        .json(&["variables", "entry", "replace", &entry], None)
        .await;
    assert_eq!(code, 0, "{asked}");
    assert_eq!(asked["data"]["action"], "replace_value");
    let request = asked["data"]["request"].as_str().unwrap().to_owned();
    let (code, done) = f
        .json(
            &["variables", "entry", "complete", &request],
            Some("synthetic-third-value\n"),
        )
        .await;
    assert_eq!(code, 0, "{done}");
    assert_eq!(done["data"]["variable"]["version"], 4);

    // Deletion names the version seen and is final for later writes.
    let (code, early) = f
        .json(
            &["variables", "delete", &entry, "--expected-version", "3"],
            None,
        )
        .await;
    assert_eq!((code, early["code"].as_str()), (6, Some("state_conflict")));
    let (code, deleted) = f
        .json(
            &["variables", "delete", &entry, "--expected-version", "4"],
            None,
        )
        .await;
    assert_eq!(code, 0, "{deleted}");
    assert_eq!(deleted["data"]["deleted"]["name"], "EDITED");
    let (_, empty) = f.json(&["variables", "list"], None).await;
    assert_eq!(empty["data"]["total"], 0);
    let (code, late) = f
        .json(
            &[
                "variables",
                "update",
                &entry,
                "--expected-version",
                "4",
                "--description",
                "Revived",
            ],
            None,
        )
        .await;
    assert_eq!((code, late["code"].as_str()), (6, Some("state_conflict")));
    assert!(late["message"].as_str().unwrap().contains("was deleted"));

    // The operator's audit names action, entry and outcome, never a value, name or description.
    let (code, trail) = f.json(&["variables", "audit"], None).await;
    assert_eq!(code, 0, "{trail}");
    let events: Vec<(&str, &str)> = trail["data"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["action"].as_str().unwrap(),
                e["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    for expected in [
        ("update", "ok"),
        ("update", "stale"),
        ("rotate", "ok"),
        ("delete", "ok"),
        ("update", "deleted"),
    ] {
        assert!(events.contains(&expected), "{expected:?} in {events:?}");
    }
    // An event names its entry; only a malformed ID (nothing valid to name) leaves it empty.
    for event in trail["data"]["events"].as_array().unwrap() {
        assert!(
            event["entry"].is_null() || event["entry"] == entry.as_str(),
            "{event}"
        );
    }
    assert!(
        trail["data"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["entry"] == entry.as_str())
    );
    for hidden in [SECRET, NEXT, "EDITED", "After", "synthetic-third-value"] {
        assert!(!trail.to_string().contains(hidden), "{hidden}");
    }
    let disk = f.disk();
    for secret in [SECRET, NEXT, "synthetic-third-value"] {
        assert!(
            !disk.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "{secret}"
        );
    }
    let (_, logs) = f.json(&["logs", "--limit", "200"], None).await;
    assert!(!logs.to_string().contains("synthetic-"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_edits_metadata_and_requests_replacements_but_has_no_way_to_read_a_value() {
    let f = fixture();
    let (_, created) = f
        .json(
            &[
                "variables",
                "create",
                "AGENT_VISIBLE",
                "--description",
                "Original",
            ],
            Some(SECRET),
        )
        .await;
    let entry = created["data"]["variable"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let replies = f
        .mcp(&[
            (
                "variable_update",
                json!({"id":entry,"version":1,"description":"Agent edit"}),
            ),
            (
                "variable_update",
                json!({"id":entry,"version":1,"description":"Stale"}),
            ),
            (
                "variable_update",
                json!({"id":entry,"version":2,"value":NEXT}),
            ),
            (
                "variable_update",
                json!({"id":entry,"version":2,"description":"d","account":"other"}),
            ),
            ("variable_rotation_request", json!({"id":entry})),
            ("variable_delete", json!({"id":entry,"version":2})),
            ("variable_list", json!({})),
        ])
        .await;
    // A refused call is a protocol error (invalid arguments) or a tool result flagged as an error.
    let refused = |reply: &Value| reply["error"].is_object() || reply["result"]["isError"] == true;
    assert!(!refused(&replies[0]), "{}", replies[0]);
    assert_eq!(tool_json(&replies[0])["variable"]["version"], 2);
    assert!(refused(&replies[1]), "a stale edit is a conflict");
    assert!(refused(&replies[2]), "no tool accepts a value");
    assert!(refused(&replies[3]), "authority is never an argument");
    assert!(!refused(&replies[4]), "{}", replies[4]);
    let request = tool_json(&replies[4]);
    assert_eq!(request["action"], "replace_value");
    assert!(
        refused(&replies[5]),
        "deleting needs the user's confirmation"
    );
    assert!(replies[5].to_string().contains("confirm"));
    assert!(!refused(&replies[6]), "{}", replies[6]);
    let listed = tool_json(&replies[6]);
    assert_eq!(listed["total"], 1, "an unconfirmed delete deleted nothing");
    assert_eq!(listed["variables"][0]["description"], "Agent edit");
    assert_eq!(listed["pending"][0]["action"], "replace_value");

    // The advertised tools can edit metadata and delete, and none reads, copies or takes a value.
    let tools = replies.last().unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    for name in [
        "variable_update",
        "variable_rotation_request",
        "variable_delete",
    ] {
        assert!(tools.iter().any(|t| t["name"] == name), "{name}");
    }
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        assert!(
            !["reveal", "copy", "read", "get_value", "show"]
                .iter()
                .any(|word| name.contains(word) && name.starts_with("variable_")),
            "{name}"
        );
        let properties = tool["inputSchema"]["properties"].as_object().unwrap();
        assert!(
            !["value", "secret", "password"]
                .iter()
                .any(|key| properties.contains_key(*key)),
            "{name}"
        );
    }

    // With the user's approval the same call deletes; the replacement is completed by the user.
    let (code, done) = f
        .json(
            &[
                "variables",
                "entry",
                "complete",
                request["request"].as_str().unwrap(),
            ],
            Some(&format!("{NEXT}\n")),
        )
        .await;
    assert_eq!(code, 0, "{done}");
    let replies = f
        .mcp(&[
            (
                "variable_entry_status",
                json!({"request":request["request"]}),
            ),
            (
                "variable_delete",
                json!({"id":entry,"version":3,"confirmed":true}),
            ),
            ("variable_search", json!({"query":"agent"})),
        ])
        .await;
    assert_eq!(tool_json(&replies[0])["state"], "completed");
    assert_eq!(tool_json(&replies[0])["action"], "replace_value");
    assert!(!refused(&replies[1]), "{}", replies[1]);
    assert_eq!(tool_json(&replies[2])["total"], 0);
    assert!(!f.disk().windows(NEXT.len()).any(|w| w == NEXT.as_bytes()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_consumer_uses_a_variable_by_reference_and_nothing_shows_the_value() {
    let f = fixture();
    let (_, created) = f
        .json(
            &[
                "variables",
                "create",
                "DEEPSEEK_KEY",
                "--description",
                "My DeepSeek key",
            ],
            Some(SECRET),
        )
        .await;
    let entry = created["data"]["variable"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = || async {
        let (_, credentials) = f.json(&["credentials", "list"], None).await;
        credentials["data"]["DEEPSEEK_API_KEY"].clone()
    };
    let before = source().await;
    assert_ne!(before, "variable");

    // Granting access needs the user's confirmation; a refused command changes nothing.
    let (code, refused) = f
        .json(&["variables", "bind", "deepseek", &entry], None)
        .await;
    assert_eq!(code, 2, "{refused}");
    assert!(refused["message"].as_str().unwrap().contains("--confirmed"));
    let (_, listed) = f.json(&["variables", "consumers"], None).await;
    assert!(listed["data"]["consumers"][0]["binding"].is_null());
    for args in [
        vec![
            "variables",
            "bind",
            "https://attacker.invalid",
            &entry,
            "--confirmed",
        ],
        vec!["variables", "bind", "deepseek", "not-a-uuid", "--confirmed"],
        vec![
            "variables",
            "check",
            "deepseek",
            "--confirmed",
            "--destination",
            "x",
        ],
        vec![
            "variables",
            "launcher",
            "run",
            "x",
            "--confirmed",
            "--env",
            "A=B",
        ],
    ] {
        let (code, _) = f.json(&args, None).await;
        assert_ne!(code, 0, "{args:?}");
    }

    let (code, bound) = f
        .json(
            &["variables", "bind", "deepseek", &entry, "--confirmed"],
            None,
        )
        .await;
    assert_eq!(code, 0, "{bound}");
    assert_eq!(bound["data"]["binding"]["variable"]["name"], "DEEPSEEK_KEY");
    let (_, listed) = f.json(&["variables", "consumers"], None).await;
    let consumer = &listed["data"]["consumers"][0];
    assert_eq!(consumer["destination"], "https://api.deepseek.com");
    assert_eq!(consumer["binding"]["variable"]["version"], 1);
    // The application credential now comes from the variable (real host, real lookup).
    assert_eq!(source().await, "variable");

    // An agent sees references and statuses; a launcher is refused outside allowed roots.
    let replies = f
        .mcp(&[
            ("consumer_list", json!({})),
            ("consumer_bind", json!({"consumer":"deepseek","variable":entry})),
            ("consumer_unbind", json!({"consumer":"deepseek"})),
            ("consumer_bind", json!({"consumer":"deepseek","variable":entry,"value":SECRET})),
            ("consumer_bind", json!({"consumer":"deepseek","variable":entry,"confirmed":true})),
            (
                "launcher_define",
                json!({"id":"tool","description":"d","program":"/bin/true","directory":"/","env":{"TOKEN":entry},"confirmed":true}),
            ),
        ])
        .await;
    let refused = |reply: &Value| reply["error"].is_object() || reply["result"]["isError"] == true;
    assert!(!refused(&replies[0]), "{}", replies[0]);
    let seen = tool_json(&replies[0]);
    assert_eq!(
        seen["consumers"][0]["binding"]["variable"]["name"],
        "DEEPSEEK_KEY"
    );
    assert!(refused(&replies[1]), "binding needs confirmation");
    assert!(!refused(&replies[2]), "{}", replies[2]);
    assert!(refused(&replies[3]), "no tool accepts a value");
    assert!(!refused(&replies[4]), "{}", replies[4]);
    assert!(
        refused(&replies[5]),
        "empty allowed roots grant no execution"
    );
    let tools = replies.last().unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    for name in [
        "consumer_list",
        "consumer_bind",
        "consumer_unbind",
        "consumer_check",
        "launcher_define",
        "launcher_remove",
        "launcher_run",
    ] {
        assert!(tools.iter().any(|t| t["name"] == name), "{name}");
    }

    // Deleting the variable leaves the binding in place and unusable: no fallback.
    let (_, current) = f.json(&["variables", "list"], None).await;
    let version = current["data"]["variables"][0]["version"]
        .as_u64()
        .unwrap()
        .to_string();
    let (code, _) = f
        .json(
            &[
                "variables",
                "delete",
                &entry,
                "--expected-version",
                &version,
            ],
            None,
        )
        .await;
    assert_eq!(code, 0);
    let (_, listed) = f.json(&["variables", "consumers"], None).await;
    assert_eq!(
        listed["data"]["consumers"][0]["binding"]["state"],
        "deleted"
    );
    let (code, checked) = f
        .json(&["variables", "check", "deepseek", "--confirmed"], None)
        .await;
    assert_ne!(code, 0, "{checked}");
    assert!(checked["message"].as_str().unwrap().contains("deleted"));
    let (_, unbound) = f.json(&["variables", "unbind", "deepseek"], None).await;
    assert_eq!(unbound["data"]["unbound"], true);
    assert_ne!(source().await, "variable");
    assert!(
        !f.disk()
            .windows(SECRET.len())
            .any(|w| w == SECRET.as_bytes())
    );
}

#[test]
fn the_public_fixtures_hold_no_synthetic_variable_values() {
    // Source, preview data and packaged skill text may describe the feature but never carry a
    // stored value or a key file.
    fn scan(dir: &Path, hits: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if !["target", ".git", "node_modules"].contains(&name.as_str()) {
                    scan(&path, hits);
                }
            } else if name.ends_with(".db") || name.starts_with("master-") && name.ends_with(".key")
            {
                hits.push(path);
            }
        }
    }
    let mut hits = Vec::new();
    scan(Path::new(env!("CARGO_MANIFEST_DIR")), &mut hits);
    assert!(hits.is_empty(), "{hits:?}");
}
