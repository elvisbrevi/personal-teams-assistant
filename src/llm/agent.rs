//! Codex and Claude Code as non-interactive model calls through their installed CLIs.
//!
//! Each call runs the CLI with its own login, in a fresh empty private directory, with a
//! minimal environment (no host credentials), every tool disabled, no session persistence and
//! the untrusted request on stdin. Only the final JSON message is used.
use super::{Backend, Failure, Unavailable, efforts, usage_limited};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

/// Model calls have no deadline elsewhere, but a CLI can wait forever on its own retries.
const CLI_DEADLINE: Duration = Duration::from_secs(20 * 60);
const PROBE_DEADLINE: Duration = Duration::from_secs(20);

/// Codex features that could reach files, the network, other agents or user customizations.
/// Set through `-c features.NAME=false`, which older or newer CLIs ignore when unknown.
const CODEX_DISABLED_FEATURES: [&str; 26] = [
    "shell_tool",
    "unified_exec",
    "code_mode",
    "code_mode_host",
    "apps",
    "plugins",
    "remote_plugin",
    "browser_use",
    "browser_use_external",
    "in_app_browser",
    "computer_use",
    "image_generation",
    "view_image",
    "multi_agent",
    "multi_agent_v2",
    "enable_fanout",
    "goals",
    "hooks",
    "memories",
    "skill_search",
    "skill_mcp_dependency_install",
    "tool_suggest",
    "sleep_tool",
    "workspace_dependencies",
    "shell_snapshot",
    "standalone_web_search",
];

/// Variables a CLI needs to find its own login, configuration, locale and proxy. Host
/// credentials (`NAME`, `NAME_FILE`) are never passed.
const PASSED_ENV: [&str; 38] = [
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
    "TMP",
    "TEMP",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    metadata.is_file()
}

/// Find an installed CLI in PATH or the usual per-user install locations. A GUI started from
/// Finder has a minimal PATH, so `~/.local/bin` and Homebrew are checked too. Relative PATH
/// entries are skipped so nothing runs from the working directory.
pub(crate) fn locate(program: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{program}.exe")
    } else {
        program.to_owned()
    };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = home() {
        for dir in [
            ".local/bin",
            ".npm-global/bin",
            ".bun/bin",
            ".volta/bin",
            ".claude/local",
            ".cargo/bin",
            "bin",
        ] {
            dirs.push(home.join(dir));
        }
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs.into_iter()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&file))
        .find(|path| executable(path))
}

fn child_env(binary: &Path) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| {
            name.to_str()
                .is_some_and(|n| PASSED_ENV.contains(&n) || n.starts_with("LC_"))
        })
        .collect();
    let mut path: Vec<PathBuf> = binary.parent().map(Path::to_path_buf).into_iter().collect();
    if let Some(inherited) = std::env::var_os("PATH") {
        path.extend(std::env::split_paths(&inherited).filter(|d| d.is_absolute()));
    }
    path.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from));
    if let Ok(joined) = std::env::join_paths(path) {
        env.push(("PATH".into(), joined));
    }
    env
}

