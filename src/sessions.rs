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
/// store does not exist adds nothing; one whose store, or any of whose sessions, cannot be
/// read is reported unavailable, never as having fewer sessions.
pub fn local(roots: &Roots, limit: usize, machine: &str) -> Listing {
    let limit = limit.clamp(1, MAX_LIMIT);
    let scrubber = Scrubber::new();
    let mut parts = Vec::new();
    let mut unavailable = Vec::new();
    let mut read = |provider: &str, result: Result<Found>| match result {
        Ok(found) => {
            if found.failed > 0 {
                unavailable.push(Unavailable {
                    machine: machine.into(),
                    provider: provider.into(),
                    reason: "unreadable".into(),
                });
            }
            parts.push(Listing {
                sessions: found.sessions,
                unavailable: Vec::new(),
            });
        }
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
/// directory, file or command beyond the project's name and no work item name or link.
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
        // The work item is not named: an answer names a work item only with its verified link,
        // and this evidence carries none.
        if let Some(run) = &s.started_by {
            text.push_str(&format!(
                "Started by the assistant to work on one of the user's work items; run state {}\n",
                run.state
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

/// The tags the CLIs wrap around text that is not the user's own (reminders, slash commands,
/// shell input and output, task notifications, hooks, environment and instructions), removed
/// with everything inside them up to the closing tag of the same name.
const INJECTED: &[&str] = &[
    "system-reminder",
    "command-name",
    "command-message",
    "command-args",
    "command-contents",
    "local-command-stdout",
    "local-command-stderr",
    "local-command-caveat",
    "bash-input",
    "bash-stdout",
    "bash-stderr",
    "task-notification",
    "user-prompt-submit-hook",
    "environment_context",
    "user_instructions",
];

/// What a session's own text keeps before it leaves the machine.
pub struct Scrubber {
    redactor: crate::security::Redactor,
    secret: Vec<regex::Regex>,
    opaque: regex::Regex,
    url: regex::Regex,
    quoted_path: regex::Regex,
    home_path: regex::Regex,
    path: regex::Regex,
    pending: regex::Regex,
    /// A key that leaves its value for the next line: `Password:`, `"secret" =`.
    bare_key: regex::Regex,
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
            // A key whose name says it is secret, quoted or not, before its value:
            // `DB_PASSWORD=…`, `"db_password": …`, `secret: …`, `AccountKey=…`, `apiKey:`.
            // `PGPASSWORD=…` too: the secret word may end a longer name.
            r#"(?i)(?:password|passwd|passphrase|pwd|secret|token|api_?key|apikey|accountkey|account_key|access_?key|private_?key|sharedaccesskey|sharedaccesssignature|client_?secret|auth|authorization|credentials?|sas|signature|connection_?string|contraseña|contrasena|clave|credencial(?:es)?)(?:[_.-][a-z0-9]+)*["']?\s*[:=]"#,
            // Short key names: `pass: …`, `key: …`.
            r#"(?i)(?:^|[^a-z0-9])(?:pass|key)["']?\s*[:=]"#,
            // Connection strings.
            r"(?i)\b(?:server|data source|host|endpoint)\s*=[^;\n]*;",
            // Flags that take one: `--password X`, `-p:…`, `--api-key X`.
            r"(?i)(?:^|\s)--?(?:password|passwd|pass|pwd|token|secret|api-?key|key|auth|client-secret|access-key|sas)\b",
            // A password glued to its flag (`mysql -pHunter2`) or a user with one (`curl -u a:b`).
            r"(?:^|\s)-p[^\s-]\S*",
            r"(?i)(?:^|\s)(?:-u|--user)[\s=]+\S+:\S+",
            // Prose about one, in English or Spanish: `the password is …`, `la clave: …`.
            r"(?i)\b(?:passwords?|passwd|passphrases?|contraseñas?|contrasenas?|claves?|secrets?|secretos?|tokens?|credentials?|credenciales?|api ?keys?|private ?keys?|llaves? privadas?|access ?keys?)\b",
            r"-----(?:BEGIN|END)[A-Z ]*-----",
            r"\b(?:AKIA|ASIA)[A-Z0-9]{12,}\b",
            r"\bxox[abprs]-[A-Za-z0-9-]{8,}",
        ]
        .map(|p| regex::Regex::new(p).expect("valid secret pattern"))
        .to_vec();
        Self {
            redactor: crate::security::Redactor::new(&[], Vec::new())
                .expect("built-in redaction patterns"),
            secret,
            opaque: regex::Regex::new(r"[A-Za-z0-9+/_=\-]{32,}").expect("valid opaque pattern"),
            // Paths are rewritten outside URLs only, so a URL's own path stays whole.
            url: regex::Regex::new(r#"(?i)\b(?:https?|ftps?|wss?|ssh|git|svn|s3|gs)://[^\s"'<>]*"#)
                .expect("valid URL pattern"),
            // A quoted absolute path, spaces and all.
            quoted_path: regex::Regex::new(
                r#"(?P<open>["'`“‘])(?P<path>(?:[A-Za-z]:[\\/]|\\\\|~[\\/]|/)[^"'`“”‘’\n]*)(?P<close>["'`”’])"#,
            )
            .expect("valid quoted path pattern"),
            // A path from a drive, a share, a home or a usual system root, wherever it starts
            // (`at/home/…`, `-I/home/…`, `**/home/…**`); a directory name holds a space only
            // before a capitalized word (`First Last`, `Application Support`).
            home_path: regex::Regex::new(
                r#"(?:(?P<pre>^|[^A-Za-z0-9])[A-Za-z]:[\\/]|\\\\[^\s\\/]+[\\/]|~[\\/]|/(?:home|Users|root|var|opt|srv|tmp|etc|mnt|media|private|Volumes|usr|Library|workspaces?|data|run|nix|snap|app|code)[\\/])(?:[^\s\\/"'`“”‘’<>|:*?]+(?: [A-Z][^\s\\/"'`“”‘’<>|:*?]*)*[\\/])*(?P<last>[^\s\\/"'`“”‘’<>|:*?]*)"#,
            )
            .expect("valid home path pattern"),
            // Any other absolute path, where a word does not run into it (not `src/a/b`).
            path: regex::Regex::new(
                r#"(?P<pre>^|[^A-Za-z0-9_.\-/\\~])/(?:[^\s\\/"'`“”‘’<>|:*?]+(?: [A-Z][^\s\\/"'`“”‘’<>|:*?]*)*[\\/])+(?P<last>[^\s\\/"'`“”‘’<>|:*?]*)"#,
            )
            .expect("valid path pattern"),
            bare_key: regex::Regex::new(
                r#"(?i)\b(?:password|passwd|passphrase|pwd|pass|secret|token|key|auth|authorization|credentials?|sas|signature|string|contraseña|contrasena|clave|credencial(?:es)?|is|are|was|es|son|era)\b["'*_`\s]*[:=][\s"'*_`]*$"#,
            )
            .expect("valid key pattern"),
            pending: regex::Regex::new(
                r"(?i)\b(?:todo|pending|pendiente|pendientes|next step|next steps|siguiente paso|próximo paso|falta|faltan|remaining|still|todavía|aún|blocked|bloquead[oa]s?|waiting|esperando|not yet|once|cuando termine)\b",
            )
            .expect("valid pending pattern"),
        }
    }
    /// Whether a line looks like it carries a secret, or talks about one.
    pub fn secret(&self, line: &str) -> bool {
        !self.redactor.clean(line)
            || self.secret.iter().any(|p| p.is_match(line))
            || self.opaque.find_iter(line).any(|m| opaque(m.as_str()))
    }
    /// The lines of `text` worth keeping: no fenced code, no injected blocks, no line that looks
    /// like a secret (nor the line after a key that leaves its value for it), no PEM block, and
    /// absolute paths reduced to their last component.
    fn lines(&self, text: &str) -> Vec<String> {
        let text = without_injected(text);
        let mut kept = Vec::new();
        let mut fence: Option<(char, usize)> = None;
        let mut in_key = false;
        let mut value_next = false;
        for line in text.lines() {
            // A fence may sit in a blockquote (`> ```).
            let trimmed = line.trim_start_matches(|c: char| c.is_whitespace() || c == '>');
            let run = |c: char| trimmed.chars().take_while(|&x| x == c).count();
            if let Some((c, n)) = fence {
                if run(c) >= n && trimmed.trim_start_matches(c).trim().is_empty() {
                    fence = None;
                }
                continue;
            }
            // ```` ```js``` ```` closes on its own line: inline code, not a block.
            if let Some(c) = ['`', '~'].into_iter().find(|&c| run(c) >= 3)
                && !trimmed[run(c) * c.len_utf8()..].contains(&c.to_string().repeat(run(c)))
            {
                fence = Some((c, run(c)));
                continue;
            }
            if line.contains("-----BEGIN") {
                in_key = true;
            }
            let secret = in_key || self.secret(line);
            if line.contains("-----END") {
                in_key = false;
            }
            if line.trim().is_empty() {
                continue;
            }
            if std::mem::take(&mut value_next) || secret {
                // `Password:` alone leaves its value on the next line.
                value_next = secret && self.bare_key.is_match(line);
                continue;
            }
            let line = self.without_paths(line);
            let line = line.trim().trim_start_matches(['#', '>', '-', '*']).trim();
            if !line.is_empty() {
                kept.push(line.to_owned());
            }
        }
        kept
    }
    /// The line with every absolute path outside a URL reduced to its last component.
    fn without_paths(&self, line: &str) -> String {
        let last = |path: &str| {
            path.trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or_default()
                .to_owned()
        };
        let rewrite = |text: &str| -> String {
            let text = self.quoted_path.replace_all(text, |c: &regex::Captures| {
                format!("{}{}{}", &c["open"], last(&c["path"]), &c["close"])
            });
            let text = self.home_path.replace_all(&text, "${pre}${last}");
            self.path.replace_all(&text, "${pre}${last}").into_owned()
        };
        let mut out = String::new();
        let mut at = 0;
        for url in self.url.find_iter(line) {
            out.push_str(&rewrite(&line[at..url.start()]));
            out.push_str(url.as_str());
            at = url.end();
        }
        out.push_str(&rewrite(&line[at..]));
        out
    }
    /// The text as one bounded line of what `lines` keeps.
    pub fn scrub(&self, text: &str) -> String {
        bounded(&self.lines(text).join(" "), PART_CHARS)
    }
    /// The sentences of an answer that leave something pending, from the lines it keeps.
    fn pending(&self, answer: &str) -> String {
        let sentences: Vec<String> = self
            .lines(answer)
            .iter()
            .flat_map(|line| line.split(['.', '!', '?']).map(str::trim))
            .filter(|s| !s.is_empty() && self.pending.is_match(s))
            .map(str::to_owned)
            .collect();
        bounded(&sentences.join(". "), PART_CHARS)
    }
}
/// A long run of letters and digits that reads as a token, key, signature or hash, not a long
/// word or a path of words.
fn opaque(run: &str) -> bool {
    let digits = run.chars().filter(char::is_ascii_digit).count();
    let upper = run.chars().filter(char::is_ascii_uppercase).count();
    let letters = run.chars().filter(char::is_ascii_alphabetic).count();
    digits > 0
        && letters > 0
        && (!run.contains('/') || run.contains(['+', '=']) || (upper >= 3 && digits >= 3))
}
/// `text` without the blocks the CLIs inject, each closed by the tag of its own name (to the end
/// when it is never closed).
fn without_injected(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let name_len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(after.len());
        let name = &after[..name_len];
        let injected = after[name_len..].starts_with(['>', ' '])
            && (INJECTED.contains(&name) || name.starts_with("ide_") || name.starts_with("ide-"));
        if !injected {
            out.push('<');
            rest = after;
            continue;
        }
        // Closed by its own name, counting the same tag nested inside it.
        let (open, close) = (format!("<{name}>"), format!("</{name}>"));
        let mut depth = 1;
        let mut at = name_len;
        rest = loop {
            let next_open = after[at..].find(&open).map(|i| i + at);
            let Some(next_close) = after[at..].find(&close).map(|i| i + at) else {
                break "";
            };
            match next_open {
                Some(o) if o < next_close => {
                    depth += 1;
                    at = o + open.len();
                }
                _ => {
                    depth -= 1;
                    at = next_close + close.len();
                    if depth == 0 {
                        break &after[at..];
                    }
                }
            }
        };
        out.push(' ');
    }
    out.push_str(rest);
    out
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

/// Whether `text` carries the private mark as a token of its own (`#private`, `#private!`,
/// `"#private"`, `**#private**`), not as part of a longer word.
pub fn marked_private(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mark = PRIVATE_MARK.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    bytes.windows(mark.len()).enumerate().any(|(i, w)| {
        w.eq_ignore_ascii_case(mark)
            && (i == 0 || !word(bytes[i - 1]))
            && bytes
                .get(i + mark.len())
                .is_none_or(|&next| !word(next) && next != b'-')
    })
}
/// Whether raw bytes contain the mark's text at all, whatever surrounds it: in JSON the byte
/// before a mark that starts a line is the `n` of `\n`, so whether it is a word of its own is
/// decided on the decoded text (`marked_private`).
fn raw_mark(bytes: &[u8]) -> bool {
    bytes
        .windows(PRIVATE_MARK.len())
        .any(|w| w.eq_ignore_ascii_case(PRIVATE_MARK.as_bytes()))
}
/// Whether the mark's text appears anywhere in the file, read in bounded chunks.
fn file_has_mark(path: &Path) -> Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let mut window: Vec<u8> = Vec::new();
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut chunk)?;
        window.extend_from_slice(&chunk[..n]);
        if raw_mark(&window) {
            return Ok(true);
        }
        if n == 0 {
            return Ok(false);
        }
        // Keep enough to see a mark that crosses chunks.
        let keep = window.len().min(PRIVATE_MARK.len() - 1);
        window.drain(..window.len() - keep);
    }
}
/// Whether the user marked a JSON-lines session private in any request, wherever it is in the
/// file. Only a file that contains the mark is parsed; a line too long to parse that contains it
/// counts as marked.
fn jsonl_private(path: &Path, request: &dyn Fn(&Value) -> Option<String>) -> Result<bool> {
    if !file_has_mark(path)? {
        return Ok(false);
    }
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = Read::take(&mut reader, LINE_BYTES as u64 + 1).read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(false);
        }
        if line.last() != Some(&b'\n') && n > LINE_BYTES {
            if long_line_has_mark(&mut reader, &line)? {
                return Ok(true);
            }
            continue;
        }
        if raw_mark(&line)
            && let Ok(value) = serde_json::from_slice::<Value>(&line)
            && request(&value).is_some_and(|text| marked_private(&text))
        {
            return Ok(true);
        }
    }
}
/// Reads the rest of a long line in chunks, looking for the mark's text (a line too long to
/// decode that contains it counts as marked).
fn long_line_has_mark(reader: &mut dyn BufRead, head: &[u8]) -> Result<bool> {
    let mut found = raw_mark(head);
    let mut carry = head[head.len().saturating_sub(PRIVATE_MARK.len())..].to_vec();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(found);
        }
        let (chunk, end) = match buffer.iter().position(|&b| b == b'\n') {
            Some(i) => (&buffer[..i], true),
            None => (buffer, false),
        };
        carry.extend_from_slice(chunk);
        found |= raw_mark(&carry);
        let used = chunk.len() + usize::from(end);
        carry.drain(..carry.len().saturating_sub(PRIVATE_MARK.len()));
        reader.consume(used);
        if end {
            return Ok(found);
        }
    }
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
/// What a reader found: the summaries, and how many sessions it could not read.
#[derive(Default)]
struct Found {
    sessions: Vec<Summary>,
    failed: usize,
}
impl Found {
    fn add(&mut self, read: Result<Option<Summary>>) {
        match read {
            Ok(Some(summary)) => self.sessions.push(summary),
            Ok(None) => {}
            Err(_) => self.failed += 1,
        }
    }
}
impl Parsed {
    /// The summary, `None` for a session with no request of the user's or one marked private
    /// in what was read, and an error when no time can be told for it.
    fn summary(
        self,
        provider: &str,
        modified: Option<SystemTime>,
        scrubber: &Scrubber,
    ) -> Result<Option<Summary>> {
        let requests: Vec<&Message> = self
            .messages
            .iter()
            .filter(|m| m.user && !m.text.trim().is_empty())
            .collect();
        if requests.is_empty()
            || self.id.is_empty()
            || requests.iter().any(|m| marked_private(&m.text))
        {
            return Ok(None);
        }
        let updated = self
            .updated
            .or_else(|| self.messages.iter().rev().find_map(|m| m.at.clone()))
            .and_then(|t| normalized(&t))
            .or_else(|| modified.and_then(system_time))
            .ok_or_else(|| anyhow::anyhow!("the session has no readable time"))?;
        let started = self
            .started
            .or_else(|| self.messages.iter().find_map(|m| m.at.clone()))
            .and_then(|t| normalized(&t))
            .unwrap_or_else(|| updated.clone());
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
            (Some(a), _) => scrubber.pending(&self.messages[a].text),
        };
        let project = self
            .cwd
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default();
        Ok(Some(Summary {
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
        }))
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
/// A file time as RFC 3339, or none when it is out of range.
fn system_time(time: SystemTime) -> Option<String> {
    let since = time.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    chrono::DateTime::from_timestamp(i64::try_from(since.as_secs()).ok()?, since.subsec_nanos())
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
    found.sort_by_key(|f| std::cmp::Reverse(f.1));
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

/// A Claude Code line as the user's own request (`true`) or the agent's answer, with its text;
/// none for side chains, meta and compaction summaries, tool results, and lines whose text is
/// only what Claude Code injects (shell input and output, commands, task notifications).
fn claude_message(line: &Value) -> Option<(bool, String)> {
    if [
        "isSidechain",
        "isMeta",
        "isCompactSummary",
        "isVisibleInTranscriptOnly",
    ]
    .iter()
    .any(|flag| line[*flag] == true)
    {
        return None;
    }
    let user = match line["type"].as_str() {
        Some("user") => true,
        Some("assistant") => false,
        _ => return None,
    };
    let text = text_of(&line["message"]["content"], &["text"]);
    let own = without_injected(&text);
    let own = own.trim();
    (!own.is_empty() && !own.starts_with("[Request interrupted")).then(|| (user, own.to_owned()))
}
/// Claude Code: one JSON-lines file per session under `projects/<directory>/`.
fn claude(root: &Path, limit: usize, scrubber: &Scrubber) -> Result<Found> {
    let files = newest(root, 2, limit * 2 + 10, &|p| {
        p.extension().is_some_and(|e| e == "jsonl")
    })?;
    let mut found = Found::default();
    for (path, modified) in files {
        if found.sessions.len() == limit {
            break;
        }
        found.add(claude_session(&path, modified, scrubber));
    }
    Ok(found)
}
fn claude_session(
    path: &Path,
    modified: SystemTime,
    scrubber: &Scrubber,
) -> Result<Option<Summary>> {
    if jsonl_private(path, &|line| {
        claude_message(line).and_then(|(user, text)| user.then_some(text))
    })? {
        return Ok(None);
    }
    let lines = json_lines(path)?;
    // The file is named after the session; a resumed one may carry older IDs inside.
    let mut parsed = Parsed {
        id: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .into(),
        ..Default::default()
    };
    for line in &lines {
        if let Some(cwd) = line["cwd"].as_str()
            && parsed.cwd.is_empty()
        {
            parsed.cwd = cwd.into();
        }
        if let Some((user, text)) = claude_message(line) {
            parsed.messages.push(Message {
                user,
                text,
                at: line["timestamp"].as_str().map(str::to_owned),
            });
        }
    }
    parsed.summary("claude", Some(modified), scrubber)
}

/// A Codex rollout line as a message: from its events (`user_message`, `agent_message`) or,
/// in older rollouts, its response items, never the environment or instructions it injects.
/// The first element says whether it is an event.
fn codex_message(line: &Value) -> Option<(bool, bool, String)> {
    let (kind, payload) = match line["type"].as_str() {
        Some(kind) if line.get("payload").is_some() => (kind, &line["payload"]),
        Some(_) => ("response_item", line),
        None => return None,
    };
    match (kind, payload["type"].as_str()) {
        ("event_msg", Some(kind @ ("user_message" | "agent_message"))) => Some((
            true,
            kind == "user_message",
            payload["message"].as_str().unwrap_or_default().into(),
        )),
        ("response_item", Some("message")) => {
            let user = payload["role"] == "user";
            if !user && payload["role"] != "assistant" {
                return None;
            }
            let text = text_of(&payload["content"], &["input_text", "output_text"]);
            let injected = [
                "<environment_context>",
                "<user_instructions>",
                "# AGENTS.md",
            ]
            .iter()
            .any(|p| text.trim_start().starts_with(p));
            (!injected).then_some((false, user, text))
        }
        _ => None,
    }
}
/// Codex: `sessions/YYYY/MM/DD/rollout-*.jsonl`, one file per session.
fn codex(root: &Path, limit: usize, scrubber: &Scrubber) -> Result<Found> {
    let files = newest(root, 4, limit * 2 + 10, &|p| {
        p.extension().is_some_and(|e| e == "jsonl")
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-"))
    })?;
    let mut found = Found::default();
    for (path, modified) in files {
        if found.sessions.len() == limit {
            break;
        }
        found.add(codex_session(&path, modified, scrubber));
    }
    Ok(found)
}
fn codex_session(
    path: &Path,
    modified: SystemTime,
    scrubber: &Scrubber,
) -> Result<Option<Summary>> {
    if jsonl_private(path, &|line| {
        codex_message(line).and_then(|(_, user, text)| user.then_some(text))
    })? {
        return Ok(None);
    }
    let lines = json_lines(path)?;
    let mut parsed = Parsed::default();
    let mut events = Vec::new();
    let mut items = Vec::new();
    for line in &lines {
        // Older rollouts start with the metadata itself, without a type.
        let meta = if line["type"] == "session_meta" {
            Some(&line["payload"])
        } else {
            (line["type"].is_null() && line.get("id").is_some()).then_some(line)
        };
        if let Some(meta) = meta {
            parsed.id = meta["id"].as_str().unwrap_or_default().into();
            parsed.cwd = meta["cwd"].as_str().unwrap_or_default().into();
            parsed.started = meta["timestamp"].as_str().map(str::to_owned);
            continue;
        }
        let at = line["timestamp"].as_str().map(str::to_owned);
        if let Some((event, user, text)) = codex_message(line) {
            let message = Message { user, text, at };
            if event {
                events.push(message);
            } else {
                items.push(message);
            }
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
    parsed.summary("codex", Some(modified), scrubber)
}

/// OpenCode: its database (`opencode.db`) or, in older versions, JSON files under `storage/`.
fn opencode(root: &Path, limit: usize, scrubber: &Scrubber) -> Result<Found> {
    let database = root.join("opencode.db");
    if database.is_file() {
        return opencode_database(&database, limit, scrubber);
    }
    let storage = root.join("storage");
    if !storage.join("session").is_dir() {
        return Ok(Found::default());
    }
    let files = newest(&storage.join("session"), 2, limit * 3 + 10, &|p| {
        p.extension().is_some_and(|e| e == "json")
    })?;
    let mut found = Found::default();
    for (path, modified) in files {
        if found.sessions.len() == limit {
            break;
        }
        found.add(opencode_file_session(&storage, &path, modified, scrubber));
    }
    Ok(found)
}
/// A small JSON file; a larger one is not read.
fn read_json(path: &Path) -> Result<Value> {
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= LINE_BYTES as u64,
        "file too large"
    );
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
/// The files of a directory sorted by name (OpenCode's IDs sort by time); none when it does not
/// exist, an error past `max`.
fn sorted_files(dir: &Path, max: usize) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut files = Vec::new();
    for entry in entries {
        files.push(entry?.path());
        anyhow::ensure!(files.len() <= max, "too many files");
    }
    files.sort();
    Ok(files)
}
fn opencode_file_session(
    storage: &Path,
    path: &Path,
    modified: SystemTime,
    scrubber: &Scrubber,
) -> Result<Option<Summary>> {
    let info = read_json(path)?;
    // A sub-agent's session belongs to its parent's.
    if info["parentID"].is_string() {
        return Ok(None);
    }
    let id = info["id"].as_str().unwrap_or_default();
    anyhow::ensure!(
        !id.is_empty() && !id.contains(['/', '\\', '.']),
        "invalid session ID"
    );
    let mut messages = Vec::new();
    for file in sorted_files(&storage.join("message").join(id), 20_000)? {
        let message = read_json(&file)?;
        let message_id = message["id"].as_str().unwrap_or_default().to_owned();
        anyhow::ensure!(
            !message_id.is_empty() && !message_id.contains(['/', '\\', '.']),
            "invalid message ID"
        );
        messages.push((message_id, message));
    }
    messages.sort_by_key(|(_, m)| m["time"]["created"].as_i64().unwrap_or_default());
    // Every request of the user, wherever it is, may mark the session private.
    for (message_id, message) in &messages {
        if message["role"] == "user" {
            for part in sorted_files(&storage.join("part").join(message_id), 2_000)? {
                if file_has_mark(&part)?
                    && read_json(&part).map_or(true, |p| marked_private(&opencode_text(&[p])))
                {
                    return Ok(None);
                }
            }
        }
    }
    let mut read = Vec::new();
    for (message_id, message) in first_and_last(messages) {
        let mut parts = Vec::new();
        for part in sorted_files(&storage.join("part").join(&message_id), 2_000)?
            .into_iter()
            .take(200)
        {
            // A large part is a tool's output, never text worth a summary; any other part that
            // cannot be read makes the session unreadable.
            if std::fs::metadata(&part)?.len() > LINE_BYTES as u64 {
                continue;
            }
            parts.push(read_json(&part)?);
        }
        read.push(Message {
            user: message["role"] == "user",
            text: opencode_text(&parts),
            at: message["time"]["created"].as_i64().and_then(from_millis),
        });
    }
    Parsed {
        id: id.into(),
        cwd: info["directory"].as_str().unwrap_or_default().into(),
        started: info["time"]["created"].as_i64().and_then(from_millis),
        updated: info["time"]["updated"].as_i64().and_then(from_millis),
        messages: read,
    }
    .summary("opencode", Some(modified), scrubber)
}
/// The messages a summary reads: the first ones (the request) and the latest ones.
fn first_and_last<T>(mut messages: Vec<T>) -> Vec<T> {
    if messages.len() > FIRST_MESSAGES + LAST_MESSAGES {
        messages.drain(FIRST_MESSAGES..messages.len() - LAST_MESSAGES);
    }
    messages
}
const FIRST_MESSAGES: usize = 10;
const LAST_MESSAGES: usize = 60;
/// The text parts of an OpenCode message the user or the model wrote (not synthetic ones).
fn opencode_text(parts: &[Value]) -> String {
    parts
        .iter()
        .filter(|p| p["type"] == "text" && p["synthetic"] != true)
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
fn opencode_database(path: &Path, limit: usize, scrubber: &Scrubber) -> Result<Found> {
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
    let mut found = Found::default();
    for (id, directory, created, updated) in rows {
        if found.sessions.len() == limit {
            break;
        }
        found.add(opencode_row(&db, id, directory, created, updated, scrubber));
    }
    Ok(found)
}
fn opencode_row(
    db: &rusqlite::Connection,
    id: String,
    directory: String,
    created: i64,
    updated: i64,
    scrubber: &Scrubber,
) -> Result<Option<Summary>> {
    // Every request of the user, wherever it is, may mark the session private.
    // Only the user's text parts that contain the mark's text; one too long to decode counts.
    let mut statement = db.prepare(
        "SELECT CASE WHEN length(p.data) <= ?2 THEN p.data END FROM part p \
         JOIN message m ON m.id = p.message_id \
         WHERE m.session_id = ?1 AND json_extract(m.data, '$.role') = 'user' \
         AND json_extract(p.data, '$.type') = 'text' AND instr(lower(p.data), '#private') > 0",
    )?;
    let marked = statement
        .query_map(rusqlite::params![&id, LINE_BYTES as i64], |row| {
            row.get::<_, Option<String>>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for part in marked {
        let Some(part) = part else {
            return Ok(None);
        };
        if marked_private(&opencode_text(&[serde_json::from_str(&part)?])) {
            return Ok(None);
        }
    }
    // The first messages (the request) and the latest ones, in order.
    let read = |order: &str, count: usize| -> Result<Vec<(String, i64, String)>> {
        let mut statement = db.prepare(&format!(
            "SELECT id, time_created, data FROM message WHERE session_id = ?1 \
             ORDER BY time_created {order}, id {order} LIMIT ?2"
        ))?;
        let rows = statement
            .query_map(rusqlite::params![&id, count as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    };
    let mut rows = read("ASC", FIRST_MESSAGES)?;
    let mut latest = read("DESC", LAST_MESSAGES)?;
    latest.reverse();
    for row in latest {
        if !rows.iter().any(|(seen, _, _)| seen == &row.0) {
            rows.push(row);
        }
    }
    let mut parts = db.prepare(
        "SELECT data FROM part WHERE message_id = ?1 AND length(data) <= ?2 \
         AND json_extract(data, '$.type') = 'text' \
         AND IFNULL(json_extract(data, '$.synthetic'), 0) = 0 ORDER BY id LIMIT 50",
    )?;
    let mut messages = Vec::new();
    for (message_id, at, data) in rows {
        let data: Value = serde_json::from_str(&data)?;
        let texts = parts
            .query_map(rusqlite::params![&message_id, LINE_BYTES as i64], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .iter()
            .map(|part| serde_json::from_str::<Value>(part))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        messages.push(Message {
            user: data["role"] == "user",
            text: opencode_text(&texts),
            at: from_millis(at),
        });
    }
    let modified = u64::try_from(updated)
        .ok()
        .and_then(|ms| SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(ms)));
    Parsed {
        id,
        cwd: directory,
        started: from_millis(created),
        updated: from_millis(updated),
        messages,
    }
    .summary("opencode", modified, scrubber)
}

#[cfg(test)]
mod tests;
