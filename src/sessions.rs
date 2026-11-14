//! The user's recent coding-agent sessions as bounded summaries: what the agents sources read.
//!
//! Each summary is written by code from the session's own messages (no model call): when it
//! ran, where, what the user asked, what the agent concluded and what is still open. Only the
//! plain text of user and agent messages is read; tool calls and their results (file contents,
//! command output) never are. Every line that looks like a secret is dropped and absolute paths
//! keep only their last component before a summary leaves the machine that holds the session.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{BufRead, Read, Seek},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::SystemTime,
};

/// Sessions a source reads unless it says otherwise.
pub const DEFAULT_LIMIT: usize = 10;
/// The most sessions a source may read.
pub const MAX_LIMIT: usize = 50;
/// Characters each part of a summary keeps.
const PART_CHARS: usize = 600;
/// Bytes read from the start of a session file (its first request and metadata) and from its
/// end (its latest messages); a long session is never read whole.
const HEAD_BYTES: u64 = 512 * 1024;
const TAIL_BYTES: u64 = 2 * 1024 * 1024;
/// A line longer than this (an image, a large tool result) is skipped unread.
const LINE_BYTES: usize = 256 * 1024;
/// How long an answer waits for the account's machines to list their sessions.
pub const READ_SECONDS: u64 = 30;
/// A session whose own requests carry this word is never summarized.
pub const PRIVATE_MARK: &str = "#private";

/// A PTA run that started a session, as the coordinator recorded it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Started {
    pub run: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub work_item_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub work_item_title: String,
    pub state: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    /// `claude`, `codex` or `opencode`.
    pub provider: String,
    /// The agent's own session ID.
    pub session_id: String,
    /// The machine's name, filled in by whoever gathered it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub machine: String,
    /// The last component of its working directory.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project: String,
    /// RFC 3339, UTC.
    pub started_at: String,
    pub updated_at: String,
    /// The first request of the user.
    pub asked: String,
    /// The user's latest request, when it is another one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub latest: String,
    /// The agent's latest answer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub concluded: String,
    /// What its latest answer leaves pending, or the request it has not answered yet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub open: String,
    /// The PTA run that started it (`implementation_sessions`, `implementation_history`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<Started>,
}

/// A machine (or one agent on it) whose sessions could not be read: never the same as none.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Unavailable {
    pub machine: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// `offline`, `outdated`, `no_answer` or `unreadable`.
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Listing {
    pub sessions: Vec<Summary>,
    #[serde(default)]
    pub unavailable: Vec<Unavailable>,
}
impl Listing {
    /// Join listings from several machines: the `limit` most recent, newest first.
    pub fn merge(parts: impl IntoIterator<Item = Listing>, limit: usize) -> Self {
        let mut all = Self::default();
        for part in parts {
            all.sessions.extend(part.sessions);
            all.unavailable.extend(part.unavailable);
        }
        all.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let mut seen = std::collections::BTreeSet::new();
        all.sessions
            .retain(|s| seen.insert((s.provider.clone(), s.session_id.clone())));
        all.sessions.truncate(limit.clamp(1, MAX_LIMIT));
        all
    }
    /// Mark the sessions PTA started with their run, from the runs it recorded.
    pub fn attribute(&mut self, runs: &[(String, Started)]) {
        for session in &mut self.sessions {
            if let Some((_, started)) = runs.iter().find(|(id, _)| id == &session.session_id) {
                session.started_by = Some(started.clone());
            }
        }
    }
}

/// Where this computer's coding CLIs keep their sessions.
#[derive(Clone, Debug, Default)]
pub struct Roots {
    /// Claude Code's configuration directory (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
    pub claude: Option<PathBuf>,
    /// Codex's home (`CODEX_HOME`, else `~/.codex`).
    pub codex: Option<PathBuf>,
    /// OpenCode's data directory (`$XDG_DATA_HOME/opencode`, else `~/.local/share/opencode`).
    pub opencode: Option<PathBuf>,
}
impl Roots {
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        let var = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        Self {
            claude: var("CLAUDE_CONFIG_DIR").or_else(|| home.as_ref().map(|h| h.join(".claude"))),
            codex: var("CODEX_HOME").or_else(|| home.as_ref().map(|h| h.join(".codex"))),
            opencode: var("XDG_DATA_HOME")
                .map(|d| d.join("opencode"))
                .or_else(|| home.as_ref().map(|h| h.join(".local/share/opencode"))),
        }
    }
}

