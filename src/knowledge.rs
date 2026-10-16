use anyhow::{Context as _, Result, ensure};
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
    /// Subjects the user works on («cobros», a project): the words that name them, the
    /// sources that know about them and how to answer about them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<Context>,
    /// What the user changed in the built-in cases (kinds of request); see `crate::cases`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cases: Vec<crate::cases::Case>,
}
/// A subject the user works on. A request that names it (one of its keywords, its name or ID)
/// is answered from its sources when the conversation may read them, with its instructions.
/// It narrows and guides; it never authorizes a source.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Words or phrases that name the subject, matched whole, ignoring case and accents.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// The sources that know about it, by ID.
    #[serde(default)]
    pub sources: Vec<String>,
    /// How to answer about it: terms, tone, what to include.
    #[serde(default)]
    pub instructions: String,
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
    File {
        repository: String,
        path: PathBuf,
    },
    Url {
        url: String,
    },
    Tool {
        tool: crate::tools::ToolSpec,
    },
    /// A whole repository: its Markdown and text files, searched through the knowledge index.
    Repository {
        repository: String,
    },
    /// The verified notes (written in the app, with `pta notes` or by agents); `contexts`
    /// keeps only the notes of those contexts and the general ones.
    Notes {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        contexts: Vec<String>,
    },
}
impl Access {
    /// Read through the knowledge index rather than whole.
    pub fn indexed(&self) -> bool {
        matches!(self, Self::Repository { .. } | Self::Notes { .. })
    }
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
        crate::cases::validate(&map.cases, |id| map.resources.iter().any(|r| r.id == id))?;
        ensure!(map.contexts.len() <= 50, "too many contexts");
        let mut context_ids = BTreeSet::new();
        let plain = |text: &str, max: usize| {
            text.chars().count() <= max
                && !text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
        };
        for c in &map.contexts {
            ensure!(
                id_pattern.is_match(&c.id) && context_ids.insert(c.id.as_str()),
                "invalid or duplicate context id"
            );
            ensure!(
                !c.name.trim().is_empty()
                    && plain(&c.name, 80)
                    && plain(&c.description, 2000)
                    && plain(&c.instructions, 4000),
                "a context needs a name of at most 80 characters, a description of at most 2000 and instructions of at most 4000"
            );
            ensure!(
                c.keywords.len() <= 50
                    && c.keywords
                        .iter()
                        .all(|k| !k.trim().is_empty() && plain(k, 80)),
                "a context has at most 50 keywords of at most 80 characters"
            );
            ensure!(
                c.sources.len() <= 50
                    && c.sources
                        .iter()
                        .all(|id| map.resources.iter().any(|r| &r.id == id)),
                "a context's sources must exist in the knowledge map"
            );
        }
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
                Access::Repository { repository } => {
                    ensure!(
                        map.repositories.contains_key(repository),
                        "unknown repository"
                    );
                }
                Access::Notes { contexts } => {
                    ensure!(
                        contexts.iter().all(|c| context_ids.contains(c.as_str())),
                        "a notes source's contexts must exist in the knowledge map"
                    );
                }
                Access::Tool { tool } => {
                    tool.validate()?;
                    // A catalog file must live in a declared repository; an inline one needs none.
                    if let crate::tools::ToolSpec::AzureDevopsWiki {
                        repository, path, ..
                    }
                    | crate::tools::ToolSpec::AzureDevopsStatus {
                        repository, path, ..
                    } = tool
                        && tool.catalog_repository().is_some()
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
    /// The context a request names: the one with the most of its keywords, name or ID found
    /// as whole words (ignoring case and accents); the first in the map on a tie.
    pub fn context_for(&self, text: &str) -> Option<&Context> {
        let mut best: Option<(usize, &Context)> = None;
        for context in &self.contexts {
            let found = phrases_named(
                text,
                context
                    .keywords
                    .iter()
                    .map(String::as_str)
                    .chain([context.name.as_str(), context.id.as_str()]),
            );
            if found > 0 && best.is_none_or(|(most, _)| found > most) {
                best = Some((found, context));
            }
        }
        best.map(|(_, context)| context)
    }
    /// The user's changes to a built-in case, if any.
    pub fn case(&self, id: &str) -> Option<&crate::cases::Case> {
        self.cases.iter().find(|c| c.id == id)
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
            Access::Repository { .. } | Access::Notes { .. } => {
                anyhow::bail!("indexed resources are read through the knowledge index")
            }
        };
        Ok(text)
    }
}
/// How many of `phrases` appear in `text` as whole words, ignoring case and accents.
pub fn phrases_named<'a>(text: &str, phrases: impl IntoIterator<Item = &'a str>) -> usize {
    let words = |text: &str| -> Vec<String> {
        crate::index::fold(text)
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let request = words(text);
    phrases
        .into_iter()
        .filter(|phrase| {
            let phrase = words(phrase);
            !phrase.is_empty()
                && request
                    .windows(phrase.len())
                    .any(|w| w == phrase.as_slice())
        })
        .count()
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
pub(crate) fn parts(text: &str, limit: usize) -> Vec<(String, String)> {
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
    fn a_request_names_its_context_by_keywords_name_or_id() {
        let map = KnowledgeMap::parse(
            "[[resources]]\nid = \"wiki-cobros\"\ndescription = \"x\"\ntopics = []\nenabled = false\nexternal_processing = false\nallowed_conversations = []\nkind = \"url\"\nurl = \"https://docs.example.com/\"\n\n[[contexts]]\nid = \"cobros\"\nname = \"Cobros\"\nkeywords = [\"conciliación bancaria\", \"factura\"]\nsources = [\"wiki-cobros\"]\n\n[[contexts]]\nid = \"portal-pagos\"\nname = \"Portal de pagos\"\nkeywords = [\"pago QR\", \"factura\"]\n",
        )
        .unwrap();
        let named = |text: &str| map.context_for(text).map(|c| c.id.as_str());
        assert_eq!(named("¿Cómo va la CONCILIACION bancaria?"), Some("cobros"));
        // A phrase matches whole: «bancaria» alone or «conciliaciones» is not the phrase.
        assert_eq!(named("una cuenta bancaria"), None);
        // More matches win; a tie keeps the first context.
        assert_eq!(named("la factura del pago qr"), Some("portal-pagos"));
        assert_eq!(named("una factura"), Some("cobros"));
        assert_eq!(named("el portal de pagos"), Some("portal-pagos"));
        assert_eq!(named("portal-pagos"), Some("portal-pagos"));
        // Contexts name existing sources only, with unique IDs.
        let invalid = |extra: &str| {
            KnowledgeMap::parse(&format!("[[contexts]]\nid = \"a\"\nname = \"A\"\n{extra}"))
                .is_err()
        };
        assert!(invalid("sources = [\"missing\"]\n"));
        assert!(invalid("\n[[contexts]]\nid = \"a\"\nname = \"B\"\n"));
        assert!(invalid("keywords = [\" \"]\n"));
        assert!(!invalid(""));
    }
    #[test]
    fn repositories_and_notes_are_sources_that_round_trip() {
        let text = "[repositories]\nhandbook = \"/tmp/handbook\"\n\n[[resources]]\nid = \"handbook\"\ndescription = \"Runbooks\"\ntopics = []\nenabled = false\nexternal_processing = false\nallowed_conversations = []\nkind = \"repository\"\nrepository = \"handbook\"\n\n[[resources]]\nid = \"notes\"\ndescription = \"Notes\"\ntopics = []\nenabled = false\nexternal_processing = false\nallowed_conversations = []\nkind = \"notes\"\ncontexts = [\"cobros\"]\n\n[[contexts]]\nid = \"cobros\"\nname = \"Cobros\"\nsources = [\"handbook\"]\n";
        let map = KnowledgeMap::parse(text).unwrap();
        assert!(map.resources.iter().all(|r| r.access.indexed()));
        let saved = toml::to_string(&map).unwrap();
        assert_eq!(KnowledgeMap::parse(&saved).unwrap().resources.len(), 2);
        // An undeclared repository, an unknown context or an extra field are refused.
        assert!(
            KnowledgeMap::parse(
                &text.replace("repository = \"handbook\"", "repository = \"other\"")
            )
            .is_err()
        );
        assert!(
            KnowledgeMap::parse(&text.replace("contexts = [\"cobros\"]", "contexts = [\"pagos\"]"))
                .is_err()
        );
        assert!(
            KnowledgeMap::parse(
                &text.replace("kind = \"notes\"", "kind = \"notes\"\npath = \"x\"")
            )
            .is_err()
        );
        // A map without contexts writes none.
        assert!(
            !toml::to_string(&KnowledgeMap::parse("").unwrap())
                .unwrap()
                .contains("contexts")
        );
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