struct Output {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Run a CLI with `input` on stdin. The child is killed if the deadline passes or the caller
/// is cancelled (service stop).
async fn run(
    binary: &Path,
    args: &[OsString],
    input: &str,
    dir: &Path,
    deadline: Duration,
) -> Result<Output> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new(binary)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .envs(child_env(binary))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Unavailable(Failure::NotInstalled)
            } else {
                Unavailable(Failure::Failed)
            }
        })?;
    let mut stdin = child.stdin.take().context("CLI stdin unavailable")?;
    let input = input.to_owned();
    let writer = async move {
        // A CLI that exits early closes the pipe; its exit status reports the failure.
        let _ = stdin.write_all(input.as_bytes()).await;
    };
    let (_, output) = tokio::time::timeout(deadline, async {
        tokio::join!(writer, child.wait_with_output())
    })
    .await
    .map_err(|_| Unavailable(Failure::Failed))?;
    let output = output.map_err(|_| Unavailable(Failure::Failed))?;
    Ok(Output {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn workspace() -> Result<tempfile::TempDir> {
    // Empty and private (0700): nothing for the CLI to discover, nothing left behind.
    Ok(tempfile::Builder::new().prefix("pta-llm-").tempdir()?)
}

fn failure(text: &str) -> anyhow::Error {
    Unavailable(if usage_limited(text) {
        Failure::UsageLimit
    } else {
        Failure::Failed
    })
    .into()
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.into()).to_string()
}

/// OpenAI Codex through `codex exec`.
pub struct CodexCli {
    program: Option<PathBuf>,
    model: String,
    effort: String,
    label: String,
}
impl CodexCli {
    pub fn new(model: &str, effort: &str) -> Self {
        Self {
            program: None,
            model: model.into(),
            effort: effort.into(),
            label: format!("codex:{model}:{effort}"),
        }
    }
    #[cfg(test)]
    fn at(program: &Path, model: &str, effort: &str) -> Self {
        Self {
            program: Some(program.into()),
            ..Self::new(model, effort)
        }
    }
    fn args(&self, dir: &Path, schema: &Path, system: &str) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "exec",
            "--ephemeral",
            "--skip-git-repo-check",
            "--ignore-user-config",
            "--ignore-rules",
            "--sandbox",
            "read-only",
            "--color",
            "never",
            "--json",
        ]
        .map(OsString::from)
        .into();
        args.push("--cd".into());
        args.push(dir.into());
        args.push(format!("--model={}", self.model).into());
        args.push("--output-schema".into());
        args.push(schema.into());
        let settings = [
            format!("model_reasoning_effort={}", toml_string(&self.effort)),
            format!("developer_instructions={}", toml_string(system)),
            "approval_policy=\"never\"".into(),
            "web_search=\"disabled\"".into(),
            "skills.include_instructions=false".into(),
            "project_doc_max_bytes=0".into(),
            "tools.update_plan.enabled=false".into(),
            "tools.experimental_request_user_input.enabled=false".into(),
        ];
        let features = CODEX_DISABLED_FEATURES.map(|f| format!("features.{f}=false"));
        for setting in settings.into_iter().chain(features) {
            args.push("-c".into());
            args.push(setting.into());
        }
        args.push("-".into());
        args
    }
}
/// The final agent message of a completed `codex exec --json` turn.
fn codex_answer(output: &Output) -> Result<String> {
    let mut answer = None;
    let mut completed = false;
    let mut errors = String::new();
    for line in output.stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event["type"].as_str() {
            Some("item.completed") if event["item"]["type"] == "agent_message" => {
                answer = event["item"]["text"].as_str().map(str::to_owned);
            }
            Some("turn.completed") => completed = true,
            Some("turn.failed") => errors.push_str(&event["error"]["message"].to_string()),
            Some("error") => errors.push_str(&event["message"].to_string()),
            _ => {}
        }
    }
    match answer.filter(|a| !a.trim().is_empty()) {
        Some(answer) if completed && output.success && errors.is_empty() => Ok(answer),
        _ => Err(failure(&format!("{errors}\n{}", output.stderr))),
    }
}
#[async_trait]
impl Backend for CodexCli {
    fn label(&self) -> &str {
        &self.label
    }
    async fn complete_json(&self, system: &str, user: &str, schema: &Value) -> Result<String> {
        let binary = self
            .program
            .clone()
            .or_else(|| locate("codex"))
            .ok_or(Unavailable(Failure::NotInstalled))?;
        let dir = workspace()?;
        let schema_path = dir.path().join("schema.json");
        std::fs::write(&schema_path, schema.to_string())?;
        let output = run(
            &binary,
            &self.args(dir.path(), &schema_path, system),
            user,
            dir.path(),
            CLI_DEADLINE,
        )
        .await?;
        codex_answer(&output)
    }
}