/// The `limit` most recent sessions of every CLI under `roots`, newest first. A CLI whose
/// store does not exist adds nothing; one whose store cannot be read is reported unavailable.
pub fn local(roots: &Roots, limit: usize, machine: &str) -> Listing {
    let limit = limit.clamp(1, MAX_LIMIT);
    let scrubber = Scrubber::new();
    let mut parts = Vec::new();
    let mut unavailable = Vec::new();
    let mut read = |provider: &str, result: Result<Vec<Summary>>| match result {
        Ok(found) => parts.push(Listing {
            sessions: found,
            unavailable: Vec::new(),
        }),
        Err(_) => unavailable.push(Unavailable {
            machine: machine.into(),
            provider: provider.into(),
            reason: "unreadable".into(),
        }),
    };
    if let Some(root) = roots.claude.as_ref().map(|r| r.join("projects"))
        && root.is_dir()
    {
        read("claude", claude(&root, limit, &scrubber));
    }
    if let Some(root) = roots.codex.as_ref().map(|r| r.join("sessions"))
        && root.is_dir()
    {
        read("codex", codex(&root, limit, &scrubber));
    }
    if let Some(root) = roots.opencode.as_ref().filter(|r| r.is_dir()) {
        read("opencode", opencode(root, limit, &scrubber));
    }
    let mut listing = Listing::merge(parts, limit);
    for session in &mut listing.sessions {
        session.machine = machine.into();
    }
    listing.unavailable = unavailable;
    listing
}

/// Reads the account's recent sessions. The host installs one that asks every machine of the
/// account through the machine channel; without it only this computer's are read.
#[async_trait]
pub trait Reader: Send + Sync {
    async fn recent(&self, limit: usize) -> Result<Listing>;
}
struct LocalReader;
#[async_trait]
impl Reader for LocalReader {
    async fn recent(&self, limit: usize) -> Result<Listing> {
        Ok(tokio::task::spawn_blocking(move || local(&Roots::from_env(), limit, "")).await?)
    }
}
static READER: RwLock<Option<Arc<dyn Reader>>> = RwLock::new(None);
pub fn install(reader: Option<Arc<dyn Reader>>) {
    *READER.write().unwrap_or_else(|e| e.into_inner()) = reader;
}
pub fn reader() -> Arc<dyn Reader> {
    READER
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| Arc::new(LocalReader))
}

/// The summaries as evidence for an answer, each under its closed key (`s1`, `s2`…), with no
/// directory, file or command beyond the project's name.
pub fn evidence(sessions: &[(String, &Summary)]) -> String {
    let mut text = String::new();
    for (key, s) in sessions {
        let agent = match s.provider.as_str() {
            "claude" => "Claude Code",
            "codex" => "Codex",
            "opencode" => "OpenCode",
            other => other,
        };
        text.push_str(&format!(
            "### Working session {key} · {agent} · started {} · last activity {}{}{}\n",
            day_time(&s.started_at),
            day_time(&s.updated_at),
            if s.machine.is_empty() {
                String::new()
            } else {
                format!(" · machine {}", s.machine)
            },
            if s.project.is_empty() {
                String::new()
            } else {
                format!(" · project {}", s.project)
            },
        ));
        for (label, part) in [
            ("Asked", &s.asked),
            ("Latest request", &s.latest),
            ("Concluded so far", &s.concluded),
            ("Still open", &s.open),
        ] {
            if !part.is_empty() {
                text.push_str(&format!("{label}: {part}\n"));
            }
        }
        if let Some(run) = &s.started_by
            && !run.work_item_url.is_empty()
        {
            text.push_str(&format!(
                "Started by the assistant for the work item «{}» ({}), state {}\n",
                run.work_item_title, run.work_item_url, run.state
            ));
        }
        text.push('\n');
    }
    text
}
/// A line of evidence saying whose sessions could not be read, so none is never assumed.
pub fn unavailable_note(gap: &Unavailable) -> String {
    let whose = match (gap.machine.is_empty(), gap.provider.is_empty()) {
        (false, false) => format!("{} on machine {}", gap.provider, gap.machine),
        (false, true) => format!("machine {}", gap.machine),
        (true, false) => gap.provider.clone(),
        (true, true) => "a machine".into(),
    };
    format!(
        "The sessions of {whose} could not be read ({}); they are unknown, not absent.\n",
        gap.reason
    )
}
fn day_time(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| {
            t.with_timezone(&chrono::Utc)
                .format("%Y-%m-%d %H:%M UTC")
                .to_string()
        })
        .unwrap_or_default()
}

