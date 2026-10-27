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
    serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
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