/// Claude Code through `claude -p`.
pub struct ClaudeCli {
    program: Option<PathBuf>,
    model: String,
    effort: String,
    label: String,
}
impl ClaudeCli {
    pub fn new(model: &str, effort: &str) -> Self {
        Self {
            program: None,
            model: model.into(),
            effort: effort.into(),
            label: format!("claude:{model}:{effort}"),
        }
    }
    #[cfg(test)]
    fn at(program: &Path, model: &str, effort: &str) -> Self {
        Self {
            program: Some(program.into()),
            ..Self::new(model, effort)
        }
    }
    fn args(&self, system: &str, schema: &Value) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "-p",
            "--output-format",
            "json",
            "--no-session-persistence",
            "--safe-mode",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--tools",
            "",
        ]
        .map(OsString::from)
        .into();
        args.push(format!("--model={}", self.model).into());
        args.push(format!("--effort={}", self.effort).into());
        args.push("--system-prompt".into());
        args.push(system.into());
        args.push("--json-schema".into());
        args.push(schema.to_string().into());
        args
    }
}
/// The structured output of a successful `claude -p --output-format json` result.
fn claude_answer(output: &Output) -> Result<String> {
    let result = output
        .stdout
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value["type"] == "result");
    let Some(result) = result else {
        return Err(failure(&format!("{}\n{}", output.stdout, output.stderr)));
    };
    if !output.success || result["is_error"].as_bool() != Some(false) {
        if matches!(result["api_error_status"].as_u64(), Some(402 | 429)) {
            return Err(Unavailable(Failure::UsageLimit).into());
        }
        return Err(failure(&format!("{}\n{}", result["result"], output.stderr)));
    }
    match &result["structured_output"] {
        Value::Object(_) => Ok(result["structured_output"].to_string()),
        _ => result["result"]
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Unavailable(Failure::Failed).into()),
    }
}
#[async_trait]
impl Backend for ClaudeCli {
    fn label(&self) -> &str {
        &self.label
    }
    async fn complete_json(&self, system: &str, user: &str, schema: &Value) -> Result<String> {
        let binary = self
            .program
            .clone()
            .or_else(|| locate("claude"))
            .ok_or(Unavailable(Failure::NotInstalled))?;
        let dir = workspace()?;
        let output = run(
            &binary,
            &self.args(system, schema),
            user,
            dir.path(),
            CLI_DEADLINE,
        )
        .await?;
        claude_answer(&output)
    }
}

/// What the settings screen can offer: installed CLIs, their models and reasoning efforts.
#[derive(Serialize)]
pub struct Catalog {
    pub providers: Vec<ProviderInfo>,
}
#[derive(Serialize)]
pub struct ProviderInfo {
    pub id: &'static str,
    /// `cli` (installed command, its own login) or `api` (needs `credential`).
    pub transport: &'static str,
    pub available: bool,
    pub version: Option<String>,
    pub credential: Option<&'static str>,
    pub efforts: Vec<String>,
    pub models: Vec<ModelInfo>,
}
#[derive(Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
}
fn model(id: &str, name: &str, provider: &str, default_effort: &str) -> ModelInfo {
    ModelInfo {
        id: id.into(),
        name: name.into(),
        efforts: efforts(provider).iter().map(|e| e.to_string()).collect(),
        default_effort: default_effort.into(),
    }
}

async fn probe(binary: &Path, args: &[&str]) -> Option<String> {
    let dir = workspace().ok()?;
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let output = run(binary, &args, "", dir.path(), PROBE_DEADLINE)
        .await
        .ok()?;
    output.success.then_some(output.stdout)
}

fn version(text: &str) -> Option<String> {
    let line: String = text
        .lines()
        .next()?
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    Some(line.trim().to_owned()).filter(|l| !l.is_empty())
}