/// What a session's own text keeps before it leaves the machine.
pub struct Scrubber {
    redactor: crate::security::Redactor,
    secret: Vec<regex::Regex>,
    opaque: regex::Regex,
    path: regex::Regex,
    fence: regex::Regex,
    tags: regex::Regex,
}
impl Default for Scrubber {
    fn default() -> Self {
        Self::new()
    }
}
impl Scrubber {
    pub fn new() -> Self {
        let secret = [
            // Credentials inside any URL, not only HTTP ones.
            r"(?i)\b[a-z][a-z0-9+.-]*://[^\s/@:]+:[^\s/@]+@",
            // Connection strings and key–value credentials.
            r"(?i)\b(?:password|passwd|pwd|user\s*id|uid|account\s*key|accountkey|shared\s*access\s*key|sharedaccesskey|sharedaccesssignature|sig|secret|client[_-]?secret|api[_-]?key|apikey|token|access[_-]?key|private[_-]?key|credential|credentials|auth)\s*[:=]",
            r"(?i)\b(?:server|data source|host)\s*=[^;\n]*;",
            // Environment values named like secrets: `export DB_PASSWORD=…`, `GH_TOKEN: …`.
            r"\b[A-Z][A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|PASS|PWD|CREDENTIALS?|SAS)[A-Z0-9_]*\s*[:=]",
            r"-----(?:BEGIN|END) [A-Z ]*-----",
            r"\b(?:AKIA|ASIA)[A-Z0-9]{12,}\b",
            r"\bxox[abprs]-[A-Za-z0-9-]{8,}",
        ]
        .map(|p| regex::Regex::new(p).expect("valid secret pattern"))
        .to_vec();
        Self {
            redactor: crate::security::Redactor::new(&[], Vec::new())
                .expect("built-in redaction patterns"),
            secret,
            opaque: regex::Regex::new(r"[A-Za-z0-9+/_\-]{32,}").expect("valid opaque pattern"),
            // Only where a path starts a word: a URL's own path is not a directory.
            path: regex::Regex::new(
                r#"(^|[\s"'`(=\[])(?:[A-Za-z]:[\\/]|\\\\|~[\\/]|/)(?:[^\s\\/"'`<>|:*?]+[\\/])+([^\s\\/"'`<>|:*?]*)"#,
            )
            .expect("valid path pattern"),
            fence: regex::Regex::new(r"(?s)```.*?(?:```|\z)|~~~.*?(?:~~~|\z)")
                .expect("valid fence pattern"),
            tags: regex::Regex::new(
                r"(?s)<(system-reminder|command-[a-z]+|local-command-[a-z]+|environment_context|user_instructions|ide_[a-z_]+)>.*?(?:</[a-z_-]+>|\z)",
            )
            .expect("valid tag pattern"),
        }
    }
    /// Whether a line looks like it carries a secret.
    pub fn secret(&self, line: &str) -> bool {
        !self.redactor.clean(line)
            || self.secret.iter().any(|p| p.is_match(line))
            // A long run of letters and digits (a token, key, signature or hash), not a long
            // word or a path.
            || self.opaque.find_iter(line).any(|m| {
                let run = m.as_str();
                run.chars().any(|c| c.is_ascii_digit())
                    && run.chars().any(|c| c.is_ascii_alphabetic())
                    && run.matches('/').count() <= 2
            })
    }
    /// The text without code blocks, tags the CLIs inject, secret-looking lines or directory
    /// names, on one line and bounded.
    pub fn scrub(&self, text: &str) -> String {
        let text = self.fence.replace_all(text, " ");
        let text = self.tags.replace_all(&text, " ");
        let mut kept = Vec::new();
        let mut in_key = false;
        for line in text.lines() {
            if line.contains("-----BEGIN") {
                in_key = true;
            }
            let skip = in_key || self.secret(line);
            if line.contains("-----END") {
                in_key = false;
            }
            if skip {
                continue;
            }
            let line = self.path.replace_all(line, "${1}${2}");
            let line = line.trim().trim_start_matches(['#', '>', '-', '*']).trim();
            if !line.is_empty() {
                kept.push(line.to_owned());
            }
        }
        bounded(&kept.join(" "), PART_CHARS)
    }
}
fn bounded(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let mut cut: String = text.chars().take(max).collect();
    if let Some(space) = cut.rfind(' ') {
        cut.truncate(space);
    }
    cut.push('…');
    cut
}
/// The sentences of an answer that leave something pending.
fn pending(scrubber: &Scrubber, answer: &str) -> String {
    let marker = regex::Regex::new(
        r"(?i)\b(?:todo|pending|pendiente|pendientes|next step|next steps|siguiente paso|próximo paso|falta|faltan|remaining|still|todavía|aún|blocked|bloquead[oa]s?|waiting|esperando|not yet|once|cuando termine)\b",
    )
    .expect("valid pending pattern");
    let text = scrubber.fence.replace_all(answer, " ");
    let sentences: Vec<&str> = text
        .split(['\n', '.', '!', '?'])
        .map(str::trim)
        .filter(|s| !s.is_empty() && marker.is_match(s) && !scrubber.secret(s))
        .collect();
    scrubber.scrub(&sentences.join(". "))
}

