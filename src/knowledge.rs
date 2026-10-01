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
/// Rank paragraphs locally; the pipeline shares its total budget between authorized sources.
pub fn excerpt(text: &str, question: &str, limit: usize) -> String {
    let words: Vec<String> = question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 2)
        .map(str::to_lowercase)
        .collect();
    let mut chunks: Vec<(usize, usize, String)> = text
        .split("\n\n")
        .enumerate()
        .map(|(i, p)| {
            let lower = p.to_lowercase();
            (
                words.iter().filter(|w| lower.contains(w.as_str())).count(),
                i,
                p.chars().take(limit).collect(),
            )
        })
        .collect();
    chunks.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut out = String::new();
    for (_, index, p) in chunks.into_iter().take(4) {
        out.push_str(&format!("[passage {}]\n{}\n", index + 1, p));
        if out.chars().count() >= limit {
            break;
        }
    }
    out.chars().take(limit).collect()
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
}