/// Codex publishes its model catalog (`codex debug models`) with the efforts each supports.
fn codex_models(catalog: &str) -> Vec<ModelInfo> {
    let Ok(catalog) = serde_json::from_str::<Value>(catalog) else {
        return Vec::new();
    };
    let allowed = efforts("codex");
    catalog["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"] == "list")
        .filter_map(|m| {
            let id = m["slug"].as_str().filter(|id| super::valid_model(id))?;
            let efforts: Vec<String> = m["supported_reasoning_levels"]
                .as_array()?
                .iter()
                .filter_map(|level| level["effort"].as_str())
                .filter(|effort| allowed.contains(effort))
                .map(str::to_owned)
                .collect();
            let default = m["default_reasoning_level"]
                .as_str()
                .filter(|d| efforts.iter().any(|e| e == d))
                .or(efforts.first().map(String::as_str))?
                .to_owned();
            Some(ModelInfo {
                id: id.into(),
                name: m["display_name"]
                    .as_str()
                    .unwrap_or(id)
                    .chars()
                    .take(80)
                    .collect(),
                efforts,
                default_effort: default,
            })
        })
        .collect()
}

pub async fn catalog() -> Catalog {
    let codex = locate("codex");
    let claude = locate("claude");
    let (codex_version, codex_catalog, claude_version) = tokio::join!(
        async {
            match &codex {
                Some(binary) => probe(binary, &["--version"]).await,
                None => None,
            }
        },
        async {
            match &codex {
                Some(binary) => probe(binary, &["debug", "models"]).await,
                None => None,
            }
        },
        async {
            match &claude {
                Some(binary) => probe(binary, &["--version"]).await,
                None => None,
            }
        },
    );
    let mut codex_models = codex_catalog
        .as_deref()
        .map(codex_models)
        .unwrap_or_default();
    if codex_models.is_empty() {
        codex_models.push(model("gpt-6.1-sol", "GPT-6.1-Sol", "codex", "medium"));
    }
    Catalog {
        providers: vec![
            ProviderInfo {
                id: "codex",
                transport: "cli",
                available: codex.is_some(),
                version: codex_version.as_deref().and_then(version),
                credential: None,
                efforts: efforts("codex").iter().map(|e| e.to_string()).collect(),
                models: codex_models,
            },
            ProviderInfo {
                id: "claude",
                transport: "cli",
                available: claude.is_some(),
                version: claude_version.as_deref().and_then(version),
                credential: None,
                efforts: efforts("claude").iter().map(|e| e.to_string()).collect(),
                models: vec![
                    model("claude-opus-5-5", "Claude Opus 5.5", "claude", "medium"),
                    model("claude-sonnet-5-5", "Claude Sonnet 5.5", "claude", "medium"),
                    model("claude-fable-5-1", "Claude Fable 5.1", "claude", "medium"),
                    model("claude-haiku-4-5", "Claude Haiku 4.5", "claude", "medium"),
                ],
            },
            ProviderInfo {
                id: "deepseek",
                transport: "api",
                available: true,
                version: None,
                credential: Some("DEEPSEEK_API_KEY"),
                efforts: efforts("deepseek").iter().map(|e| e.to_string()).collect(),
                models: vec![
                    model("deepseek-flash", "DeepSeek Flash", "deepseek", "max"),
                    model("deepseek-v4-pro", "DeepSeek V4 Pro", "deepseek", "max"),
                ],
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::failure_of;
    use serde_json::json;

    fn output(success: bool, stdout: &str) -> Output {
        Output {
            success,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    #[test]
    fn codex_arguments_disable_tools_and_keep_the_request_off_argv() {
        let cli = CodexCli::new("gpt-6.1-sol", "medium");
        let args: Vec<String> = cli
            .args(
                Path::new("/tmp/pta-llm-x"),
                Path::new("/tmp/pta-llm-x/schema.json"),
                "Responde \"solo\" JSON.\nEstilo: breve",
            )
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        for expected in [
            "exec",
            "--ephemeral",
            "--ignore-user-config",
            "--ignore-rules",
            "--skip-git-repo-check",
            "--model=gpt-6.1-sol",
            "model_reasoning_effort=\"medium\"",
            "features.shell_tool=false",
            "features.unified_exec=false",
            "features.multi_agent=false",
            "skills.include_instructions=false",
        ] {
            assert!(args.iter().any(|a| a == expected), "{expected}");
        }
        let sandbox = args.iter().position(|a| a == "--sandbox").unwrap();
        assert_eq!(args[sandbox + 1], "read-only");
        assert_eq!(args.last().unwrap(), "-");
        let instructions = args
            .iter()
            .find_map(|a| a.strip_prefix("developer_instructions="))
            .unwrap();
        let parsed: toml::Value = toml::from_str(&format!("v = {instructions}")).unwrap();
        assert_eq!(
            parsed["v"].as_str().unwrap(),
            "Responde \"solo\" JSON.\nEstilo: breve"
        );
        assert!(!args.iter().any(|a| a.contains("danger")));
    }

    #[test]
    fn claude_arguments_disable_every_tool() {
        let args: Vec<String> = ClaudeCli::new("claude-opus-5-5", "medium")
            .args("Sistema", &json!({"type":"object"}))
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        let tools = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[tools + 1], "");
        for expected in [
            "-p",
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--model=claude-opus-5-5",
            "--effort=medium",
        ] {
            assert!(args.iter().any(|a| a == expected), "{expected}");
        }
        assert!(!args.iter().any(|a| a.contains("dangerously")));
    }

    #[test]
    fn codex_output_uses_the_final_message_of_a_completed_turn() {
        let ok = [
            r#"{"type":"thread.started","thread_id":"t"}"#,
            r#"{"type":"item.completed","item":{"id":"item_0","type":"error","message":"Code Mode is unavailable"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"{\"answer\":\"Reviso.\",\"detailed\":false}"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"{\"answer\":\"Final.\",\"detailed\":false}"}}"#,
            r#"{"type":"turn.completed","usage":{}}"#,
        ]
        .join("\n");
        assert_eq!(
            codex_answer(&output(true, &ok)).unwrap(),
            r#"{"answer":"Final.","detailed":false}"#
        );
        let limited = [
            r#"{"type":"turn.started"}"#,
            r#"{"type":"error","message":"You've hit your usage limit. Try again later."}"#,
            r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit."}}"#,
        ]
        .join("\n");
        let error = codex_answer(&output(false, &limited)).unwrap_err();
        assert_eq!(failure_of(&error), Failure::UsageLimit);
        assert!(!error.to_string().contains("hit your"));
        let invalid = r#"{"type":"turn.failed","error":{"message":"{\"status\":400,\"error\":{\"message\":\"The 'x' model is not supported\"}}"}}"#;
        assert_eq!(
            failure_of(&codex_answer(&output(false, invalid)).unwrap_err()),
            Failure::Failed
        );
        // An unfinished turn never counts as an answer.
        let partial = r#"{"type":"item.completed","item":{"type":"agent_message","text":"{}"}}"#;
        assert!(codex_answer(&output(true, partial)).is_err());
    }

    #[test]
    fn claude_output_prefers_structured_output_and_classifies_credit_errors() {
        let ok = json!({"type":"result","is_error":false,"subtype":"success","result":"{\"answer\":\"Hola\",\"detailed\":false}","structured_output":{"answer":"Hola","detailed":false}}).to_string();
        let answer: Value =
            serde_json::from_str(&claude_answer(&output(true, &ok)).unwrap()).unwrap();
        assert_eq!(answer, json!({"answer":"Hola","detailed":false}));
        let credits = json!({"type":"result","is_error":true,"api_error_status":429,"result":"Fable 5.1 requires usage credits."}).to_string();
        let error = claude_answer(&output(false, &credits)).unwrap_err();
        assert_eq!(failure_of(&error), Failure::UsageLimit);
        let limit = json!({"type":"result","is_error":true,"api_error_status":null,"result":"Claude AI usage limit reached|1760000000"}).to_string();
        assert_eq!(
            failure_of(&claude_answer(&output(false, &limit)).unwrap_err()),
            Failure::UsageLimit
        );
        let missing = format!(
            "[claude-code:unrecognized_model] {{}}\n{}",
            json!({"type":"result","is_error":true,"api_error_status":404,"result":"There's an issue with the selected model"})
        );
        assert_eq!(
            failure_of(&claude_answer(&output(false, &missing)).unwrap_err()),
            Failure::Failed
        );
        assert!(claude_answer(&output(true, "not json")).is_err());
    }

    #[test]
    fn codex_catalog_lists_visible_models_without_delegating_efforts() {
        let catalog = json!({"models":[
            {"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol","visibility":"list","default_reasoning_level":"low",
             "supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"max"},{"effort":"ultra"}]},
            {"slug":"hidden","visibility":"hide","default_reasoning_level":"low","supported_reasoning_levels":[{"effort":"low"}]},
            {"slug":"--bad","visibility":"list","default_reasoning_level":"low","supported_reasoning_levels":[{"effort":"low"}]}
        ]});
        let models = codex_models(&catalog.to_string());
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-6.1-sol");
        assert_eq!(models[0].efforts, ["low", "medium", "max"]);
        assert_eq!(models[0].default_effort, "low");
        assert!(codex_models("not json").is_empty());
        assert_eq!(
            version("codex-cli 0.159.3\n").as_deref(),
            Some("codex-cli 0.159.3")
        );
    }

    /// A fake CLI that records its stdin, arguments and environment, then prints `reply`.
    #[cfg(unix)]
    fn fake_cli(dir: &Path, reply: &str, status: i32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-cli");
        let log = dir.join("log");
        let reply_path = dir.join("reply");
        std::fs::write(&reply_path, reply).unwrap();
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\ncat > '{log}.stdin'\nprintf '%s\\n' \"$@\" > '{log}.args'\nenv > '{log}.env'\npwd > '{log}.pwd'\ncat '{reply}'\nexit {status}\n",
                log = log.display(),
                reply = reply_path.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_receives_the_request_on_stdin_without_host_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let reply = json!({"type":"result","is_error":false,"result":"","structured_output":{"answer":"Hola","detailed":false}}).to_string();
        let program = fake_cli(dir.path(), &reply, 0);
        // SAFETY: test-only variable read by no other test.
        unsafe { std::env::set_var("PTA_FAKE_HOST_SECRET_FOR_TEST", "must-not-leak") };
        let cli = ClaudeCli::at(&program, "claude-opus-5-5", "medium");
        let answer = cli
            .complete_json("Sistema", "pregunta privada", &json!({"type":"object"}))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&answer).unwrap()["answer"],
            "Hola"
        );
        let log = dir.path().join("log");
        let read =
            |suffix: &str| std::fs::read_to_string(format!("{}.{suffix}", log.display())).unwrap();
        assert_eq!(read("stdin"), "pregunta privada");
        assert!(!read("args").contains("pregunta privada"));
        let env = read("env");
        assert!(!env.contains("must-not-leak"));
        assert!(!env.contains("PTA_FAKE_HOST_SECRET_FOR_TEST"));
        // The working directory was a private temporary directory, removed afterwards.
        let pwd = PathBuf::from(read("pwd").trim());
        assert!(
            pwd.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pta-llm-")
        );
        assert!(!pwd.exists());

        let limited =
            [r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit."}}"#]
                .join("\n");
        let program = fake_cli(dir.path(), &limited, 1);
        let error = CodexCli::at(&program, "gpt-6.1-sol", "medium")
            .complete_json("Sistema", "pregunta", &json!({"type":"object"}))
            .await
            .unwrap_err();
        assert_eq!(failure_of(&error), Failure::UsageLimit);
        let error = CodexCli::at(&dir.path().join("missing"), "gpt-6.1-sol", "medium")
            .complete_json("Sistema", "pregunta", &json!({"type":"object"}))
            .await
            .unwrap_err();
        assert_eq!(failure_of(&error), Failure::NotInstalled);
    }
}