/// One message of a session, in order.
struct Message {
    user: bool,
    text: String,
    at: Option<String>,
}
#[derive(Default)]
struct Parsed {
    id: String,
    cwd: String,
    started: Option<String>,
    updated: Option<String>,
    messages: Vec<Message>,
}
impl Parsed {
    fn summary(self, provider: &str, modified: SystemTime, scrubber: &Scrubber) -> Option<Summary> {
        // A session the user marked private, or one with no request of theirs, is not listed.
        let requests: Vec<&Message> = self
            .messages
            .iter()
            .filter(|m| m.user && !m.text.trim().is_empty())
            .collect();
        if requests.is_empty()
            || self.id.is_empty()
            || self.messages.iter().any(|m| {
                m.user
                    && m.text
                        .split(|c: char| c.is_whitespace() || ",.;:()[]".contains(c))
                        .any(|w| w.eq_ignore_ascii_case(PRIVATE_MARK))
            })
        {
            return None;
        }
        let asked = scrubber.scrub(&requests[0].text);
        let last_request = requests.last().map(|m| scrubber.scrub(&m.text));
        let answer_index = self
            .messages
            .iter()
            .rposition(|m| !m.user && !m.text.trim().is_empty());
        let request_index = self.messages.iter().rposition(|m| m.user);
        let concluded = answer_index
            .map(|i| scrubber.scrub(&self.messages[i].text))
            .unwrap_or_default();
        let open = match (answer_index, request_index) {
            (Some(a), Some(r)) if r > a => last_request.clone().unwrap_or_default(),
            (None, _) => last_request.clone().unwrap_or_default(),
            (Some(a), _) => pending(scrubber, &self.messages[a].text),
        };
        let modified: chrono::DateTime<chrono::Utc> = modified.into();
        let updated = self
            .updated
            .or_else(|| self.messages.iter().rev().find_map(|m| m.at.clone()))
            .and_then(|t| normalized(&t))
            .unwrap_or_else(|| modified.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        let started = self
            .started
            .or_else(|| self.messages.iter().find_map(|m| m.at.clone()))
            .and_then(|t| normalized(&t))
            .unwrap_or_else(|| updated.clone());
        let project = self
            .cwd
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default();
        Some(Summary {
            provider: provider.into(),
            session_id: self.id.chars().take(128).collect(),
            machine: String::new(),
            project: if scrubber.secret(project) {
                String::new()
            } else {
                project.chars().take(120).collect()
            },
            started_at: started,
            updated_at: updated,
            latest: last_request.filter(|l| l != &asked).unwrap_or_default(),
            asked,
            concluded,
            open,
            started_by: None,
        })
    }
}
fn normalized(time: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(time).ok().map(|t| {
        t.with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    })
}
fn from_millis(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// The newest `count` files under `root` (to `depth` levels) that `wanted` accepts.
fn newest(
    root: &Path,
    depth: usize,
    count: usize,
    wanted: &dyn Fn(&Path) -> bool,
) -> Result<Vec<(PathBuf, SystemTime)>> {
    let mut found = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut visited = 0;
    while let Some((dir, level)) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            visited += 1;
            anyhow::ensure!(visited <= 200_000, "too many session files");
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() && level + 1 < depth {
                pending.push((entry.path(), level + 1));
            } else if kind.is_file() && wanted(&entry.path()) {
                found.push((entry.path(), entry.metadata()?.modified()?));
            }
        }
    }
    found.sort_by(|a, b| b.1.cmp(&a.1));
    found.truncate(count);
    Ok(found)
}
/// The lines of the start and the end of a JSON-lines file, each parsed, skipping long ones.
fn json_lines(path: &Path) -> Result<Vec<Value>> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut values = Vec::new();
    let mut read = |reader: &mut dyn BufRead, skip_first: bool| -> Result<()> {
        let mut first = skip_first;
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = Read::take(&mut *reader, LINE_BYTES as u64 + 1).read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            if line.last() != Some(&b'\n') && n > LINE_BYTES {
                // Too long: skip the rest of it unread.
                reader.skip_until(b'\n')?;
                first = false;
                continue;
            }
            if std::mem::take(&mut first) {
                continue;
            }
            if let Ok(value) = serde_json::from_slice::<Value>(&line) {
                values.push(value);
            }
        }
        Ok(())
    };
    if size <= HEAD_BYTES + TAIL_BYTES {
        read(&mut std::io::BufReader::new(&mut file), false)?;
    } else {
        read(
            &mut std::io::BufReader::new((&mut file).take(HEAD_BYTES)),
            false,
        )?;
        // The head's last line may be cut; parsing drops it. The tail starts mid-line.
        file.seek(std::io::SeekFrom::Start(size - TAIL_BYTES))?;
        read(&mut std::io::BufReader::new(&mut file), true)?;
    }
    Ok(values)
}
/// The plain text of a message's content: a string, or its text parts (never tool parts).
fn text_of(content: &Value, kinds: &[&str]) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| kinds.contains(&p["type"].as_str().unwrap_or_default()))
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Claude Code: one JSON-lines file per session under `projects/<directory>/`.
fn claude(root: &Path, limit: usize, scrubber: &Scrubber) -> Result<Vec<Summary>> {
    let files = newest(root, 2, limit + 10, &|p| {
        p.extension().is_some_and(|e| e == "jsonl")
    })?;
    let mut sessions = Vec::new();
    for (path, modified) in files {
        let Ok(lines) = json_lines(&path) else {
            continue;
        };
        let mut parsed = Parsed {
            id: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .into(),
            ..Default::default()
        };
        for line in &lines {
            if line["isSidechain"] == true || line["isMeta"] == true {
                continue;
            }
            if let Some(cwd) = line["cwd"].as_str()
                && parsed.cwd.is_empty()
            {
                parsed.cwd = cwd.into();
            }
            if let Some(id) = line["sessionId"].as_str() {
                parsed.id = id.into();
            }
            let user = match line["type"].as_str() {
                Some("user") => true,
                Some("assistant") => false,
                _ => continue,
            };
            let text = text_of(&line["message"]["content"], &["text"]);
            if text.trim().is_empty() {
                continue;
            }
            parsed.messages.push(Message {
                user,
                text,
                at: line["timestamp"].as_str().map(str::to_owned),
            });
        }
        if let Some(summary) = parsed.summary("claude", modified, scrubber) {
            sessions.push(summary);
        }
        if sessions.len() == limit {
            break;
        }
    }
    Ok(sessions)
}

