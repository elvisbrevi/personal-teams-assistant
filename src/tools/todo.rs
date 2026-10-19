//! Typed results shared by the To-Do tools, the CLI and the GUI. No model is involved.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

fn default_limit() -> usize {
    10
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Filters {
    pub source: Option<String>,
    pub project: Option<String>,
    pub state: Option<String>,
    pub kind: Option<String>,
    pub limit: usize,
}
impl Default for Filters {
    fn default() -> Self {
        Self {
            source: None,
            project: None,
            state: None,
            kind: None,
            limit: default_limit(),
        }
    }
}
impl Filters {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.kind
                .as_deref()
                .is_none_or(|kind| ["hu", "bug"].contains(&kind)),
            "invalid kind: use hu or bug"
        );
        ensure!(
            (1..=100).contains(&self.limit),
            "limit must be between 1 and 100"
        );
        for value in [&self.source, &self.project, &self.state]
            .into_iter()
            .flatten()
        {
            ensure!(
                !value.trim().is_empty()
                    && value.len() <= 200
                    && !value.chars().any(char::is_control),
                "invalid To-Do filter"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Date {
    pub field: String,
    pub name: String,
    pub value: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub project: String,
    pub state: String,
    #[serde(default)]
    pub kind: String,
    pub created_at: Option<String>,
    pub dates: Vec<Date>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailRequest {
    pub id: u64,
    pub source: Option<String>,
    pub project: Option<String>,
}
impl DetailRequest {
    pub fn validate(&self, github: bool) -> Result<()> {
        ensure!(
            (1..=i32::MAX as u64).contains(&self.id),
            "invalid work item ID"
        );
        Filters {
            source: self.source.clone(),
            project: self.project.clone(),
            ..Default::default()
        }
        .validate()?;
        if github {
            ensure!(self.source.is_none(), "invalid GitHub source");
            validate_repository(self.project.as_deref().unwrap_or_default())?;
        }
        Ok(())
    }
}

pub fn validate_repository(repo: &str) -> Result<()> {
    let parts: Vec<_> = repo.split('/').collect();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|s| !s.is_empty()
                && *s != "."
                && *s != ".."
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))),
        "invalid GitHub repository: use OWNER/REPO"
    );
    Ok(())
}

