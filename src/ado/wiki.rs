//! Native ADO Wiki/Search reads. No content cache, cloning, embeddings or permission fallback.
use super::{Catalog, Source, organization_url, project_url};
use crate::evidence::{Evidence, Reference};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::time::{Instant, timeout_at};
use url::Url;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthorMode {
    #[default]
    PreferMine,
    MineOnly,
    All,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchInput {
    pub query: String,
    #[serde(default)]
    pub wiki_id: Option<String>,
    #[serde(default)]
    pub author_mode: Option<AuthorMode>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadInput {
    pub wiki_id: String,
    pub path: String,
}
impl SearchInput {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.query.trim().is_empty()
                && self.query.chars().count() <= 500
                && !self.query.chars().any(char::is_control),
            "invalid Wiki query: use 1–500 printable characters"
        );
        if let Some(id) = &self.wiki_id {
            validate_id(id)?;
        }
        Ok(())
    }
}
impl ReadInput {
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.wiki_id)?;
        validate_path(&self.path)
    }
}
pub fn validate_id(id: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(id).is_ok(),
        "invalid Wiki ID: expected UUID"
    );
    Ok(())
}
fn validate_path(path: &str) -> Result<()> {
    ensure!(
        path.starts_with('/')
            && path.len() <= 2048
            && !path.contains('\\')
            && !path.chars().any(char::is_control)
            && !path.split('/').any(|p| p == "." || p == "..")
            && (path == "/" || !path.contains("//")),
        "invalid canonical Wiki path"
    );
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wiki {
    pub organization: String,
    pub project: String,
    pub project_id: String,
    pub id: String,
    pub name: String,
    pub kind: String,
    pub repository_id: String,
    pub mapped_path: String,
    pub versions: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub wiki: Wiki,
    pub title: String,
    pub path: String,
    pub git_item_path: String,
    pub version: String,
    pub revision: String,
    #[serde(default)]
    pub git_revision: Option<String>,
    pub content: String,
    pub reference: Reference,
    #[serde(default)]
    pub entities: Vec<Reference>,
}
#[derive(Default, Debug, Serialize, Deserialize)]
pub struct WikiResult {
    pub query: String,
    pub wikis: Vec<Wiki>,
    pub pages: Vec<Page>,
    pub candidates: usize,
    pub histories_checked: usize,
    pub partial: bool,
    pub warnings: Vec<String>,
}
impl WikiResult {
    pub fn evidence(&self, question: &str, budget: usize) -> Evidence {
        let mut evidence = Evidence {
            partial: self.partial,
            warnings: self.warnings.clone(),
            ..Default::default()
        };
        let each = budget / self.pages.len().max(1);
        for p in &self.pages {
            let header = format!(
                "\nPágina Wiki {}: {}\n",
                p.reference.id,
                serde_json::to_string(&(&p.reference, &p.entities)).unwrap()
            );
            if header.chars().count() + 50 > each {
                evidence.partial = true;
                continue;
            }
            let excerpt =
                crate::knowledge::excerpt(&p.content, question, each - header.chars().count() - 1);
            if excerpt.trim().is_empty() {
                continue;
            }
            evidence.text.push_str(&header);
            evidence.text.push_str(&excerpt);
            evidence.text.push('\n');
            evidence.references.push(p.reference.clone());
            evidence.references.extend(p.entities.clone());
        }
        if self.partial {
            evidence.text.push_str("\nCobertura Wiki parcial: ");
            evidence.text.push_str(&self.warnings.join("; "));
        }
        evidence
    }
    fn warn(&mut self, phase: &str) {
        self.partial = true;
        if !self.warnings.iter().any(|w| w == phase) {
            self.warnings.push(phase.to_owned());
        }
    }
}

pub struct Reader<'a> {
    client: &'a Client,
    key: &'a str,
    catalog: Catalog,
    wiki_ids: &'a [String],
    deadline: Instant,
    #[cfg(test)]
    base: Option<String>,
}
impl<'a> Reader<'a> {
    pub fn new(
        client: &'a Client,
        key: &'a str,
        catalog: &str,
        wiki_ids: &'a [String],
    ) -> Result<Self> {
        for id in wiki_ids {
            validate_id(id)?;
        }
        Ok(Self {
            client,
            key,
            catalog: Catalog::parse(catalog)?,
            wiki_ids,
            deadline: Instant::now() + Duration::from_secs(30),
            #[cfg(test)]
            base: None,
        })
    }
    async fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<Value>,
    ) -> Result<(Value, String)> {
        ensure!(
            matches!(
                url.host_str(),
                Some("dev.azure.com" | "almsearch.dev.azure.com")
            ),
            "invalid Wiki API origin"
        );
        #[cfg(test)]
        let url = if let Some(base) = &self.base {
            Url::parse(&format!(
                "{base}{}?{}",
                url.path(),
                url.query().unwrap_or("")
            ))?
        } else {
            url
        };
        let operation = async {
            let mut req = self
                .client
                .request(method, url)
                .basic_auth("", Some(self.key))
                .header("Accept", "application/json")
                .timeout(Duration::from_secs(8));
            if let Some(body) = body {
                req = req.json(&body);
            }
            let r = req
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("Wiki API network or request timeout"))?;
            let status = r.status().as_u16();
            ensure!(
                (200..300).contains(&status),
                "Wiki API HTTP {status}: {}",
                match status {
                    401 => "credential rejected; audit existing Azure credential",
                    403 =>
                        "access denied; Wiki reads/search need vso.wiki, Git attribution needs vso.code",
                    404 => "wiki, page or published version not found",
                    429 => "rate limit; retry later",
                    _ => "read failed",
                }
            );
            let revision = r
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .trim_matches('"')
                .to_owned();
            let data = crate::adapters::bounded_json(r, 1_000_000)
                .await
                .map_err(|_| anyhow::anyhow!("Wiki API invalid JSON or response exceeds 1 MB"))?;
            Ok((data, revision))
        };
        timeout_at(self.deadline, operation)
            .await
            .map_err(|_| anyhow::anyhow!("Wiki operation timeout (30 seconds)"))?
    }
    fn allowed_id(&self, id: &str) -> bool {
        self.wiki_ids.is_empty() || self.wiki_ids.iter().any(|x| x.eq_ignore_ascii_case(id))
    }
    async fn catalog_for(&self, source: &Source, project: Option<&str>) -> Result<Vec<Wiki>> {
        let url = if let Some(p) = project {
            project_url(source, p, "dev.azure.com", &["_apis", "wiki", "wikis"])?
        } else {
            organization_url(source, &["_apis", "wiki", "wikis"])?
        };
        let expected_project = if let Some(p) = project {
            let project_url = organization_url(source, &["_apis", "projects", p])?;
            let (project_data, _) = self.request(Method::GET, project_url, None).await?;
            let id = project_data["id"]
                .as_str()
                .context("Wiki project ID missing")?;
            let name = project_data["name"]
                .as_str()
                .context("Wiki project name missing")?;
            ensure!(
                name.eq_ignore_ascii_case(p) || id.eq_ignore_ascii_case(p),
                "Wiki project resolution outside scope"
            );
            Some(id.to_owned())
        } else {
            None
        };
        let (data, _) = self.request(Method::GET, url, None).await?;
        let mut wikis = Vec::new();
        for v in data["value"]
            .as_array()
            .context("invalid Wiki catalog response")?
        {
            let id = v["id"].as_str().context("missing Wiki ID")?;
            if !self.allowed_id(id) {
                continue;
            }
            validate_id(id)?;
            let project_id = v["projectId"].as_str().context("missing Wiki project")?;
            validate_id(project_id)?;
            if expected_project
                .as_ref()
                .is_some_and(|expected| !expected.eq_ignore_ascii_case(project_id))
            {
                continue;
            }
            let repo = v["repositoryId"]
                .as_str()
                .context("missing Wiki repository")?;
            validate_id(repo)?;
            let mapped_path = v["mappedPath"].as_str().unwrap_or("/");
            validate_path(mapped_path)?;
            let kind = v["type"].as_str().unwrap_or("");
            ensure!(
                matches!(kind, "projectWiki" | "codeWiki"),
                "unsupported Wiki type"
            );
            let versions: Vec<String> = v["versions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["version"].as_str().map(str::to_owned))
                .collect();
            ensure!(
                !versions.is_empty() && versions.len() <= 20,
                "missing or excessive Wiki published versions"
            );
            // Explicit project calls cannot grant another project's wiki. projectId resolves names.
            wikis.push(Wiki {
                organization: source.organization.clone(),
                project: project.unwrap_or(project_id).into(),
                project_id: project_id.into(),
                id: id.into(),
                name: v["name"].as_str().unwrap_or("Wiki").into(),
                kind: kind.into(),
                repository_id: repo.into(),
                mapped_path: mapped_path.into(),
                versions,
            });
        }
        Ok(wikis)
    }
    pub async fn list(&self) -> Result<WikiResult> {
        let mut result = WikiResult::default();
        for source in &self.catalog.sources {
            if source.projects == ["*"] {
                result.wikis.extend(self.catalog_for(source, None).await?);
            } else {
                for p in &source.projects {
                    result
                        .wikis
                        .extend(self.catalog_for(source, Some(p)).await?);
                }
            }
        }
        Ok(result)
    }
    fn source(&self, wiki: &Wiki) -> Result<&Source> {
        self.catalog
            .sources
            .iter()
            .find(|s| s.organization == wiki.organization)
            .context("Wiki outside source")
    }
    fn url(&self, wiki: &Wiki, segments: &[&str]) -> Result<Url> {
        project_url(
            self.source(wiki)?,
            &wiki.project_id,
            "dev.azure.com",
            segments,
        )
    }
    async fn tree(&self, wiki: &Wiki, version: &str) -> Result<Value> {
        let mut url = self.url(wiki, &["_apis", "wiki", "wikis", &wiki.id, "pages"])?;
        url.query_pairs_mut()
            .append_pair("path", "/")
            .append_pair("recursionLevel", "full")
            .append_pair("versionDescriptor.version", version)
            .append_pair("versionDescriptor.versionType", "branch");
        Ok(self.request(Method::GET, url, None).await?.0)
    }
    async fn page(
        &self,
        wiki: &Wiki,
        path: &str,
        expected_git: Option<&str>,
        version: &str,
    ) -> Result<Page> {
        validate_path(path)?;
        ensure!(
            wiki.versions.iter().any(|v| v == version),
            "unpublished Wiki version"
        );
        let mut url = self.url(wiki, &["_apis", "wiki", "wikis", &wiki.id, "pages"])?;
        url.query_pairs_mut()
            .append_pair("path", path)
            .append_pair("includeContent", "true")
            .append_pair("versionDescriptor.version", version)
            .append_pair("versionDescriptor.versionType", "branch");
        let (v, revision) = self.request(Method::GET, url, None).await?;
        ensure!(
            v["path"].as_str() == Some(path),
            "Wiki returned another page"
        );
        let git = v["gitItemPath"].as_str().context("missing Wiki Git path")?;
        validate_path(git)?;
        ensure!(
            under_mapped(git, &wiki.mapped_path),
            "Wiki Git path escapes published folder"
        );
        if let Some(expected) = expected_git {
            ensure!(
                expected == git,
                "Search path does not match Wiki page Git path"
            );
        }
        let remote = v["remoteUrl"]
            .as_str()
            .context("Wiki page has no verified link")?;
        verify_page_url(remote, wiki, path, v["id"].as_u64())?;
        let remote_url = Url::parse(remote)?;
        let linked_version = remote_url
            .query_pairs()
            .find(|(k, _)| k == "version")
            .map(|(_, v)| v.into_owned());
        if let Some(linked) = &linked_version {
            ensure!(
                linked == version || linked == &format!("GB{version}"),
                "Wiki page link points to another published version"
            );
        }
        ensure!(
            wiki.versions.len() == 1 || linked_version.is_some(),
            "Wiki page link does not resolve the published version"
        );
        let content = v["content"]
            .as_str()
            .context("Wiki content not returned")?
            .to_owned();
        let title = path.rsplit('/').next().unwrap_or("Wiki").to_owned();
        let reference = Reference {
            id: format!(
                "wiki:{}:{}:{}:{}:{}",
                wiki.organization, wiki.project_id, wiki.id, version, path
            ),
            kind: "wiki".into(),
            label: format!("{} / {} / {}", wiki.project, wiki.name, title),
            url: Url::parse(remote)?
                .to_string()
                .replace('(', "%28")
                .replace(')', "%29"),
            organization: wiki.organization.clone(),
            project: if Url::parse(remote)?.path().starts_with(&format!(
                "{}/{}/",
                Url::parse(&wiki.organization)?.path().trim_end_matches('/'),
                wiki.project_id
            )) {
                wiki.project_id.clone()
            } else {
                wiki.project.clone()
            },
            aliases: vec![],
            parent: None,
            revision: Some(revision.clone()),
            authority: Some("unknown".into()),
            author: None,
            author_role: None,
        };
        reference.validate()?;
        Ok(Page {
            wiki: wiki.clone(),
            title,
            path: path.into(),
            git_item_path: git.into(),
            version: version.into(),
            revision,
            git_revision: None,
            content,
            reference,
            entities: Vec::new(),
        })
    }
    async fn attribution(&self, page: &mut Page) -> Result<()> {
        // Pin history to the revision actually read, never branch HEAD after a concurrent edit.
        ensure!(
            page.revision.len() == 40 && page.revision.chars().all(|c| c.is_ascii_hexdigit()),
            "Wiki revision unavailable for Git attribution"
        );
        let source = self.source(&page.wiki)?;
        let mut item_url = self.url(
            &page.wiki,
            &[
                "_apis",
                "git",
                "repositories",
                &page.wiki.repository_id,
                "items",
            ],
        )?;
        item_url
            .query_pairs_mut()
            .append_pair("path", &page.git_item_path)
            .append_pair("versionDescriptor.version", &page.version)
            .append_pair("versionDescriptor.versionType", "branch");
        let (item, _) = self.request(Method::GET, item_url, None).await?;
        let commit = item["commitId"]
            .as_str()
            .context("Git item revision missing")?;
        ensure!(
            commit.len() == 40 && commit.chars().all(|c| c.is_ascii_hexdigit()),
            "Git item revision invalid"
        );
        ensure!(
            item["objectId"].as_str() == Some(&page.revision) || commit == page.revision,
            "Wiki content revision changed before Git attribution"
        );
        page.git_revision = Some(commit.into());
        page.reference.revision = Some(commit.into());
        let mut url = self.url(
            &page.wiki,
            &[
                "_apis",
                "git",
                "repositories",
                &page.wiki.repository_id,
                "commits",
            ],
        )?;
        url.query_pairs_mut()
            .append_pair("searchCriteria.itemPath", &page.git_item_path)
            .append_pair("searchCriteria.itemVersion.version", commit)
            .append_pair("searchCriteria.itemVersion.versionType", "commit")
            .append_pair("searchCriteria.$top", "1");
        let mut own = url.clone();
        own.query_pairs_mut()
            .append_pair("searchCriteria.author", &source.author_email);
        let (history, _) = self.request(Method::GET, own, None).await?;
        if let Some(commit) = history["value"].as_array().and_then(|v| v.first())
            && commit["author"]["email"]
                .as_str()
                .is_some_and(|e| e.eq_ignore_ascii_case(&source.author_email))
        {
            page.reference.authority = Some("edited_by_me".into());
            if let Some(sha) = commit["commitId"]
                .as_str()
                .filter(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()))
            {
                let changes_url = self.url(
                    &page.wiki,
                    &[
                        "_apis",
                        "git",
                        "repositories",
                        &page.wiki.repository_id,
                        "commits",
                        sha,
                        "changes",
                    ],
                )?;
                let ambiguous = commit["comment"].as_str().is_some_and(|c| {
                    ["squash", "import", "rename"]
                        .iter()
                        .any(|term| c.to_lowercase().contains(term))
                });
                if !ambiguous
                    && let Ok((changes, _)) = self.request(Method::GET, changes_url, None).await
                    && changes["changes"].as_array().is_some_and(|v| {
                        v.len() == 1
                            && v[0]["changeType"].as_str() == Some("add")
                            && v[0]["item"]["path"].as_str() == Some(page.git_item_path.as_str())
                            && v[0]["originalPath"].is_null()
                    })
                    && changes["changeCounts"]["Add"].as_u64() == Some(1)
                    && commit["parents"].as_array().is_some_and(|p| p.len() == 1)
                {
                    page.reference.authority = Some("created_by_me".into());
                }
            }
            return Ok(());
        }
        let (latest, _) = self.request(Method::GET, url, None).await?;
        if let Some(commit) = latest["value"].as_array().and_then(|v| v.first()) {
            page.reference.author = commit["author"]["name"]
                .as_str()
                .filter(|n| !n.trim().is_empty())
                .map(str::to_owned);
            page.reference.author_role = Some("último editor registrado".into());
            // A bounded/filtered history cannot prove the absence of earlier own contributions.
            if page.reference.author.is_some() && commit["author"]["email"].as_str().is_some() {
                page.reference.authority = Some("other".into());
            }
        }
        Ok(())
    }
    async fn entities(&self, page: &mut Page, result: &mut WikiResult) {
        let urls = regex::Regex::new(r"https://dev\.azure\.com/[^\s<>)\]]+").unwrap();
        let mut seen = BTreeSet::new();
        for hit in urls.find_iter(&page.content).take(16) {
            if !seen.insert(hit.as_str()) {
                continue;
            }
            let Some(entity) = super::entity_link(
                hit.as_str(),
                &page.wiki.organization,
                &[&page.wiki.project_id, &page.wiki.project],
            ) else {
                continue;
            };
            let segments = match entity.kind.as_str() {
                "work_item" => vec!["_apis", "wit", "workitems", entity.id.as_str()],
                "pipeline_run" => vec!["_apis", "build", "builds", entity.id.as_str()],
                "pipeline_definition" => vec!["_apis", "build", "definitions", entity.id.as_str()],
                _ => continue,
            };
            let Ok(url) = self.url(&page.wiki, &segments) else {
                continue;
            };
            match self.request(Method::GET, url, None).await {
                Ok((v, _))
                    if v["id"]
                        .as_u64()
                        .is_some_and(|id| id.to_string() == entity.id) =>
                {
                    let project_ok = if entity.kind == "work_item" {
                        let Ok(source) = self.source(&page.wiki) else {
                            continue;
                        };
                        let Ok(project_url) =
                            organization_url(source, &["_apis", "projects", &page.wiki.project_id])
                        else {
                            continue;
                        };
                        match self.request(Method::GET, project_url, None).await {
                            Ok((project, _)) => {
                                project["id"].as_str() == Some(&page.wiki.project_id)
                                    && project["name"].as_str().is_some_and(|name| {
                                        v["fields"]["System.TeamProject"]
                                            .as_str()
                                            .is_some_and(|actual| actual.eq_ignore_ascii_case(name))
                                    })
                            }
                            Err(_) => false,
                        }
                    } else {
                        v["project"]["id"].as_str() == Some(&page.wiki.project_id)
                    };
                    if !project_ok {
                        result.warn("Linked Azure entity project could not be verified; excluded");
                        continue;
                    }

                    let title = if entity.kind == "work_item" {
                        v["fields"]["System.Title"].as_str().unwrap_or("Work item")
                    } else {
                        v["name"]
                            .as_str()
                            .or_else(|| v["definition"]["name"].as_str())
                            .unwrap_or("Pipeline")
                    };
                    let label = format!(
                        "{} {title} #{} ({})",
                        entity.kind,
                        entity.id,
                        if entity.kind == "pipeline_definition" {
                            "configuración"
                        } else if entity.kind == "pipeline_run" {
                            "ejecución"
                        } else {
                            "work item"
                        }
                    );
                    let reference = Reference {
                        id: format!(
                            "{}:{}:{}:{}",
                            entity.kind, page.wiki.organization, page.wiki.project_id, entity.id
                        ),
                        kind: entity.kind,
                        label,
                        url: entity.url,
                        organization: page.wiki.organization.clone(),
                        project: entity.project,
                        aliases: vec![format!("#{}", entity.id), title.into()],
                        parent: None,
                        revision: None,
                        authority: None,
                        author: None,
                        author_role: None,
                    };
                    if reference.validate().is_ok() {
                        if let Some(stage_id) = entity.stage
                            && reference.kind == "pipeline_run"
                            && let Ok(url) = self.url(
                                &page.wiki,
                                &["_apis", "build", "builds", &entity.id, "timeline"],
                            )
                            && let Ok((timeline, _)) = self.request(Method::GET, url, None).await
                        {
                            if let Some(stage) =
                                timeline["records"].as_array().and_then(|records| {
                                    records.iter().find(|s| {
                                        s["type"].as_str() == Some("Stage")
                                            && s["id"].as_str() == Some(&stage_id)
                                    })
                                })
                            {
                                let name = stage["name"].as_str().unwrap_or("sin nombre");
                                let mut sr = reference.clone();
                                sr.id = format!("stage:{}:{}", reference.id, stage_id);
                                sr.kind = "stage_run".into();
                                sr.label = format!(
                                    "Stage {name}, ejecución #{} que lo contiene",
                                    entity.id
                                );
                                sr.aliases = vec![name.into()];
                                sr.parent = Some(reference.id.clone());
                                page.entities.push(sr);
                            } else {
                                result.warn("Linked stage identity could not be verified");
                            }
                        }
                        page.entities.push(reference);
                    }
                }
                _ => result.warn(
                    "Linked Azure entity could not be verified; concrete claim must be excluded",
                ),
            }
        }
    }
    pub async fn read(&self, input: &ReadInput) -> Result<WikiResult> {
        input.validate()?;
        let mut result = self.list().await?;
        let candidates: Vec<_> = result
            .wikis
            .iter()
            .filter(|w| w.id.eq_ignore_ascii_case(&input.wiki_id))
            .cloned()
            .collect();
        ensure!(
            candidates.len() == 1,
            "invalid Wiki selection: ID outside source or ambiguous"
        );
        let wiki = &candidates[0];
        let version = wiki.versions.first().context("Wiki version missing")?;
        let mut page = self.page(wiki, &input.path, None, version).await?;
        result.histories_checked = 1;
        if let Err(error) = self.attribution(&mut page).await {
            result.warn(&format!(
                "Git attribution incomplete; author unknown: {error}"
            ));
        }
        self.entities(&mut page, &mut result).await;
        result.pages.push(page);
        Ok(result)
    }
    pub async fn search(
        &self,
        input: &SearchInput,
        default_mode: AuthorMode,
    ) -> Result<WikiResult> {
        input.validate()?;
        if let Some(id) = &input.wiki_id {
            ensure!(
                self.allowed_id(id),
                "invalid Wiki selection: outside source"
            );
        }
        let mut result = WikiResult {
            query: input.query.trim().into(),
            ..Default::default()
        };
        let mut seen = BTreeSet::new();
        let mut catalogs = BTreeMap::<(String, String), Vec<Wiki>>::new();
        let mut trees = BTreeMap::<(String, String, String), Value>::new();
        let mut read_candidates = Vec::new();
        'organizations: for source in &self.catalog.sources {
            for skip in (0..100).step_by(25) {
                if result.candidates >= 100 {
                    result.warn("Search candidate limit (100)");
                    break 'organizations;
                }
                let mut url = organization_url(source, &["_apis", "search", "wikisearchresults"])?;
                url.set_host(Some("almsearch.dev.azure.com"))?;
                let mut body = json!({"searchText":result.query,"$top":25,"$skip":skip});
                if source.projects != ["*"] {
                    body["filters"] = json!({"Project":source.projects});
                }
                let (v, _) = match self.request(Method::POST, url, Some(body)).await {
                    Ok(v) => v,
                    Err(e)
                        if e.to_string().contains("HTTP 401")
                            || e.to_string().contains("HTTP 403") =>
                    {
                        return Err(e);
                    }
                    Err(e) if result.candidates == 0 => return Err(e),
                    Err(_) => {
                        result.warn("Search phase incomplete (network/timeout)");
                        break 'organizations;
                    }
                };
                let info = v["infoCode"].as_i64().context("Search infoCode missing")?;
                if matches!(info, 3 | 4 | 5 | 19 | 20) {
                    bail!("invalid Wiki search syntax (infoCode {info})");
                }
                if info != 0 {
                    result.warn(&format!(
                        "Search index/coverage incomplete (infoCode {info})"
                    ));
                }
                let hits = v["results"]
                    .as_array()
                    .context("invalid Wiki Search results")?;
                for hit in hits {
                    if result.candidates >= 100 {
                        result.warn("Search global candidate limit (100)");
                        break 'organizations;
                    }
                    result.candidates += 1;
                    let Some(project) = hit["project"]["name"].as_str() else {
                        result.warn("Search project metadata missing");
                        continue;
                    };
                    let pid = hit["project"]["id"].as_str().unwrap_or("");
                    if source.projects != ["*"]
                        && !source
                            .projects
                            .iter()
                            .any(|p| p.eq_ignore_ascii_case(project) || p.eq_ignore_ascii_case(pid))
                    {
                        result.warn("Search returned unauthorized project; excluded");
                        continue;
                    }
                    let wid = hit["wiki"]["id"].as_str().unwrap_or("");
                    if !self.allowed_id(wid)
                        || input
                            .wiki_id
                            .as_ref()
                            .is_some_and(|id| !id.eq_ignore_ascii_case(wid))
                    {
                        continue;
                    }
                    let git = hit["path"].as_str().unwrap_or("");
                    let version = hit["wiki"]["version"].as_str().unwrap_or("");
                    if !seen.insert((
                        source.organization.clone(),
                        pid.to_owned(),
                        wid.to_owned(),
                        git.to_owned(),
                        version.to_owned(),
                    )) {
                        continue;
                    }
                    if read_candidates.len() >= 10 {
                        result.warn("Relevant candidate/history limit (10)");
                        continue;
                    }
                    let ck = (
                        source.organization.clone(),
                        if source.projects == ["*"] {
                            "*".to_owned()
                        } else {
                            pid.to_owned()
                        },
                    );
                    if !catalogs.contains_key(&ck) {
                        match self
                            .catalog_for(
                                source,
                                if source.projects == ["*"] {
                                    None
                                } else {
                                    Some(project)
                                },
                            )
                            .await
                        {
                            Ok(catalog) => {
                                catalogs.insert(ck.clone(), catalog);
                            }
                            Err(e)
                                if e.to_string().contains("HTTP 401")
                                    || e.to_string().contains("HTTP 403") =>
                            {
                                return Err(e);
                            }
                            Err(_) => {
                                result.warn("Wiki catalog resolution incomplete");
                                continue;
                            }
                        }
                    }
                    let Some(mut wiki) = catalogs[&ck]
                        .iter()
                        .find(|w| w.id == wid && w.project_id == pid)
                        .cloned()
                    else {
                        result.warn("Search wiki not in authorized catalog");
                        continue;
                    };
                    wiki.project = project.into();
                    if !wiki.versions.iter().any(|v| v == version)
                        || !under_mapped(git, &wiki.mapped_path)
                    {
                        result.warn("Search path/version outside published wiki");
                        continue;
                    }
                    let tk = (
                        wiki.organization.clone(),
                        wiki.id.clone(),
                        version.to_owned(),
                    );
                    if !trees.contains_key(&tk) {
                        match self.tree(&wiki, version).await {
                            Ok(tree) => {
                                trees.insert(tk.clone(), tree);
                            }
                            Err(e) => {
                                if e.to_string().contains("HTTP 401")
                                    || e.to_string().contains("HTTP 403")
                                {
                                    return Err(e);
                                }
                                result.warn("Wiki path resolution incomplete");
                                continue;
                            }
                        }
                    }
                    if let Some(path) = find_path(&trees[&tk], git) {
                        read_candidates.push((
                            wiki,
                            path.to_owned(),
                            git.to_owned(),
                            version.to_owned(),
                        ));
                    } else {
                        result.warn("Search Git path has no resolvable Wiki page link");
                    }
                }
                let count = v["count"].as_u64().unwrap_or(0) as usize;
                if hits.len() < 25 || skip + hits.len() >= count {
                    break;
                }
                if skip == 75 {
                    result.warn("Search pagination limit (100)");
                }
            }
        }
        for (wiki, path, git, version) in read_candidates {
            let mut page = match self.page(&wiki, &path, Some(&git), &version).await {
                Ok(page) => page,
                Err(e)
                    if e.to_string().contains("HTTP 401") || e.to_string().contains("HTTP 403") =>
                {
                    return Err(e);
                }
                Err(_) => {
                    result.warn("Wiki content read incomplete; unread page excluded");
                    continue;
                }
            };
            result.histories_checked += 1;
            if let Err(error) = self.attribution(&mut page).await {
                result.warn(&format!(
                    "Git attribution incomplete; author unknown: {error}"
                ));
            }
            self.entities(&mut page, &mut result).await;
            result.pages.push(page);
        }
        let mode = input.author_mode.unwrap_or(default_mode);
        if mode == AuthorMode::MineOnly {
            result.pages.retain(is_mine);
        }
        if mode == AuthorMode::PreferMine {
            // Personal preference breaks topical ties only; native search provides relevant candidates.
            let terms: Vec<_> = result
                .query
                .split_whitespace()
                .map(str::to_lowercase)
                .collect();
            result.pages.sort_by_key(|p| {
                std::cmp::Reverse((
                    terms
                        .iter()
                        .filter(|t| p.title.to_lowercase().contains(t.as_str()))
                        .count(),
                    is_mine(p),
                ))
            });
        }
        if result.pages.len() > 4 {
            result.warn("Content/context page limit (4)");
            result.pages.truncate(4);
        }
        Ok(result)
    }
}
fn is_mine(p: &Page) -> bool {
    matches!(
        p.reference.authority.as_deref(),
        Some("created_by_me" | "edited_by_me")
    )
}
fn under_mapped(path: &str, mapped: &str) -> bool {
    validate_path(path).is_ok()
        && (mapped == "/"
            || path
                .strip_prefix(mapped.trim_end_matches('/'))
                .is_some_and(|p| p.starts_with('/')))
}
fn find_path<'a>(v: &'a Value, git: &str) -> Option<&'a str> {
    if v["gitItemPath"].as_str() == Some(git) {
        return v["path"].as_str();
    }
    v["subPages"]
        .as_array()?
        .iter()
        .find_map(|p| find_path(p, git))
}
fn verify_page_url(remote: &str, wiki: &Wiki, path: &str, id: Option<u64>) -> Result<()> {
    let url = Url::parse(remote)?;
    let mut prefix = Url::parse(&wiki.organization)?;
    prefix
        .path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(&wiki.project_id)
        .extend(["_wiki", "wikis", &wiki.id]);
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("dev.azure.com")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "invalid Wiki page link origin"
    );
    let mut named = Url::parse(&wiki.organization)?;
    named
        .path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(&wiki.project)
        .extend(["_wiki", "wikis", &wiki.id]);
    let matching = [prefix.path(), named.path()].iter().any(|p| {
        url.path() == *p || id.is_some_and(|id| url.path().starts_with(&format!("{p}/{id}/")))
    });
    ensure!(matching, "Wiki link outside project/wiki/page scope");
    ensure!(
        url.query_pairs()
            .all(|(k, _)| matches!(k.as_ref(), "pagePath" | "version" | "pageId" | "anchor")),
        "unsupported Wiki link query"
    );
    let path_matches = url.query_pairs().any(|(k, v)| k == "pagePath" && v == path);
    ensure!(
        path_matches || id.is_some_and(|id| url.path().contains(&format!("/{id}/"))),
        "Wiki link does not identify returned page"
    );
    Ok(())
}
/// Search receives only the current human request, with native operators escaped by tokenization.
pub fn question_query(question: &str) -> Result<String> {
    let stop = [
        "según",
        "segun",
        "la",
        "el",
        "los",
        "las",
        "wiki",
        "wikis",
        "busca",
        "buscar",
        "en",
        "cómo",
        "como",
        "se",
        "qué",
        "que",
        "dice",
        "sobre",
        "por",
        "favor",
        "documentación",
        "documentacion",
        "de",
        "del",
        "un",
        "una",
        "y",
        "para",
        "me",
        "dime",
        "explica",
        "explicar",
        "muestra",
        "mostrar",
        "indica",
        "indicar",
        "documentado",
        "documentados",
        "configura",
        "configurar",
        "dame",
        "más",
        "mas",
        "detalles",
        "continúa",
        "continua",
        "amplía",
        "amplia",
        "eso",
    ];
    let question = question.split(['.', '\n']).next().unwrap_or(question);
    let terms: Vec<_> = question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty() && !stop.contains(&t.to_lowercase().as_str()))
        .take(20)
        .collect();
    ensure!(
        !terms.is_empty(),
        "invalid Wiki question: specify the topic"
    );
    Ok(terms.join(" ").chars().take(500).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };
    const PID: &str = "00000000-0000-0000-0000-000000000001";
    const WID: &str = "00000000-0000-0000-0000-000000000002";
    const RID: &str = "00000000-0000-0000-0000-000000000003";
    const SHA: &str = "1111111111111111111111111111111111111111";
    fn catalog(wild: bool) -> String {
        format!(
            "[[sources]]\norganization='https://dev.azure.com/test'\nprojects=['{}']\nauthor_email='me@example.test'",
            if wild { "*" } else { "Project" }
        )
    }
    fn meta(mapped: &str, kind: &str) -> Value {
        json!({"id":WID,"name":"Documentation","type":kind,"projectId":PID,"repositoryId":RID,"mappedPath":mapped,"versions":[{"version":"published"}]})
    }
    fn hit(git: &str) -> Value {
        json!({"path":git,"fileName":"doc.md","project":{"name":"Project","id":PID},"wiki":{"id":WID,"version":"published"}})
    }
    fn page_value(git: &str, canonical: &str, content: &str) -> Value {
        let mut u = Url::parse(&format!(
            "https://dev.azure.com/test/{PID}/_wiki/wikis/{WID}"
        ))
        .unwrap();
        u.query_pairs_mut().append_pair("pagePath", canonical);
        json!({"path":canonical,"gitItemPath":git,"remoteUrl":u.as_str(),"content":content,"subPages":[]})
    }
    async fn fixture(
        server: &MockServer,
        mapped: &str,
        kind: &str,
        git: &str,
        canonical: &str,
        mine: bool,
    ) {
        Mock::given(method("GET"))
            .and(path("/test/_apis/projects/Project"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"id":PID,"name":"Project"})),
            )
            .mount(server)
            .await;
        for p in [
            "/test/_apis/wiki/wikis".to_owned(),
            "/test/Project/_apis/wiki/wikis".to_owned(),
        ] {
            Mock::given(method("GET"))
                .and(path(p))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"value":[meta(mapped,kind)]})),
                )
                .mount(server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path(format!("/test/{PID}/_apis/wiki/wikis/{WID}/pages")))
            .and(query_param("recursionLevel", "full"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"path":"/","gitItemPath":"/","subPages":[page_value(git,canonical,"")]}),
            ))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/test/{PID}/_apis/wiki/wikis/{WID}/pages")))
            .and(query_param("includeContent", "true"))
            .and(query_param("path", canonical))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("etag", format!("\"{SHA}\""))
                    .set_body_json(page_value(
                        git,
                        canonical,
                        "Procedimiento vigente: validar y revisar.",
                    )),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/test/{PID}/_apis/git/repositories/{RID}/items"
            )))
            .and(query_param("versionDescriptor.version", "published"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"objectId":SHA,"commitId":SHA})),
            )
            .mount(server)
            .await;
        Mock::given(method("GET")).and(path(format!("/test/{PID}/_apis/git/repositories/{RID}/commits"))).and(query_param("searchCriteria.author","me@example.test"))
            .and(query_param("searchCriteria.itemVersion.version",SHA)).and(query_param("searchCriteria.itemVersion.versionType","commit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":if mine {vec![json!({"commitId":SHA,"parents":["parent"],"author":{"name":"Self","email":"me@example.test"}})]} else {vec![]}}))).mount(server).await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/test/{PID}/_apis/git/repositories/{RID}/commits"
            )))
            .and(|r: &wiremock::Request| {
                !r.url
                    .query_pairs()
                    .any(|(k, _)| k == "searchCriteria.author")
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"value":[{"author":{"name":"Ana","email":"ana@example.test"}}]}),
            ))
            .mount(server)
            .await;
    }
    fn reader<'a>(
        client: &'a Client,
        key: &'a str,
        catalog: &str,
        ids: &'a [String],
        server: &MockServer,
    ) -> Reader<'a> {
        let mut r = Reader::new(client, key, catalog, ids).unwrap();
        r.base = Some(server.uri());
        r
    }
    #[tokio::test]
    async fn native_search_resolves_project_and_code_paths_without_guessing_or_time_window() {
        for (mapped, kind, git, canonical) in [
            (
                "/",
                "projectWiki",
                "/Folder/My-page%25.md",
                "/Folder/My page%25",
            ),
            (
                "/docs",
                "codeWiki",
                "/docs/Despliegue-ñ/Procedimiento.md",
                "/Despliegue ñ/Procedimiento",
            ),
        ] {
            let server = MockServer::start().await;
            fixture(&server, mapped, kind, git, canonical, true).await;
            let mut unauthorized = hit(git);
            unauthorized["project"]["name"] = json!("Other");
            let mut wrong_wiki = hit(git);
            wrong_wiki["wiki"]["id"] = json!("00000000-0000-0000-0000-000000000004");
            Mock::given(method("POST")).and(path("/test/_apis/search/wikisearchresults")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"infoCode":0,"count":4,"results":[hit(git),hit(git),unauthorized,wrong_wiki]}))).expect(1).mount(&server).await;
            let client = Client::new();
            let ids = vec![WID.to_owned()];
            let r = reader(&client, "synthetic-token", &catalog(false), &ids, &server);
            let result = r
                .search(
                    &SearchInput {
                        query: "procedimiento".into(),
                        wiki_id: None,
                        author_mode: Some(AuthorMode::MineOnly),
                    },
                    AuthorMode::All,
                )
                .await
                .unwrap();
            assert_eq!(result.pages.len(), 1);
            assert_eq!(result.pages[0].path, canonical);
            assert_eq!(
                result.pages[0].reference.authority.as_deref(),
                Some("edited_by_me")
            );
            assert!(result.partial);
            let requests = server.received_requests().await.unwrap();
            assert!(requests.iter().all(|r| {
                !r.url
                    .query_pairs()
                    .any(|(k, _)| k.contains("fromDate") || k.contains("Oldest"))
            }));
            assert_eq!(
                requests
                    .iter()
                    .find(|r| r.method == "POST")
                    .unwrap()
                    .body_json::<Value>()
                    .unwrap()["filters"],
                json!({"Project":["Project"]})
            );
            assert!(
                !serde_json::to_string(&result)
                    .unwrap()
                    .contains("me@example.test")
            );
        }
    }
    #[tokio::test]
    async fn wildcard_search_does_not_enumerate_projects_and_exposes_index_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test/_apis/search/wikisearchresults"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"infoCode":1,"count":0,"results":[]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::new();
        let r = reader(&client, "synthetic", &catalog(true), &[], &server);
        let result = r
            .search(
                &SearchInput {
                    query: "deploy".into(),
                    wiki_id: None,
                    author_mode: None,
                },
                AuthorMode::All,
            )
            .await
            .unwrap();
        assert!(result.partial);
        assert!(result.pages.is_empty());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]
                .body_json::<Value>()
                .unwrap()
                .get("filters")
                .is_none()
        );
    }
    #[tokio::test]
    async fn authorship_is_page_revision_specific_and_git_failure_does_not_fake_authority() {
        for git_denied in [false, true] {
            let server = MockServer::start().await;
            fixture(
                &server,
                "/",
                "projectWiki",
                "/Procedure.md",
                "/Procedure",
                false,
            )
            .await;
            if git_denied {
                Mock::given(path(format!(
                    "/test/{PID}/_apis/git/repositories/{RID}/commits"
                )))
                .respond_with(ResponseTemplate::new(403).set_body_string("PRIVATE TOKEN body"))
                .with_priority(1)
                .mount(&server)
                .await;
            }
            let client = Client::new();
            let r = reader(&client, "synthetic", &catalog(false), &[], &server);
            let result = r
                .read(&ReadInput {
                    wiki_id: WID.into(),
                    path: "/Procedure".into(),
                })
                .await
                .unwrap();
            assert_eq!(
                result.pages[0].reference.authority.as_deref(),
                Some(if git_denied { "unknown" } else { "other" })
            );
            assert_eq!(
                result.pages[0].reference.author.as_deref(),
                if git_denied { None } else { Some("Ana") }
            );
            assert_eq!(result.partial, git_denied);
            assert!(!serde_json::to_string(&result).unwrap().contains("PRIVATE"));
        }
    }
    #[tokio::test]
    async fn verified_single_file_add_is_creation_but_import_is_only_contribution() {
        for import in [false, true] {
            let server = MockServer::start().await;
            fixture(
                &server,
                "/",
                "projectWiki",
                "/Procedure.md",
                "/Procedure",
                true,
            )
            .await;
            let changes = if import {
                vec![
                    json!({"changeType":"add","item":{"path":"/Procedure.md"}}),
                    json!({"changeType":"add","item":{"path":"/Other.md"}}),
                ]
            } else {
                vec![json!({"changeType":"add","item":{"path":"/Procedure.md"}})]
            };
            Mock::given(path(format!(
                "/test/{PID}/_apis/git/repositories/{RID}/commits/{SHA}/changes"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"changes":changes,"changeCounts":{"Add":if import {2} else {1}}}),
            ))
            .mount(&server)
            .await;
            let client = Client::new();
            let r = reader(&client, "synthetic", &catalog(false), &[], &server);
            let result = r
                .read(&ReadInput {
                    wiki_id: WID.into(),
                    path: "/Procedure".into(),
                })
                .await
                .unwrap();
            assert_eq!(
                result.pages[0].reference.authority.as_deref(),
                Some(if import {
                    "edited_by_me"
                } else {
                    "created_by_me"
                })
            );
        }
    }
    #[tokio::test]
    async fn pagination_cap_timeouts_statuses_and_large_bodies_are_explicit_and_sanitized() {
        let server = MockServer::start().await;
        let hits: Vec<_> = (0..25)
            .map(|_| {
                let mut h = hit("/doc.md");
                h["project"]["name"] = json!("Other");
                h
            })
            .collect();
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"infoCode":8,"count":120,"results":hits})),
            )
            .expect(4)
            .mount(&server)
            .await;
        let client = Client::new();
        let r = reader(&client, "synthetic", &catalog(false), &[], &server);
        let input = SearchInput {
            query: "deploy".into(),
            wiki_id: None,
            author_mode: None,
        };
        let result = r.search(&input, AuthorMode::All).await.unwrap();
        assert!(result.partial);
        assert_eq!(result.candidates, 100);
        server.reset().await;
        for status in [401, 403, 404, 429] {
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_string("PRIVATE secret token"))
                .mount(&server)
                .await;
            let e = r
                .search(&input, AuthorMode::All)
                .await
                .unwrap_err()
                .to_string();
            assert!(e.contains(&status.to_string()));
            assert!(!e.contains("PRIVATE"));
            server.reset().await;
        }
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(1_000_001)))
            .mount(&server)
            .await;
        assert!(
            r.search(&input, AuthorMode::All)
                .await
                .unwrap_err()
                .to_string()
                .contains("1 MB")
        );
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(100))
                    .set_body_json(json!({"infoCode":0,"count":0,"results":[]})),
            )
            .mount(&server)
            .await;
        let mut r = reader(&client, "synthetic", &catalog(false), &[], &server);
        r.deadline = Instant::now() + Duration::from_millis(10);
        assert!(
            r.search(&input, AuthorMode::All)
                .await
                .unwrap_err()
                .to_string()
                .contains("timeout")
        );
    }
    #[tokio::test]
    async fn unread_page_is_excluded_and_repeat_read_observes_current_revision() {
        let server = MockServer::start().await;
        fixture(
            &server,
            "/",
            "projectWiki",
            "/Procedure.md",
            "/Procedure",
            true,
        )
        .await;
        let client = Client::new();
        let r = reader(&client, "synthetic", &catalog(false), &[], &server);
        let input = ReadInput {
            wiki_id: WID.into(),
            path: "/Procedure".into(),
        };
        let first = r.read(&input).await.unwrap();
        Mock::given(path(format!("/test/{PID}/_apis/wiki/wikis/{WID}/pages")))
            .and(query_param("includeContent", "true"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("etag", "2222222222222222222222222222222222222222")
                    .set_body_json(page_value(
                        "/Procedure.md",
                        "/Procedure",
                        "Contenido actualizado.",
                    )),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        let second = r.read(&input).await.unwrap();
        assert_ne!(first.pages[0].content, second.pages[0].content);
        assert_ne!(first.pages[0].revision, second.pages[0].revision);
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"infoCode":0,"count":1,"results":[hit("/missing.md")]})),
            )
            .mount(&server)
            .await;
        let result = r
            .search(
                &SearchInput {
                    query: "missing".into(),
                    wiki_id: None,
                    author_mode: None,
                },
                AuthorMode::All,
            )
            .await
            .unwrap();
        assert!(result.pages.is_empty());
        assert!(result.partial);
    }
    #[test]
    fn paths_links_input_and_current_question_are_closed() {
        assert!(!under_mapped("/docs2/file.md", "/docs"));
        assert!(!under_mapped("/docs/../file.md", "/docs"));
        assert!(under_mapped("/docs/ñ %.md", "/docs"));
        assert!(
            serde_json::from_value::<ReadInput>(
                json!({"wiki_id":WID,"path":"/p","url":"https://evil.test"})
            )
            .is_err()
        );
        assert_eq!(
            question_query("Según la wiki, ¿cómo se configura el pipeline?").unwrap(),
            "pipeline"
        );
        assert!(question_query("busca en la wiki").is_err());
        assert!(question_query("Según la wiki, dame más detalles").is_err());
        assert_eq!(
            question_query("Según la wiki, despliegue continuo. Resume sin nombres concretos.")
                .unwrap(),
            "despliegue continuo"
        );
        let wiki:Wiki=serde_json::from_value(json!({"organization":"https://dev.azure.com/test","project":"Project","project_id":PID,"id":WID,"name":"Wiki","kind":"codeWiki","repository_id":RID,"mapped_path":"/docs","versions":["published"]})).unwrap();
        assert!(verify_page_url("https://evil.test/page", &wiki, "/p", None).is_err());
        assert!(
            verify_page_url(
                &format!("https://dev.azure.com/test/{PID}/_wiki/wikis/{WID}?pagePath=/other"),
                &wiki,
                "/p",
                None
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn page_link_must_identify_the_selected_published_version() {
        let server = MockServer::start().await;
        let client = Client::new();
        let reader = reader(&client, "token", &catalog(true), &[], &server);
        let mut wiki: Wiki = serde_json::from_value(json!({"organization":"https://dev.azure.com/test","project":"Project","project_id":PID,"id":WID,"name":"Wiki","kind":"codeWiki","repository_id":RID,"mapped_path":"/docs","versions":["published","other"]})).unwrap();
        for (version, accepted) in [
            (None, false),
            (Some("GBother"), false),
            (Some("GBpublished"), true),
        ] {
            server.reset().await;
            let mut value = page_value("/docs/Procedure.md", "/Procedure", "Current procedure");
            if let Some(version) = version {
                let mut url = Url::parse(value["remoteUrl"].as_str().unwrap()).unwrap();
                url.query_pairs_mut().append_pair("version", version);
                value["remoteUrl"] = json!(url.as_str());
            }
            Mock::given(method("GET"))
                .and(path(format!("/test/{PID}/_apis/wiki/wikis/{WID}/pages")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("etag", SHA)
                        .set_body_json(value),
                )
                .mount(&server)
                .await;
            assert_eq!(
                reader
                    .page(&wiki, "/Procedure", None, "published")
                    .await
                    .is_ok(),
                accepted
            );
        }
        wiki.versions = vec!["other".into()];
        assert!(
            reader
                .page(&wiki, "/Procedure", None, "published")
                .await
                .is_err()
        );
    }
}

#[cfg(test)]
mod entity_tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{path, query_param},
    };
    #[tokio::test]
    async fn wiki_entity_links_are_verified_inside_scope_and_stage_uses_exact_execution() {
        let server = MockServer::start().await;
        let pid = "abcdeabc-abcd-abcd-abcd-abcdeabcdea1";
        let wid = "abcdeabc-abcd-abcd-abcd-abcdeabcdea2";
        let rid = "abcdeabc-abcd-abcd-abcd-abcdeabcdea3";
        let stage = "abcdeabc-abcd-abcd-abcd-abcdeabcdea4";
        let catalog = "[[sources]]\norganization='https://dev.azure.com/test'\nprojects=['*']\nauthor_email='me@example.test'";
        Mock::given(path("/test/_apis/wiki/wikis")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[{"id":wid,"projectId":pid,"repositoryId":rid,"type":"projectWiki","mappedPath":"/","versions":[{"version":"main"}]}]}))).mount(&server).await;
        let url =
            format!("https://dev.azure.com/test/{pid}/_wiki/wikis/{wid}?pagePath=%2FProcedure");
        Mock::given(path(format!("/test/{pid}/_apis/wiki/wikis/{wid}/pages"))).and(query_param("includeContent","true")).respond_with(ResponseTemplate::new(200).insert_header("etag","1111111111111111111111111111111111111111").set_body_json(json!({"path":"/Procedure","gitItemPath":"/Procedure.md","remoteUrl":url,"content":format!("Work item https://dev.azure.com/test/{pid}/_workitems/edit/42 y padre https://dev.azure.com/test/{pid}/_workitems/edit/43. Ejecución https://dev.azure.com/test/{pid}/_build/results?buildId=40&j={stage}. Configuración https://dev.azure.com/test/{pid}/_build?definitionId=5. Fuera https://dev.azure.com/test/Other/_workitems/edit/99.")}))).mount(&server).await;
        Mock::given(path(format!("/test/_apis/projects/{pid}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"id":pid,"name":"Project"})),
            )
            .mount(&server)
            .await;
        for id in [42, 43] {
            Mock::given(path(format!("/test/{pid}/_apis/wit/workitems/{id}")))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(
                        json!({"id":id,"fields":{"System.Title":format!("Task {id}"),"System.TeamProject":"Project"}}),
                    ),
                )
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(path(format!("/test/{pid}/_apis/build/builds/40")))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"id":40,"project":{"id":pid},"definition":{"name":"Delivery"}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path(format!("/test/{pid}/_apis/build/builds/40/timeline")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"records":[{"id":stage,"type":"Stage","name":"Deploy"}]}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path(format!("/test/{pid}/_apis/build/definitions/5")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id":5,"project":{"id":pid},"name":"Delivery"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::new();
        let mut reader = Reader::new(&client, "synthetic", catalog, &[]).unwrap();
        reader.base = Some(server.uri());
        let result = reader
            .read(&ReadInput {
                wiki_id: wid.into(),
                path: "/Procedure".into(),
            })
            .await
            .unwrap();
        let refs = &result.pages[0].entities;
        assert_eq!(refs.len(), 5);
        assert!(refs.iter().any(|r| r.kind == "stage_run"
            && r.parent.is_some()
            && r.label.contains("ejecución #40")));
        assert!(
            refs.iter()
                .any(|r| r.kind == "pipeline_definition" && r.url.contains("definitionId=5"))
        );
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| !r.url.path().contains("Other"))
        );
    }
}