/// Codex: `sessions/YYYY/MM/DD/rollout-*.jsonl`, one file per session.
fn codex(root: &Path, limit: usize, scrubber: &Scrubber) -> Result<Vec<Summary>> {
    let files = newest(root, 4, limit + 10, &|p| {
        p.extension().is_some_and(|e| e == "jsonl")
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-"))
    })?;
    let mut sessions = Vec::new();
    for (path, modified) in files {
        let Ok(lines) = json_lines(&path) else {
            continue;
        };
        let mut parsed = Parsed::default();
        let mut events = Vec::new();
        let mut items = Vec::new();
        for line in &lines {
            let (kind, payload) = match line["type"].as_str() {
                Some(kind) if line.get("payload").is_some() => (kind, &line["payload"]),
                // Older rollouts: the metadata first, then the items themselves.
                None if line.get("id").is_some() => ("session_meta", line),
                Some(_) => ("response_item", line),
                None => continue,
            };
            let at = line["timestamp"].as_str().map(str::to_owned);
            match (kind, payload["type"].as_str()) {
                ("session_meta", _) => {
                    parsed.id = payload["id"].as_str().unwrap_or_default().into();
                    parsed.cwd = payload["cwd"].as_str().unwrap_or_default().into();
                    parsed.started = payload["timestamp"].as_str().map(str::to_owned);
                }
                ("event_msg", Some(kind @ ("user_message" | "agent_message"))) => {
                    events.push(Message {
                        user: kind == "user_message",
                        text: payload["message"].as_str().unwrap_or_default().into(),
                        at,
                    });
                }
                ("response_item", Some("message")) => {
                    let user = payload["role"] == "user";
                    if !user && payload["role"] != "assistant" {
                        continue;
                    }
                    let text = text_of(&payload["content"], &["input_text", "output_text"]);
                    // Codex sends its environment and the project's instructions as user text.
                    let injected = [
                        "<environment_context>",
                        "<user_instructions>",
                        "# AGENTS.md",
                    ]
                    .iter()
                    .any(|p| text.trim_start().starts_with(p));
                    if !injected {
                        items.push(Message { user, text, at });
                    }
                }
                _ => {}
            }
        }
        parsed.messages = if events.iter().any(|m| m.user) {
            events
        } else {
            items
        };
        parsed.updated = lines
            .iter()
            .rev()
            .find_map(|l| l["timestamp"].as_str().map(str::to_owned));
        if let Some(summary) = parsed.summary("codex", modified, scrubber) {
            sessions.push(summary);
        }
        if sessions.len() == limit {
            break;
        }
    }
    Ok(sessions)
}