/// Safe rich text, parsed without a browser or network. No provider attributes reach the UI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RichNode {
    pub tag: String,
    pub text: String,
    pub url: Option<String>,
    pub children: Vec<RichNode>,
}
impl RichNode {
    fn text(text: &str) -> Self {
        Self {
            tag: "text".into(),
            text: text.into(),
            url: None,
            children: Vec::new(),
        }
    }
}
pub fn rich_nodes(raw: &str, base: &str) -> Vec<RichNode> {
    fn children(
        element: scraper::ElementRef<'_>,
        base: &str,
        remaining: &mut usize,
        depth: usize,
        clipped: &mut bool,
    ) -> Vec<RichNode> {
        let mut nodes = Vec::new();
        for child in element.children() {
            if *remaining == 0 || depth > 40 {
                *clipped = true;
                break;
            }
            *remaining -= 1;
            if let Some(text) = child.value().as_text() {
                nodes.push(RichNode::text(text));
                continue;
            }
            let Some(element) = scraper::ElementRef::wrap(child) else {
                continue;
            };
            let tag = element.value().name();
            if [
                "script", "style", "iframe", "object", "embed", "form", "input", "button", "svg",
                "math", "template", "noscript", "link", "meta", "video", "audio", "source",
            ]
            .contains(&tag)
            {
                continue;
            }
            let allowed = [
                "p",
                "div",
                "span",
                "strong",
                "b",
                "em",
                "i",
                "u",
                "s",
                "del",
                "br",
                "ul",
                "ol",
                "li",
                "pre",
                "code",
                "blockquote",
                "hr",
                "table",
                "thead",
                "tbody",
                "tr",
                "th",
                "td",
                "h1",
                "h2",
                "h3",
                "h4",
                "h5",
                "h6",
                "a",
                "img",
            ];
            let mut node = RichNode {
                tag: if allowed.contains(&tag) { tag } else { "span" }.into(),
                text: String::new(),
                url: None,
                children: Vec::new(),
            };
            if ["a", "img"].contains(&tag) {
                node.url = element
                    .value()
                    .attr(if tag == "a" { "href" } else { "src" })
                    .filter(|s| !s.is_empty())
                    .and_then(|raw| url::Url::parse(base).ok()?.join(raw).ok())
                    .and_then(|url| https_link(url.as_str()));
                if tag == "img" {
                    node.text = element.value().attr("alt").unwrap_or_default().into();
                }
            }
            node.children = children(element, base, remaining, depth + 1, clipped);
            nodes.push(node);
        }
        nodes
    }
    let document = scraper::Html::parse_fragment(raw);
    let mut clipped = false;
    let mut nodes = children(document.root_element(), base, &mut 12000, 0, &mut clipped);
    if clipped {
        nodes.push(RichNode {
            tag: "notice".into(),
            ..RichNode::text("")
        });
    }
    nodes
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetailField {
    pub reference: String,
    pub name: String,
    pub kind: String,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<RichNode>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    pub text: String,
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<RichNode>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Relation {
    pub name: String,
    pub url: Option<String>,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub author: String,
    pub at: Option<String>,
    pub fields: Vec<DetailField>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Detail {
    pub item: Item,
    pub fields: Vec<DetailField>,
    pub comments: Vec<Comment>,
    pub relations: Vec<Relation>,
    pub history: Vec<Change>,
    pub partial: bool,
    pub warnings: Vec<String>,
}
impl Detail {
    pub fn warn(&mut self, warning: &str) {
        self.partial = true;
        self.warnings.push(warning.into());
    }
}

/// Provider content can link out, but must never execute a scheme or contain URL credentials.
pub fn https_link(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    (url.scheme() == "https" && url.username().is_empty() && url.password().is_none())
        .then(|| url.to_string())
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct List {
    pub items: Vec<Item>,
    pub states: Vec<String>,
    pub projects: Vec<String>,
    /// Field names used for developer assignment, useful when a process differs from Assigned To.
    pub assignment_fields: Vec<String>,
    pub warnings: Vec<String>,
    pub partial: bool,
    pub has_more: bool,
}
impl List {
    pub fn finish(&mut self, limit: usize) {
        self.items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
                .then_with(|| a.url.cmp(&b.url))
        });
        self.items.dedup_by(|a, b| a.url == b.url);
        self.has_more |= self.items.len() > limit;
        self.items.truncate(limit);
        for values in [
            &mut self.states,
            &mut self.projects,
            &mut self.assignment_fields,
        ] {
            values.sort();
            values.dedup();
        }
    }
    pub fn warn(&mut self, warning: String) {
        self.partial = true;
        self.warnings.push(warning);
    }
}

/// Only canonical provider work-item links may leave the native app through its shell.
pub fn validate_link(raw: &str) -> Result<url::Url> {
    let url = url::Url::parse(raw)?;
    ensure!(
        url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid work item link"
    );
    let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
    let valid = match url.host_str() {
        Some("dev.azure.com") => parts.len() == 5 && parts[2..4] == ["_workitems", "edit"],
        Some("github.com") => parts.len() == 4 && parts[2] == "issues",
        _ => false,
    };
    ensure!(
        valid
            && parts.iter().all(|p| !p.is_empty())
            && parts
                .last()
                .is_some_and(|id| id.parse::<u64>().is_ok_and(|n| n > 0)),
        "invalid work item link"
    );
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_html_is_parsed_without_executable_attributes_or_remote_media() {
        let nodes = rich_nodes(
            r#"<script>unsafe()</script><p style="color:red" onclick="unsafe()">Safe <strong>text</strong></p><img src="/image.png" alt="Receipt" onerror="unsafe()"><a href="javascript:unsafe()">Bad link</a><a href="https://token@evil.test/">Credentials</a><iframe src="https://evil.test/"></iframe>"#,
            "https://dev.azure.com/example/Payments/_workitems/edit/7",
        );
        let value = serde_json::to_string(&nodes).unwrap();
        for forbidden in [
            "unsafe",
            "onclick",
            "onerror",
            "color:red",
            "iframe",
            "token@",
        ] {
            assert!(!value.contains(forbidden), "{value}");
        }
        assert!(value.contains("strong"));
        assert!(value.contains("https://dev.azure.com/image.png"));
        assert!(value.contains("Receipt"));
        let deep = format!("{}text{}", "<div>".repeat(100), "</div>".repeat(100));
        assert!(
            rich_nodes(&deep, "https://dev.azure.com/example/")
                .iter()
                .any(|node| node.tag == "notice")
        );
    }

    #[test]
    fn filters_and_links_cannot_widen_the_tool() {
        assert_eq!(serde_json::from_str::<Filters>("{}").unwrap().limit, 10);
        assert!(serde_json::from_str::<Filters>(r#"{"assignee":"someone"}"#).is_err());
        assert!(
            Filters {
                limit: 0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        for raw in [
            "https://github.com/example/repo/issues/12",
            "https://dev.azure.com/example/Project/_workitems/edit/7",
        ] {
            validate_link(raw).unwrap();
        }
        for raw in [
            "https://github.com.evil.test/a/b/issues/1",
            "https://token@github.com/a/b/issues/1",
            "https://github.com/a/b/issues/1?token=x",
            "https://dev.azure.com/a/b/_workitems/edit/0",
        ] {
            assert!(validate_link(raw).is_err(), "{raw}");
        }
    }
}
