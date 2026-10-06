use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeMap {
    #[serde(default)]
    pub repositories: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub resources: Vec<Resource>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Resource {
    pub id: String,
    pub description: String,
    pub topics: Vec<String>,
    pub enabled: bool,
    pub external_processing: bool,
    pub allowed_conversations: Vec<String>,
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    #[serde(flatten)]
    pub access: Access,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Access {
    File { repository: String, path: PathBuf },
    Url { url: String },
    Tool { tool: crate::tools::ToolSpec },
}
impl KnowledgeMap {
    pub fn load(path: &Path) -> Result<Self> {
        Self::parse(&std::fs::read_to_string(path)?)
    }
    pub fn parse(text: &str) -> Result<Self> {
        let map: Self = toml::from_str(text)?;
        ensure!(map.resources.len() <= 250, "too many resources");
        let mut ids = BTreeSet::new();
        let id_pattern = regex::Regex::new(r"^[a-z][a-z0-9_-]{0,63}$")?;
        for r in &map.resources {
            ensure!(
                id_pattern.is_match(&r.id)
                    && !["allow", "ignore", "human"].contains(&r.id.as_str())
                    && ids.insert(&r.id),
                "invalid or duplicate resource id"
            );
            ensure!(
                r.description.len() <= 2000 && r.topics.len() <= 30,
                "resource descriptor too large"
            );
            match &r.access {
                Access::File { repository, path } => {
                    ensure!(
                        map.repositories.contains_key(repository),
                        "unknown repository"
                    );
                    ensure!(
                        !path.is_absolute()
                            && path
                                .components()
                                .all(|c| matches!(c, std::path::Component::Normal(_))),
                        "invalid knowledge path"
                    );
                }
                Access::Url { url } => {
                    validate_url(url)?;
                }
                Access::Tool { tool } => {
                    tool.validate()?;
                    if let crate::tools::ToolSpec::AzureDevopsWiki {
                        repository, path, ..
                    }
                    | crate::tools::ToolSpec::AzureDevopsStatus {
                        repository, path, ..
                    } = tool
                    {
                        ensure!(
                            map.repositories.contains_key(repository)
                                && !path.is_absolute()
                                && path
                                    .components()
                                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                            "invalid tool repository or path"
                        );
                    }
                }
            }
        }
        Ok(map)
    }
    pub fn available<'a>(&'a self, conversation: &str, sender: &str) -> Vec<&'a Resource> {
        self.resources
            .iter()
            .filter(|r| {
                r.enabled
                    && r.external_processing
                    && r.allowed_conversations
                        .iter()
                        .any(|c| c == conversation || c == "*")
                    && (r.allowed_senders.is_empty()
                        || r.allowed_senders.iter().any(|s| s == sender))
            })
            .collect()
    }
    pub async fn retrieve(
        &self,
        resource: &Resource,
        _question: &str,
        _limit: usize,
    ) -> Result<String> {
        let text = match &resource.access {
            Access::File { repository, path } => {
                read_repository_file(&self.repositories, repository, path)?
            }
            Access::Url { url } => {
                let client = public_client(url).await?;
                let r = client.get(url).send().await?;
                ensure!(r.status().is_success(), "knowledge HTTP read failed");
                let content_type = r
                    .headers()
                    .get("content-type")
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                ensure!(
                    content_type.starts_with("text/") || content_type.contains("json"),
                    "unsupported web content"
                );
                let raw = String::from_utf8(crate::adapters::bounded_bytes(r, 1_000_000).await?)?;
                if content_type.contains("html") {
                    crate::adapters::teams::plain_text(&raw)
                } else {
                    raw
                }
            }
            Access::Tool { .. } => anyhow::bail!("tool resources require the tool registry"),
        };
        Ok(text)
    }
}
pub fn read_repository_file(
    repositories: &BTreeMap<String, PathBuf>,
    repository: &str,
    path: &Path,
) -> Result<String> {
    ensure!(
        !path.is_absolute()
            && path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "invalid knowledge path"
    );
    let root = std::fs::canonicalize(repositories.get(repository).context("unknown repository")?)?;
    ensure!(
        root.join(".git").exists(),
        "knowledge root must be a Git checkout"
    );
    let full = std::fs::canonicalize(root.join(path))?;
    ensure!(full.starts_with(&root), "knowledge path escapes repository");
    let ext = full.extension().and_then(|e| e.to_str()).unwrap_or("");
    ensure!(
        ["md", "txt", "json", "toml", "yaml", "yml"].contains(&ext),
        "unsupported knowledge format"
    );
    ensure!(
        std::fs::metadata(&full)?.len() <= 1_000_000,
        "knowledge file too large"
    );
    Ok(std::fs::read_to_string(full)?)
}
/// The parts of `text` most relevant to `question`, in document order and within `limit`
/// characters; a text that fits is returned whole. The pipeline shares its total budget
/// between authorized sources.
///
/// Parts are Markdown sections (a section longer than `limit`, its paragraphs), never split
/// inside a fenced code block, so a parameter table or a request example stays whole. A
/// question word weighs more the fewer parts contain it, so the words of a page's own topic,
/// found everywhere in it, barely count; it counts again when its heading or a parent heading
/// has it. The most relevant part always goes (cut if it alone exceeds the budget), the next
/// ones go whole while they fit, and the room left goes to the best one that did not, cut.
pub fn excerpt(text: &str, question: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let words: BTreeSet<String> = question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 2)
        .map(str::to_lowercase)
        .collect();
    let parts = parts(text, limit);
    let lower: Vec<String> = parts.iter().map(|(_, p)| p.to_lowercase()).collect();
    let weights: Vec<(&str, f64)> = words
        .iter()
        .map(|w| {
            let found = lower.iter().filter(|p| p.contains(w.as_str())).count();
            let weight = if found == 0 {
                0.0
            } else {
                (parts.len() as f64 / found as f64).ln()
            };
            (w.as_str(), weight)
        })
        .collect();
    let mut ranked: Vec<(f64, usize)> = parts
        .iter()
        .zip(&lower)
        .enumerate()
        .map(|(i, ((headings, _), text))| {
            let score = weights
                .iter()
                .map(|(w, weight)| {
                    weight * f64::from(u8::from(text.contains(w)) + u8::from(headings.contains(w)))
                })
                .sum();
            (score, i)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let marker = |i: usize| format!("[passage {}]\n", i + 1);
    let mut left = limit;
    let mut chosen = Vec::new();
    let mut too_long = Vec::new();
    let cut = |i: usize, left: usize| {
        let room = left.saturating_sub(marker(i).chars().count() + 1);
        let part: String = parts[i].1.chars().take(room).collect();
        (i, format!("{}{part}\n", marker(i)))
    };
    for (rank, (_, i)) in ranked.into_iter().enumerate() {
        let size = marker(i).chars().count() + parts[i].1.chars().count() + 1;
        if size <= left {
            left -= size;
            chosen.push((i, format!("{}{}\n", marker(i), parts[i].1)));
        } else if rank == 0 {
            // The most relevant part always goes, cut to the whole budget if it must.
            chosen.push(cut(i, left));
            left = 0;
        } else {
            too_long.push(i);
        }
    }
    // The room left after the parts that fit whole goes to the best one that did not, cut,
    // when that room still says something.
    if let Some(&i) = too_long.first()
        && left >= 200 + marker(i).chars().count()
    {
        chosen.push(cut(i, left));
    }
    chosen.sort();
    chosen.into_iter().map(|(_, part)| part).collect()
}
/// `text` split at Markdown headings outside fenced code, each part with its lowercase heading
/// path; a section longer than `limit` splits again at blank lines outside fences.
fn parts(text: &str, limit: usize) -> Vec<(String, String)> {
    let fence = |line: &str| {
        let line = line.trim_start();
        line.starts_with("```") || line.starts_with("~~~")
    };
    let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut code = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let level = trimmed.len() - trimmed.trim_start_matches('#').len();
        if fence(line) {
            code = !code;
        } else if !code && (1..=6).contains(&level) && trimmed[level..].starts_with(' ') {
            path.retain(|(l, _)| *l < level);
            path.push((level, trimmed[level..].trim().to_lowercase()));
            let headings: Vec<&str> = path.iter().map(|(_, h)| h.as_str()).collect();
            sections.push((headings.join(" / "), Vec::new()));
        }
        if sections.is_empty() {
            sections.push((String::new(), Vec::new()));
        }
        sections.last_mut().unwrap().1.push(line);
    }
    let mut parts = Vec::new();
    for (headings, lines) in sections {
        let section = lines.join("\n");
        if section.chars().count() <= limit {
            parts.push((headings, section));
            continue;
        }
        let mut code = false;
        let mut paragraph: Vec<&str> = Vec::new();
        for line in lines {
            code ^= fence(line);
            if !code && line.trim().is_empty() {
                if !paragraph.is_empty() {
                    parts.push((headings.clone(), paragraph.join("\n")));
                    paragraph.clear();
                }
                continue;
            }
            paragraph.push(line);
        }
        if !paragraph.is_empty() {
            parts.push((headings, paragraph.join("\n")));
        }
    }
    parts
}
pub fn validate_url(value: &str) -> Result<url::Url> {
    let u = url::Url::parse(value)?;
    ensure!(
        u.scheme() == "https"
            && u.username().is_empty()
            && u.password().is_none()
            && u.fragment().is_none()
            && u.host_str().is_some(),
        "resource URL must be HTTPS without credentials"
    );
    ensure!(
        u.port_or_known_default() == Some(443),
        "only HTTPS port 443 is allowed"
    );
    Ok(u)
}
fn public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => {
            let o = v.octets();
            !v.is_private()
                && !v.is_loopback()
                && !v.is_link_local()
                && !v.is_broadcast()
                && !v.is_documentation()
                && !v.is_unspecified()
                && !v.is_multicast()
                && o[0] != 0
                && o[0] < 224
                && !(o[0] == 100 && (64..=127).contains(&o[1]))
                && !(o[0] == 198 && (18..=19).contains(&o[1]))
        }
        std::net::IpAddr::V6(v) => v
            .to_ipv4_mapped()
            .map(|v| public_ip(v.into()))
            .unwrap_or_else(|| {
                let s = v.segments();
                (s[0] & 0xe000) == 0x2000 && !(s[0] == 0x2001 && (s[1] == 0xdb8 || s[1] == 0))
            }),
    }
}
pub async fn public_client(value: &str) -> Result<reqwest::Client> {
    let u = validate_url(value)?;
    let host = u.host_str().unwrap();
    let ips: Vec<_> = tokio::net::lookup_host((host, 443)).await?.collect();
    ensure!(
        !ips.is_empty() && ips.iter().all(|a| public_ip(a.ip())),
        "non-public knowledge host blocked"
    );
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .resolve_to_addrs(host, &ips)
        .build()?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_map_rejected() {
        assert!(KnowledgeMap::parse("resources=[]\nrepositories={}").is_ok());
        assert!(KnowledgeMap::parse("resources='oops'").is_err());
    }
    #[test]
    fn blocks_ssrf_urls() {
        for s in [
            "http://localhost/x",
            "file:///tmp/x",
            "https://user:pass@host/x",
            "https://x:8443/",
        ] {
            assert!(validate_url(s).is_err());
        }
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "::1",
            "::ffff:127.0.0.1",
            "100.64.0.1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()));
        }
    }
    #[test]
    fn excerpts_are_bounded_and_relevant() {
        let result = excerpt(
            "unrelated\n\nPayment status is pending",
            "payment status",
            30,
        );
        assert!(result.contains("Payment"));
        assert!(result.chars().count() <= 30);
    }
    #[test]
    fn excerpts_keep_the_relevant_sections_and_their_code_whole() {
        let page = format!(
            "# Crear Usuario Natural\n\nServicio crear usuario natural.\n\n## Estructura\n\n```\n{}```\n\n## Endpoints\n\n### Crear Usuario Natural\n\n#### Parámetros del Request\n\n| Campo | Tipo |\n|---|---|\n| `UserName` | string |\n\n#### Ejemplos de Uso\n\n```bash\ncurl -X POST \"https://x/generar\" \\\n  -d '{{\n    \"UserName\": \"1\",\n\n    \"Email\": \"a\"\n  }}'\n```\n\n## Changelog\n\n{}",
            "src/usuario-natural/crear.js\n".repeat(40),
            "- Crear usuario natural: cambio.\n".repeat(40),
        );
        let question =
            "¿Qué parámetros recibe el endpoint crear usuario natural? Crear Usuario Natural";
        let result = excerpt(&page, question, 700);
        assert!(result.chars().count() <= 700, "{result}");
        assert!(result.contains("| `UserName` | string |"), "{result}");
        // The example is under the Endpoints heading and keeps its blank line.
        assert!(
            result.contains("\"UserName\": \"1\",\n\n    \"Email\""),
            "{result}"
        );
        // Irrelevant bulk only fills the room left, cut: it never displaces the sections above.
        assert!(result.matches("cambio").count() < 40, "{result}");
        assert!(result.matches("crear.js").count() < 40, "{result}");
        // A text that fits stays whole and in order.
        assert_eq!(excerpt("a\n\nb", "b", 10), "a\n\nb");
    }
}