/// OpenCode: its database (`opencode.db`) or, in older versions, JSON files under `storage/`.
fn opencode(root: &Path, limit: usize, scrubber: &Scrubber) -> Result<Vec<Summary>> {
    let database = root.join("opencode.db");
    if database.is_file() {
        return opencode_database(&database, limit, scrubber);
    }
    let storage = root.join("storage");
    if !storage.join("session").is_dir() {
        return Ok(Vec::new());
    }
    let files = newest(&storage.join("session"), 2, limit * 3 + 10, &|p| {
        p.extension().is_some_and(|e| e == "json")
    })?;
    let read = |path: &Path| -> Option<Value> {
        let mut text = String::new();
        std::fs::File::open(path)
            .ok()?
            .take(LINE_BYTES as u64)
            .read_to_string(&mut text)
            .ok()?;
        serde_json::from_str(&text).ok()
    };
    let mut sessions = Vec::new();
    for (path, modified) in files {
        let Some(info) = read(&path) else { continue };
        // A sub-agent's session belongs to its parent's.
        if info["parentID"].is_string() {
            continue;
        }
        let id = info["id"].as_str().unwrap_or_default();
        if id.is_empty() || id.contains(['/', '\\', '.']) {
            continue;
        }
        let mut messages = Vec::new();
        let message_dir = storage.join("message").join(id);
        let mut entries: Vec<Value> = std::fs::read_dir(&message_dir)
            .into_iter()
            .flatten()
            .flatten()
            .take(2000)
            .filter_map(|e| read(&e.path()))
            .collect();
        entries.sort_by_key(|m| m["time"]["created"].as_i64().unwrap_or_default());
        for message in entries {
            let Some(message_id) = message["id"].as_str().filter(|i| !i.contains(['/', '\\']))
            else {
                continue;
            };
            let mut parts: Vec<Value> = std::fs::read_dir(storage.join("part").join(message_id))
                .into_iter()
                .flatten()
                .flatten()
                .take(500)
                .filter_map(|e| read(&e.path()))
                .collect();
            parts.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            messages.push(Message {
                user: message["role"] == "user",
                text: opencode_text(&parts),
                at: message["time"]["created"].as_i64().and_then(from_millis),
            });
        }
        let parsed = Parsed {
            id: id.into(),
            cwd: info["directory"].as_str().unwrap_or_default().into(),
            started: info["time"]["created"].as_i64().and_then(from_millis),
            updated: info["time"]["updated"].as_i64().and_then(from_millis),
            messages,
        };
        if let Some(summary) = parsed.summary("opencode", modified, scrubber) {
            sessions.push(summary);
        }
        if sessions.len() == limit {
            break;
        }
    }
    Ok(sessions)
}
/// The text parts of an OpenCode message the user or the model wrote (not synthetic ones).
fn opencode_text(parts: &[Value]) -> String {
    parts
        .iter()
        .filter(|p| p["type"] == "text" && p["synthetic"] != true)
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
fn opencode_database(path: &Path, limit: usize, scrubber: &Scrubber) -> Result<Vec<Summary>> {
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    let columns = |table: &str| -> Result<Vec<String>> {
        let mut statement = db.prepare(&format!("PRAGMA table_info({table})"))?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(names)
    };
    let has = |table: &str, wanted: &[&str]| -> Result<bool> {
        let found = columns(table)?;
        Ok(wanted.iter().all(|c| found.iter().any(|f| f == c)))
    };
    // An unknown layout is unreadable, never "no sessions".
    anyhow::ensure!(
        has(
            "session",
            &["id", "directory", "time_created", "time_updated"]
        )? && has("message", &["id", "session_id", "time_created", "data"])?
            && has("part", &["id", "message_id", "data"])?,
        "unknown OpenCode database layout"
    );
    let child = if has("session", &["parent_id"])? {
        "WHERE parent_id IS NULL"
    } else {
        ""
    };
    let mut statement = db.prepare(&format!(
        "SELECT id, directory, time_created, time_updated FROM session {child} ORDER BY time_updated DESC LIMIT ?1"
    ))?;
    let rows = statement
        .query_map([(limit * 3 + 10) as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut sessions = Vec::new();
    for (id, directory, created, updated) in rows {
        let mut messages = Vec::new();
        let mut statement = db.prepare(
            "SELECT id, time_created, data FROM message WHERE session_id = ?1 ORDER BY time_created LIMIT 2000",
        )?;
        let found = statement
            .query_map([&id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (message_id, at, data) in found {
            let data: Value = serde_json::from_str(&data).unwrap_or_default();
            let mut statement =
                db.prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY id LIMIT 500")?;
            let parts: Vec<Value> = statement
                .query_map([&message_id], |row| row.get::<_, String>(0))?
                .filter_map(|data| serde_json::from_str(&data.ok()?).ok())
                .collect();
            messages.push(Message {
                user: data["role"] == "user",
                text: opencode_text(&parts),
                at: from_millis(at),
            });
        }
        let parsed = Parsed {
            id,
            cwd: directory,
            started: from_millis(created),
            updated: from_millis(updated),
            messages,
        };
        let modified = SystemTime::UNIX_EPOCH
            + std::time::Duration::from_millis(u64::try_from(updated).unwrap_or_default());
        if let Some(summary) = parsed.summary("opencode", modified, scrubber) {
            sessions.push(summary);
        }
        if sessions.len() == limit {
            break;
        }
    }
    Ok(sessions)
}

#[cfg(test)]
mod tests;
