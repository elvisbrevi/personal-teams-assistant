//! Bounded, read-only evidence from Azure DevOps. The catalog lives in the private knowledge repo.
pub mod link;
pub mod review;
pub mod wiki;
use crate::evidence::{Block, Evidence, Reference};
use crate::state::Store;
use anyhow::{Context, Result, ensure};
use chrono::{Duration, Utc};
use reqwest::{Client, Method};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use url::Url;

const COLD_REPO_SECONDS: i64 = 6 * 3600;
const ACTIVE_REPO_SECONDS: i64 = 5 * 60;

#[derive(Clone)]
struct CommitRef {
    repo_id: String,
    sha: String,
    date: String,
    message: String,
}

async fn cached_read(
    store: &Store,
    cache_key: &str,
    ttl: i64,
    read: impl std::future::Future<Output = Result<Value>>,
) -> Result<Value> {
    if let Some((value, updated_at)) = store.activity_cache(cache_key)?
        && Utc::now().timestamp() - updated_at < ttl
    {
        return Ok(serde_json::from_str(&value)?);
    }
    let value = read.await?;
    store.save_activity_cache(cache_key, &serde_json::to_string(&value)?)?;
    Ok(value)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    sources: Vec<Source>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    organization: String,
    projects: Vec<String>,
    author_email: String,
}
impl Catalog {
    fn parse(text: &str) -> Result<Self> {
        let catalog: Self = toml::from_str(text)?;
        ensure!(
            !catalog.sources.is_empty() && catalog.sources.len() <= 5,
            "invalid ADO source count"
        );
        let mut total = 0;
        for source in &catalog.sources {
            let url = Url::parse(&source.organization)?;
            ensure!(
                url.scheme() == "https"
                    && url.host_str() == Some("dev.azure.com")
                    && url.port_or_known_default() == Some(443)
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url
                        .path_segments()
                        .is_some_and(|p| p.filter(|s| !s.is_empty()).count() == 1),
                "ADO organization must be a dev.azure.com organization URL"
            );
            ensure!(
                source.author_email.len() <= 254
                    && source.author_email.contains('@')
                    && source
                        .author_email
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || ".@_+-".contains(c)),
                "invalid ADO author email"
            );
            ensure!(
                !source.projects.is_empty()
                    && source.projects.len() <= 10
                    && (source.projects.len() == 1 || !source.projects.iter().any(|p| p == "*")),
                "invalid ADO projects"
            );
            total += source.projects.len();
            for value in &source.projects {
                ensure!(
                    !value.trim().is_empty()
                        && value.len() <= 120
                        && !value.chars().any(char::is_control),
                    "invalid ADO catalog value"
                );
            }
        }
        ensure!(total <= 10, "too many ADO projects");
        Ok(catalog)
    }
}

fn project_url(source: &Source, project: &str, host: &str, segments: &[&str]) -> Result<Url> {
    let mut url = Url::parse(&source.organization)?;
    url.set_host(Some(host))?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid ADO URL"))?;
        path.pop_if_empty();
        path.push(project);
        for segment in segments {
            path.push(segment);
        }
    }
    url.query_pairs_mut().append_pair("api-version", "7.1");
    Ok(url)
}
fn organization_url(source: &Source, segments: &[&str]) -> Result<Url> {
    let mut url = Url::parse(&source.organization)?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid ADO URL"))?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    url.query_pairs_mut().append_pair("api-version", "7.1");
    Ok(url)
}
async fn request(
    client: &Client,
    key: &str,
    method: Method,
    url: Url,
    body: Option<Value>,
) -> Result<Value> {
    let endpoint = url.path().to_owned();
    let mut request = client.request(method, url).basic_auth("", Some(key));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    ensure!(
        response.status().is_success(),
        "Azure DevOps read {} returned HTTP {}",
        endpoint,
        response.status().as_u16()
    );
    crate::adapters::bounded_json(response, 1_000_000).await
}
async fn wiql(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    query: &str,
) -> Result<Vec<u64>> {
    let mut url = project_url(source, project, "dev.azure.com", &["_apis", "wit", "wiql"])?;
    url.query_pairs_mut().append_pair("$top", "80");
    let value = request(client, key, Method::POST, url, Some(json!({"query":query}))).await?;
    Ok(value["workItems"]
        .as_array()
        .context("invalid WIQL response")?
        .iter()
        .filter_map(|v| v["id"].as_u64())
        .collect())
}
async fn projects(client: &Client, key: &str, source: &Source) -> Result<Vec<String>> {
    if source.projects != ["*"] {
        return Ok(source.projects.clone());
    }
    let mut names = Vec::new();
    for skip in (0..1000).step_by(100) {
        let mut url = organization_url(source, &["_apis", "projects"])?;
        url.query_pairs_mut()
            .append_pair("$top", "100")
            .append_pair("$skip", &skip.to_string());
        let value = request(client, key, Method::GET, url, None).await?;
        let page = value["value"].as_array().context("invalid ADO projects")?;
        names.extend(
            page.iter()
                .filter_map(|p| p["name"].as_str().map(str::to_owned)),
        );
        if page.len() < 100 {
            return Ok(names);
        }
    }
    anyhow::bail!("ADO organization has more than 1000 projects; project coverage incomplete")
}
async fn work_items(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    ids: &BTreeSet<u64>,
) -> Result<Vec<Value>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "wit", "workitems"],
    )?;
    url.query_pairs_mut()
        .append_pair(
            "ids",
            &ids.iter().map(u64::to_string).collect::<Vec<_>>().join(","),
        )
        .append_pair("$expand", "Relations")
        .append_pair("errorPolicy", "Omit");
    let value = request(client, key, Method::GET, url, None).await?;
    Ok(value["value"]
        .as_array()
        .context("invalid ADO work items")?
        .iter()
        .filter(|item| {
            let project = item["fields"]["System.TeamProject"].as_str().unwrap_or("");
            !project.is_empty()
                && (source.projects == ["*"]
                    || source
                        .projects
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(project)))
        })
        .cloned()
        .collect())
}
fn field<'a>(item: &'a Value, name: &str) -> &'a str {
    item["fields"][name].as_str().unwrap_or("")
}
fn identity(item: &Value, name: &str) -> String {
    item["fields"][name]["uniqueName"]
        .as_str()
        .or_else(|| item["fields"][name].as_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}
fn belongs_to_user(item: &Value, email: &str) -> bool {
    let email = email.to_ascii_lowercase();
    identity(item, "System.AssignedTo") == email || identity(item, "System.ChangedBy") == email
}
fn relation_ids(item: &Value, relation: &str) -> Vec<u64> {
    item["relations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["rel"].as_str() == Some(relation))
        .filter_map(|r| r["url"].as_str()?.rsplit('/').next()?.parse().ok())
        .collect()
}
fn recent(item: &Value, since: chrono::DateTime<Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(field(item, "System.ChangedDate"))
        .is_ok_and(|d| d.with_timezone(&Utc) >= since)
}
fn label(item: &Value) -> String {
    let title: String = field(item, "System.Title").chars().take(120).collect();
    format!(
        "#{} {} — {}",
        item["id"],
        title,
        field(item, "System.State")
    )
}

/// A repository the account cannot read answers 404: the user cannot have pushed there, so
/// it is not a gap in their activity.
fn unreadable(error: &anyhow::Error) -> bool {
    error.to_string().ends_with("returned HTTP 404")
}

fn same_user(identity: &Value, email: &str) -> bool {
    identity["uniqueName"]
        .as_str()
        .or_else(|| identity["mailAddress"].as_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(email))
}

/// Days of activity a request asks about: «hoy»/today 1, «ayer»/yesterday 2, this week 7,
/// this month 30, «últimos N días»/last N days (at most 31); two weeks otherwise.
pub fn recent_window(question: &str) -> i64 {
    let q = question
        .to_lowercase()
        .replace('á', "a")
        .replace('é', "e")
        .replace('í', "i")
        .replace('ó', "o")
        .replace('ú', "u");
    let count = |unit: &str| {
        regex::Regex::new(&format!(r"\b(\d{{1,2}})\s*{unit}"))
            .ok()?
            .captures(&q)?[1]
            .parse::<i64>()
            .ok()
    };
    if let Some(days) = count("(?:dias|days)\\b") {
        return days.clamp(1, 31);
    }
    if let Some(weeks) = count("(?:semanas|weeks)\\b") {
        return (weeks * 7).clamp(1, 31);
    }
    let words: Vec<&str> = q.split(|c: char| !c.is_alphanumeric()).collect();
    let phrase = format!(" {} ", words.join(" "));
    let has = |options: &[&str]| options.iter().any(|o| phrase.contains(&format!(" {o} ")));
    if has(&["hoy", "today"]) {
        1
    } else if has(&["ayer", "yesterday"]) {
        2
    } else if has(&[
        "esta semana",
        "ultima semana",
        "semana pasada",
        "this week",
        "last week",
        "past week",
    ]) {
        7
    } else if has(&[
        "este mes",
        "ultimo mes",
        "mes pasado",
        "this month",
        "last month",
        "past month",
    ]) {
        30
    } else {
        14
    }
}

fn changed_excerpt(before: &str, after: &str) -> String {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let mut start = 0;
    while start < old.len() && start < new.len() && old[start] == new[start] {
        start += 1;
    }
    let mut old_end = old.len();
    let mut new_end = new.len();
    while old_end > start && new_end > start && old[old_end - 1] == new[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }
    let old_part = old[start..old_end]
        .iter()
        .take(12)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let new_part = new[start..new_end]
        .iter()
        .take(12)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Before (from line {}): {}. After: {}",
        start + 1,
        old_part.chars().take(600).collect::<String>(),
        new_part.chars().take(900).collect::<String>()
    )
}

fn bounded_lines(text: &str, limit: usize) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if out.chars().count() + line.chars().count() + 1 > limit {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

async fn file_content(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    repo: &str,
    path: &str,
    sha: &str,
) -> Result<String> {
    let mut url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "git", "repositories", repo, "items"],
    )?;
    url.query_pairs_mut()
        .append_pair("path", path)
        .append_pair("versionDescriptor.version", sha)
        .append_pair("versionDescriptor.versionType", "commit")
        .append_pair("includeContent", "true")
        .append_pair("$format", "json");
    let item = request(client, key, Method::GET, url, None).await?;
    Ok(item["content"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(100_000)
        .collect())
}

pub(super) struct EntityLink {
    pub kind: String,
    pub id: String,
    pub project: String,
    pub url: String,
    pub stage: Option<String>,
}
/// Interpret only known UI routes in the already authorized organization/project.
pub(super) fn entity_link(raw: &str, organization: &str, projects: &[&str]) -> Option<EntityLink> {
    let url = Url::parse(raw.trim_end_matches(['.', ',', ';', '!'])).ok()?;
    let org = Url::parse(organization).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("dev.azure.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let project = projects.iter().find(|project| {
        let mut prefix = org.clone();
        prefix
            .path_segments_mut()
            .unwrap()
            .pop_if_empty()
            .push(project);
        url.path()
            .starts_with(&format!("{}/", prefix.path().trim_end_matches('/')))
    })?;
    let segments: Vec<_> = url.path_segments()?.collect();
    let (kind, id) = if segments.get(2) == Some(&"_workitems")
        && segments.get(3) == Some(&"edit")
        && segments.len() == 5
    {
        ("work_item", segments[4].to_owned())
    } else if segments.get(2) == Some(&"_build")
        && segments.get(3) == Some(&"results")
        && segments.len() == 4
    {
        (
            "pipeline_run",
            url.query_pairs()
                .find(|(k, _)| k == "buildId")?
                .1
                .into_owned(),
        )
    } else if segments.get(2) == Some(&"_build") && segments.len() == 3 {
        (
            "pipeline_definition",
            url.query_pairs()
                .find(|(k, _)| k == "definitionId")?
                .1
                .into_owned(),
        )
    } else {
        return None;
    };
    if id.parse::<u64>().ok().is_none_or(|id| id == 0) {
        return None;
    }
    let stage = url
        .query_pairs()
        .find(|(k, _)| k == "j")
        .map(|(_, v)| v.into_owned())
        .filter(|v| uuid::Uuid::parse_str(v).is_ok());
    let mut canonical = org.clone();
    canonical
        .path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(project);
    match kind {
        "work_item" => {
            canonical
                .path_segments_mut()
                .unwrap()
                .extend(["_workitems", "edit", &id]);
        }
        "pipeline_run" => {
            canonical
                .path_segments_mut()
                .unwrap()
                .extend(["_build", "results"]);
            canonical.query_pairs_mut().append_pair("buildId", &id);
        }
        _ => {
            canonical.path_segments_mut().unwrap().push("_build");
            canonical.query_pairs_mut().append_pair("definitionId", &id);
        }
    }
    Some(EntityLink {
        kind: kind.into(),
        id,
        project: (*project).into(),
        url: canonical.into(),
        stage,
    })
}

pub async fn team_references(
    client: &Client,
    key: &str,
    catalog: &str,
    messages: &[crate::evidence::TeamsMessage],
) -> Result<Vec<Reference>> {
    let catalog = Catalog::parse(catalog)?;
    let urls = regex::Regex::new(r"https://dev\.azure\.com/[^\s<>)\]]+")?;
    let mut refs = Vec::new();
    let mut seen = BTreeSet::new();
    for message in messages {
        for raw in urls.find_iter(&message.text).take(8) {
            for source in &catalog.sources {
                let url = Url::parse(raw.as_str())?;
                let mut org = Url::parse(&source.organization)?;
                org.set_query(None);
                if url.host_str() != Some("dev.azure.com")
                    || url
                        .path_segments()
                        .and_then(|mut p| p.next())
                        .map(str::to_owned)
                        != org
                            .path_segments()
                            .and_then(|mut p| p.next())
                            .map(str::to_owned)
                {
                    continue;
                }
                let project = if source.projects == ["*"] {
                    let encoded = url.path_segments().and_then(|mut p| p.nth(1)).unwrap_or("");
                    // URL path '+' is literal, unlike query-form decoding.
                    url::form_urlencoded::parse(
                        format!("p={}", encoded.replace('+', "%2B")).as_bytes(),
                    )
                    .next()
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default()
                } else {
                    let Some(p) = source.projects.iter().find(|p| {
                        entity_link(raw.as_str(), &source.organization, &[p.as_str()]).is_some()
                    }) else {
                        continue;
                    };
                    p.clone()
                };
                let Some(entity) = entity_link(raw.as_str(), &source.organization, &[&project])
                else {
                    continue;
                };
                if !seen.insert(entity.url.clone()) || seen.len() > 16 {
                    continue;
                }
                let segments = match entity.kind.as_str() {
                    "work_item" => vec!["_apis", "wit", "workitems", &entity.id],
                    "pipeline_run" => vec!["_apis", "build", "builds", &entity.id],
                    _ => vec!["_apis", "build", "definitions", &entity.id],
                };
                let v = match request(
                    client,
                    key,
                    Method::GET,
                    project_url(source, &project, "dev.azure.com", &segments)?,
                    None,
                )
                .await
                {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if !v["id"]
                    .as_u64()
                    .is_some_and(|id| id.to_string() == entity.id)
                {
                    continue;
                }
                if entity.kind == "work_item" {
                    let actual = field(&v, "System.TeamProject");
                    if actual.is_empty()
                        || (source.projects != ["*"]
                            && !source
                                .projects
                                .iter()
                                .any(|p| p.eq_ignore_ascii_case(actual)))
                    {
                        continue;
                    }
                    let Ok(project_data) = request(
                        client,
                        key,
                        Method::GET,
                        organization_url(source, &["_apis", "projects", &project])?,
                        None,
                    )
                    .await
                    else {
                        continue;
                    };
                    if !project_data["name"]
                        .as_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case(actual))
                    {
                        continue;
                    }
                } else if !v["project"]["name"]
                    .as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&project))
                    && v["project"]["id"].as_str() != Some(&project)
                {
                    continue;
                }
                let title = if entity.kind == "work_item" {
                    field(&v, "System.Title")
                } else {
                    v["name"]
                        .as_str()
                        .or_else(|| v["definition"]["name"].as_str())
                        .unwrap_or("Pipeline")
                };
                let reference = Reference {
                    id: format!(
                        "{}:{}:{project}:{}",
                        entity.kind, source.organization, entity.id
                    ),
                    kind: entity.kind,
                    label: format!("{title} #{}", entity.id),
                    url: entity.url,
                    organization: source.organization.clone(),
                    project: project.clone(),
                    aliases: vec![format!("#{}", entity.id), title.into()],
                    parent: None,
                    revision: None,
                    authority: None,
                    author: None,
                    author_role: None,
                };
                reference.validate()?;
                refs.push(reference);
            }
        }
    }
    Ok(refs)
}

#[allow(clippy::too_many_arguments)] // A captured reference keeps scope, parent and revision explicit.
fn artifact(
    source: &Source,
    project: &str,
    kind: &str,
    identity: &str,
    label: String,
    aliases: Vec<String>,
    segments: &[&str],
    query: &[(&str, String)],
    parent: Option<String>,
    revision: Option<String>,
) -> Result<Reference> {
    let mut url = project_url(source, project, "dev.azure.com", segments)?;
    url.set_query(None);
    for (key, value) in query {
        url.query_pairs_mut().append_pair(key, value);
    }
    let r = Reference {
        id: format!("{kind}:{}:{project}:{identity}", source.organization),
        kind: kind.into(),
        label,
        url: url.into(),
        organization: source.organization.clone(),
        project: project.into(),
        aliases,
        parent,
        revision,
        authority: None,
        author: None,
        author_role: None,
    };
    r.validate()?;
    Ok(r)
}
fn item_reference(source: &Source, project: &str, item: &Value) -> Result<Reference> {
    let id = item["id"]
        .as_u64()
        .context("missing work item ID")?
        .to_string();
    let actual_project = field(item, "System.TeamProject");
    ensure!(
        !actual_project.is_empty()
            && (source.projects == ["*"]
                || source
                    .projects
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(actual_project))),
        "work item outside authorized project"
    );
    let _ = project;
    artifact(
        source,
        actual_project,
        "work_item",
        &id,
        label(item),
        vec![
            format!("#{id}"),
            format!("HU {id}"),
            format!("work item {id}"),
            field(item, "System.Title").to_owned(),
        ],
        &["_workitems", "edit", &id],
        &[],
        None,
        None,
    )
}

async fn commit_detail(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    commit: &CommitRef,
) -> Result<(String, Vec<Reference>)> {
    let mut references = Vec::new();
    let segments = [
        "_apis",
        "git",
        "repositories",
        commit.repo_id.as_str(),
        "commits",
        commit.sha.as_str(),
    ];
    let detail = request(
        client,
        key,
        Method::GET,
        project_url(source, project, "dev.azure.com", &segments)?,
        None,
    )
    .await?;
    let parent = detail["parents"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str);
    let mut changes_url = project_url(
        source,
        project,
        "dev.azure.com",
        &[
            "_apis",
            "git",
            "repositories",
            &commit.repo_id,
            "commits",
            &commit.sha,
            "changes",
        ],
    )?;
    changes_url.query_pairs_mut().append_pair("top", "100");
    let changes = request(client, key, Method::GET, changes_url, None).await?;
    let mut output = format!(
        "Commit detail {} ({}): {}. ",
        &commit.sha[..commit.sha.len().min(8)],
        commit.date,
        commit.message.chars().take(220).collect::<String>()
    );
    let mut files = 0;
    let mut changed: Vec<_> = changes["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    changed.sort_by_key(|c| {
        let p = c["item"]["path"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase();
        if p.contains("pipeline")
            || p.contains("stage")
            || p.ends_with(".yml")
            || p.ends_with(".yaml")
        {
            0
        } else {
            1
        }
    });
    let stages = regex::Regex::new(r"(?m)^\s*-?\s*stage:\s*([A-Za-z0-9_-]+)")?;
    for change in changed {
        let path = change["item"]["path"].as_str().unwrap_or("");
        let lower = path.to_ascii_lowercase();
        if path.is_empty()
            || change["item"]["gitObjectType"].as_str() != Some("blob")
            || [
                ".env",
                "secret",
                "credential",
                "private",
                ".pem",
                ".key",
                "password",
            ]
            .iter()
            .any(|x| lower.contains(x))
        {
            continue;
        }
        let kind = change["changeType"].as_str().unwrap_or("change");
        output.push_str(&format!(
            "File {}: {}. ",
            path.chars().take(160).collect::<String>(),
            kind
        ));
        if matches!(kind, "edit" | "add" | "rename")
            && let Ok(after) = file_content(
                client,
                key,
                source,
                project,
                &commit.repo_id,
                path,
                &commit.sha,
            )
            .await
            && !after.is_empty()
        {
            let before = if let Some(parent) = parent {
                let old_path = change["originalPath"].as_str().unwrap_or(path);
                file_content(
                    client,
                    key,
                    source,
                    project,
                    &commit.repo_id,
                    old_path,
                    parent,
                )
                .await
                .unwrap_or_default()
            } else {
                String::new()
            };
            if path.ends_with(".yml") || path.ends_with(".yaml") {
                for stage in stages.captures_iter(&after).take(10) {
                    let name = &stage[1];
                    references.push(artifact(
                        source,
                        project,
                        "stage_configuration",
                        &format!("{}:{}:{path}:{name}", commit.repo_id, commit.sha),
                        format!("Stage {name}, configuration versioned in {path}"),
                        vec![name.into()],
                        &["_git", &commit.repo_id],
                        &[
                            ("path", path.into()),
                            ("version", format!("GC{}", commit.sha)),
                        ],
                        None,
                        Some(commit.sha.clone()),
                    )?);
                }
            }
            output.push_str(&format!(
                "Content change: {}. ",
                changed_excerpt(&before, &after)
            ));
        }
        files += 1;
        if files >= 3 {
            break;
        }
    }
    if let Some(pr_id) = commit
        .message
        .strip_prefix("Merged PR ")
        .and_then(|s| s.split(':').next())
        .and_then(|s| s.trim().parse::<u64>().ok())
    {
        let mut url = project_url(
            source,
            project,
            "dev.azure.com",
            &[
                "_apis",
                "git",
                "repositories",
                &commit.repo_id,
                "pullrequests",
                &pr_id.to_string(),
            ],
        )?;
        url.query_pairs_mut()
            .append_pair("includeWorkItemRefs", "true");
        if let Ok(pr) = request(client, key, Method::GET, url, None).await {
            output.push_str(&format!(
                "PR #{pr_id}: {} (status {}). ",
                pr["title"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(160)
                    .collect::<String>(),
                pr["status"].as_str().unwrap_or("unknown")
            ));
            let ids: BTreeSet<u64> = pr["workItemRefs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|w| w["id"].as_str()?.parse().ok())
                .take(5)
                .collect();
            if let Ok(items) = work_items(client, key, source, project, &ids).await {
                for item in items {
                    references.push(item_reference(source, project, &item)?);
                    output.push_str(&format!(
                        "Related work item {} {}. ",
                        field(&item, "System.WorkItemType"),
                        label(&item)
                    ));
                }
            }
        }
    }
    Ok((output.chars().take(1200).collect(), references))
}

async fn work_item_activity(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    since: chrono::DateTime<Utc>,
) -> Result<(String, usize, bool, Vec<Reference>)> {
    let email = &source.author_email;
    let active_query = format!(
        "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND [System.AssignedTo] = '{email}' AND [System.State] <> 'Done' AND [System.State] <> 'Removido' ORDER BY [System.ChangedDate] DESC"
    );
    let recent_query = format!(
        "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND [System.ChangedBy] = '{email}' AND [System.ChangedDate] >= '{}' ORDER BY [System.ChangedDate] DESC",
        since.format("%Y-%m-%d")
    );
    let (active, recent_ids) = tokio::try_join!(
        wiql(client, key, source, project, &active_query),
        wiql(client, key, source, project, &recent_query)
    )?;
    let ids: BTreeSet<_> = recent_ids.into_iter().chain(active).take(80).collect();
    let mut items = work_items(client, key, source, project, &ids).await?;
    let parent_ids: BTreeSet<_> = items
        .iter()
        .flat_map(|v| relation_ids(v, "System.LinkTypes.Hierarchy-Reverse"))
        .take(40)
        .collect();
    let missing_parents: BTreeSet<_> = parent_ids
        .into_iter()
        .filter(|id| !ids.contains(id))
        .collect();
    items.extend(work_items(client, key, source, project, &missing_parents).await?);
    let by_id: BTreeMap<u64, &Value> = items
        .iter()
        .filter_map(|v| Some((v["id"].as_u64()?, v)))
        .collect();
    let mut selected: Vec<_> = items
        .iter()
        .filter(|item| recent(item, since) && belongs_to_user(item, email))
        .collect();
    selected.sort_by(|a, b| field(b, "System.ChangedDate").cmp(field(a, "System.ChangedDate")));
    selected.truncate(8);
    let mut output = String::new();
    let mut references = Vec::new();
    let mut blocked = false;
    for item in &selected {
        references.push(item_reference(source, project, item)?);
        output.push_str(&format!(
            "Work item {} {} (last recorded change {}). ",
            field(item, "System.WorkItemType"),
            label(item),
            field(item, "System.ChangedDate")
        ));
        if let Some(parent) = relation_ids(item, "System.LinkTypes.Hierarchy-Reverse")
            .first()
            .and_then(|id| by_id.get(id))
        {
            references.push(item_reference(source, project, parent)?);
            output.push_str(&format!("Parent: {}. ", label(parent)));
        }
        let due = field(item, "Microsoft.VSTS.Scheduling.TargetDate");
        if !due.is_empty() {
            output.push_str(&format!(
                "Recorded target date: {due}; not a personal commitment. "
            ));
        }
        let tags = field(item, "System.Tags");
        if ["bloque", "imped", "block"]
            .iter()
            .any(|w| tags.to_lowercase().contains(w))
        {
            blocked = true;
            output.push_str(&format!(
                "Tagged impediment: {}. ",
                tags.chars().take(100).collect::<String>()
            ));
        }
        output.push('\n');
    }
    Ok((output, selected.len(), blocked, references))
}

async fn repository_activity(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    since: chrono::DateTime<Utc>,
    store: &Store,
) -> Result<(
    String,
    usize,
    BTreeSet<(String, String)>,
    Vec<CommitRef>,
    bool,
)> {
    let url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "git", "repositories"],
    )?;
    let scope = format!("{}|{}|{project}", source.organization, source.author_email);
    let value = cached_read(
        store,
        &format!("ado-repos|{scope}"),
        COLD_REPO_SECONDS,
        request(client, key, Method::GET, url, None),
    )
    .await?;
    let repos = value["value"]
        .as_array()
        .context("invalid ADO repositories")?;
    let mut output = String::new();
    let mut count = 0;
    let mut commit_ids = BTreeSet::new();
    let mut commit_refs = Vec::new();
    let mut incomplete = false;
    for repo in repos {
        if repo["isDisabled"].as_bool() == Some(true) {
            continue;
        }
        let (Some(id), Some(name)) = (repo["id"].as_str(), repo["name"].as_str()) else {
            continue;
        };
        let mut url = project_url(
            source,
            project,
            "dev.azure.com",
            &["_apis", "git", "repositories", id, "commits"],
        )?;
        url.query_pairs_mut()
            .append_pair("searchCriteria.author", &source.author_email)
            .append_pair(
                "searchCriteria.fromDate",
                &(Utc::now() - Duration::days(15)).to_rfc3339(),
            )
            .append_pair("searchCriteria.$top", "10");
        let cache_key = format!("ado-commits|{scope}|{id}");
        let previous = store.activity_cache(&cache_key)?;
        let active = previous.as_ref().is_some_and(|(value, _)| {
            serde_json::from_str::<Value>(value)
                .ok()
                .and_then(|v| v["value"].as_array().map(|a| !a.is_empty()))
                .unwrap_or(false)
        });
        let ttl = if active {
            ACTIVE_REPO_SECONDS
        } else {
            COLD_REPO_SECONDS
        };
        let commits = match cached_read(
            store,
            &cache_key,
            ttl,
            request(client, key, Method::GET, url, None),
        )
        .await
        {
            Ok(commits) => commits,
            Err(error) => {
                incomplete |= !unreadable(&error);
                continue;
            }
        };
        for commit in commits["value"]
            .as_array()
            .context("invalid ADO commits")?
            .iter()
            .take(5)
        {
            if !commit["author"]["email"]
                .as_str()
                .is_some_and(|e| e.eq_ignore_ascii_case(&source.author_email))
            {
                continue;
            }
            let Some(sha) = commit["commitId"].as_str() else {
                continue;
            };
            let date = commit["author"]["date"].as_str().unwrap_or("");
            if !chrono::DateTime::parse_from_rfc3339(date)
                .is_ok_and(|d| d.with_timezone(&Utc) >= since)
            {
                continue;
            }
            commit_ids.insert((id.to_ascii_lowercase(), sha.to_ascii_lowercase()));
            count += 1;
            let message = commit["comment"]
                .as_str()
                .unwrap_or("no message")
                .lines()
                .next()
                .unwrap_or("");
            commit_refs.push(CommitRef {
                repo_id: id.into(),
                sha: sha.into(),
                date: date.into(),
                message: message.into(),
            });
            output.push_str(&format!(
                "Own commit in {name}: {} ({}): {}.\n",
                sha.chars().take(8).collect::<String>(),
                date,
                message.chars().take(160).collect::<String>()
            ));
        }
    }
    Ok((output, count, commit_ids, commit_refs, incomplete))
}

async fn pipeline_activity(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    since: chrono::DateTime<Utc>,
    commit_ids: &BTreeSet<(String, String)>,
) -> Result<(String, usize, Vec<Reference>)> {
    let mut url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "build", "builds"],
    )?;
    url.query_pairs_mut()
        .append_pair("minTime", &since.to_rfc3339())
        .append_pair("queryOrder", "queueTimeDescending")
        .append_pair("$top", "100");
    let value = request(client, key, Method::GET, url, None).await?;
    let mut output = String::new();
    let mut references = Vec::new();
    let mut count = 0;
    let mut build_ids = BTreeSet::new();
    for build in value["value"].as_array().context("invalid ADO builds")? {
        let repo = build["repository"]["id"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase();
        let sha = build["sourceVersion"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase();
        let personal_commit = commit_ids.contains(&(repo, sha));
        let requested_by = same_user(&build["requestedBy"], &source.author_email);
        let requested_for = same_user(&build["requestedFor"], &source.author_email);
        if !personal_commit && !requested_by && !requested_for {
            continue;
        }
        let Some(id) = build["id"].as_u64() else {
            continue;
        };
        let name = build["definition"]["name"].as_str().unwrap_or("unnamed");
        let run = artifact(
            source,
            project,
            "pipeline_run",
            &id.to_string(),
            format!("Pipeline {name}, run #{id}"),
            vec![name.into(), format!("build #{id}")],
            &["_build", "results"],
            &[("buildId", id.to_string())],
            None,
            None,
        )?;
        references.push(run.clone());
        count += 1;
        build_ids.insert(id);
        let relationship = if requested_by {
            "started by the user"
        } else if requested_for {
            "run on behalf of the user"
        } else {
            "for an own commit"
        };
        output.push_str(&format!(
            "Pipeline {} (build #{id}, {relationship}): result {}, date {}.\n",
            build["definition"]["name"]
                .as_str()
                .unwrap_or("unnamed")
                .chars()
                .take(100)
                .collect::<String>(),
            build["result"]
                .as_str()
                .or_else(|| build["status"].as_str())
                .unwrap_or("unknown"),
            build["finishTime"]
                .as_str()
                .or_else(|| build["queueTime"].as_str())
                .unwrap_or("date unavailable")
        ));
        if count <= 5 {
            let timeline_url = project_url(
                source,
                project,
                "dev.azure.com",
                &["_apis", "build", "builds", &id.to_string(), "timeline"],
            )?;
            if let Ok(timeline) = request(client, key, Method::GET, timeline_url, None).await {
                for stage in timeline["records"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|record| record["type"].as_str() == Some("Stage"))
                    .take(10)
                {
                    if stage["id"].as_str().is_none() {
                        continue;
                    }
                    if let Some(stage_id) = stage["id"].as_str() {
                        let name = stage["name"].as_str().unwrap_or("unnamed");
                        references.push(artifact(
                            source,
                            project,
                            "stage_run",
                            &format!("{id}:{stage_id}"),
                            format!("Stage {name}, in run #{id}"),
                            vec![name.into()],
                            &["_build", "results"],
                            &[("buildId", id.to_string())],
                            Some(run.id.clone()),
                            None,
                        )?);
                    }
                    output.push_str(&format!(
                        "Stage {}: {}.\n",
                        stage["name"]
                            .as_str()
                            .unwrap_or("unnamed")
                            .chars()
                            .take(80)
                            .collect::<String>(),
                        stage["result"]
                            .as_str()
                            .or_else(|| stage["state"].as_str())
                            .unwrap_or("unknown")
                    ));
                }
            }
        }
        if count >= 10 {
            break;
        }
    }
    if !build_ids.is_empty() {
        let mut releases_url = project_url(
            source,
            project,
            "vsrm.dev.azure.com",
            &["_apis", "release", "releases"],
        )?;
        releases_url
            .query_pairs_mut()
            .append_pair("minCreatedTime", &since.to_rfc3339())
            .append_pair("$top", "50");
        if let Ok(releases) = request(client, key, Method::GET, releases_url, None).await {
            for release in releases["value"].as_array().into_iter().flatten() {
                let linked = release["artifacts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|a| {
                        a["definitionReference"]["version"]["id"]
                            .as_str()
                            .and_then(|v| v.parse::<u64>().ok())
                            .is_some_and(|id| build_ids.contains(&id))
                    });
                if linked {
                    output.push_str(&format!(
                        "Release {} for an own build: status {}.\n",
                        release["name"]
                            .as_str()
                            .unwrap_or("unnamed")
                            .chars()
                            .take(80)
                            .collect::<String>(),
                        release["status"].as_str().unwrap_or("unknown")
                    ));
                }
            }
        }
    }
    Ok((output, count, references))
}

async fn release_definition_activity(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    since: chrono::DateTime<Utc>,
) -> Result<(String, usize, Vec<Reference>)> {
    let mut url = project_url(
        source,
        project,
        "vsrm.dev.azure.com",
        &["_apis", "release", "definitions"],
    )?;
    url.query_pairs_mut().append_pair("$top", "100");
    let list = request(client, key, Method::GET, url, None).await?;
    let mut output = String::new();
    let mut references = Vec::new();
    let mut count = 0;
    for definition in list["value"]
        .as_array()
        .context("invalid release definitions")?
    {
        let date = definition["modifiedOn"].as_str().unwrap_or("");
        if !chrono::DateTime::parse_from_rfc3339(date).is_ok_and(|d| d.with_timezone(&Utc) >= since)
            || !same_user(&definition["modifiedBy"], &source.author_email)
        {
            continue;
        }
        let Some(id) = definition["id"].as_u64() else {
            continue;
        };
        let detail = request(
            client,
            key,
            Method::GET,
            project_url(
                source,
                project,
                "vsrm.dev.azure.com",
                &["_apis", "release", "definitions", &id.to_string()],
            )?,
            None,
        )
        .await?;
        let revision = detail["revision"].as_u64().unwrap_or(0);
        let name = detail["name"].as_str().unwrap_or("unnamed");
        let definition = artifact(
            source,
            project,
            "pipeline_definition",
            &id.to_string(),
            format!("Release definition {name} #{id}"),
            vec![name.into(), format!("release #{id}")],
            &["_release"],
            &[
                ("definitionId", id.to_string()),
                ("_a", "definition-tasks".into()),
            ],
            None,
            Some(revision.to_string()),
        )?;
        references.push(definition.clone());
        output.push_str(&format!(
            "Release definition #{} {} (changed by the user {}, revision {}). ",
            id,
            detail["name"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>(),
            date,
            revision
        ));
        if revision == 1 {
            output.push_str("It is the first recorded revision of this definition. ");
        }
        for stage in detail["environments"]
            .as_array()
            .into_iter()
            .flatten()
            .take(4)
        {
            if stage["id"].as_u64().is_none() {
                continue;
            }
            if let Some(stage_id) = stage["id"].as_u64() {
                let name = stage["name"].as_str().unwrap_or("unnamed");
                references.push(artifact(
                    source,
                    project,
                    "stage_configuration",
                    &format!("{id}:{stage_id}:{revision}"),
                    format!("Stage {name}, configured in release #{id}"),
                    vec![name.into()],
                    &["_release"],
                    &[
                        ("definitionId", id.to_string()),
                        ("_a", "definition-tasks".into()),
                    ],
                    Some(definition.id.clone()),
                    Some(revision.to_string()),
                )?);
            }
            output.push_str(&format!(
                "Configured stage {}. ",
                stage["name"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(100)
                    .collect::<String>()
            ));
            for phase in stage["deployPhases"]
                .as_array()
                .into_iter()
                .flatten()
                .take(3)
            {
                output.push_str(&format!(
                    "Phase {}. ",
                    phase["name"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(100)
                        .collect::<String>()
                ));
                for task in phase["workflowTasks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|t| t["enabled"].as_bool() == Some(true))
                    .take(5)
                {
                    output.push_str(&format!(
                        "Configured task: {}. ",
                        task["name"]
                            .as_str()
                            .unwrap_or("")
                            .chars()
                            .take(100)
                            .collect::<String>()
                    ));
                }
            }
        }
        output.push('\n');
        count += 1;
        if count >= 3 {
            break;
        }
    }
    Ok((output, count, references))
}

pub async fn status(
    client: &Client,
    key: &str,
    catalog_text: &str,
    question: &str,
    store: Arc<Store>,
) -> Result<Evidence> {
    let catalog = Catalog::parse(catalog_text)?;
    let days = recent_window(question);
    let since = Utc::now() - Duration::days(days);
    let mut sections = Vec::new();
    let mut references = Vec::new();
    let mut blocked = false;
    let mut partial_projects = 0;
    let mut jobs = tokio::task::JoinSet::new();
    for source in &catalog.sources {
        for project in projects(client, key, source).await? {
            let client = client.clone();
            let key = key.to_owned();
            let source = source.clone();
            let store = store.clone();
            jobs.spawn(async move {
                let (items, commits, definitions) = tokio::join!(
                    work_item_activity(&client, &key, &source, &project, since),
                    repository_activity(&client, &key, &source, &project, since, &store),
                    release_definition_activity(&client, &key, &source, &project, since)
                );
                let incomplete = items.is_err() || commits.is_err();
                let (items, item_count, item_blocked, mut refs) = items.unwrap_or_default();
                let (commits, commit_count, commit_ids, mut commit_refs, repo_incomplete) =
                    commits.unwrap_or_default();
                let pipelines =
                    pipeline_activity(&client, &key, &source, &project, since, &commit_ids).await;
                let incomplete = incomplete || repo_incomplete || pipelines.is_err();
                let (pipelines, pipeline_count, pipeline_refs) = pipelines.unwrap_or_default();
                let (definitions, definition_count, definition_refs) =
                    definitions.unwrap_or_default();
                refs.extend(pipeline_refs);
                refs.extend(definition_refs);
                commit_refs.sort_by(|a, b| b.date.cmp(&a.date));
                let mut details = String::new();
                for commit in commit_refs.iter().take(4) {
                    if let Ok((detail, detail_refs)) =
                        commit_detail(&client, &key, &source, &project, commit).await
                    {
                        refs.extend(detail_refs);
                        details.push_str(&detail);
                        details.push('\n');
                    }
                }
                let score = item_count + commit_count + pipeline_count + definition_count;
                (
                    project,
                    score,
                    format!("{commits}{details}{definitions}{pipelines}{items}"),
                    item_blocked,
                    incomplete,
                    refs,
                )
            });
            if jobs.len() >= 8 {
                let (project, score, content, item_blocked, incomplete, refs) = jobs
                    .join_next()
                    .await
                    .context("ADO project worker missing")??;
                if score > 0 {
                    sections.push((
                        score,
                        format!("Project: {project}.\n{}", bounded_lines(&content, 5_000)),
                        refs,
                    ));
                }
                blocked |= item_blocked;
                partial_projects += usize::from(incomplete);
            }
        }
    }
    while let Some(result) = jobs.join_next().await {
        let (project, score, content, item_blocked, incomplete, refs) = result?;
        if score > 0 {
            sections.push((
                score,
                format!("Project: {project}.\n{}", bounded_lines(&content, 5_000)),
                refs,
            ));
        }
        blocked |= item_blocked;
        partial_projects += usize::from(incomplete);
    }
    sections.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut output = format!(
        "Azure DevOps activity since {} (last {days} days).\n",
        since.format("%Y-%m-%d")
    );
    let mut blocks = Vec::new();
    if sections.is_empty() {
        output.push_str("No recent own activity could be verified in the projects read.\n");
    } else {
        for (_, section, refs) in sections {
            let project_header = section.lines().next().unwrap_or("");
            for line in section.lines().skip(1) {
                let lower = line.to_lowercase();
                let mut ids: Vec<String> = refs
                    .iter()
                    .filter(|r| {
                        r.aliases
                            .iter()
                            .any(|a| !a.is_empty() && lower.contains(&a.to_lowercase()))
                    })
                    .map(|r| r.id.clone())
                    .collect();
                let parents: Vec<String> = refs
                    .iter()
                    .filter(|r| ids.contains(&r.id))
                    .filter_map(|r| r.parent.clone())
                    .collect();
                ids.extend(parents);
                blocks.push(Block {
                    text: format!("{project_header}\n{line}"),
                    source_ids: ids,
                });
            }
            references.extend(refs);
            output.push_str(&section);
            output.push('\n');
        }
    }
    if ["imped", "blocker", "blocked"]
        .iter()
        .any(|w| question.to_lowercase().contains(w))
        && !blocked
    {
        output.push_str("Impediments: no explicit blocker was found in the activity read; the actual state needs the user's confirmation.\n");
    }
    if partial_projects > 0 {
        output.push_str(&format!("Partial coverage: at least one read failed in {partial_projects} project(s); do not infer that there was no activity there.\n"));
    }
    blocks.push(Block {
        text: output
            .lines()
            .filter(|l| {
                l.starts_with("Impediments:")
                    || l.starts_with("Partial coverage:")
                    || l.starts_with("Azure DevOps activity")
                    || l.starts_with("No recent own activity")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        source_ids: Vec::new(),
    });
    let mut ids = BTreeSet::new();
    references.retain(|r| ids.insert(r.id.clone()));
    Ok(Evidence {
        text: bounded_lines(&output, 11_000),
        blocks,
        references,
        partial: partial_projects > 0,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_excerpt_identifies_the_actual_edit() {
        let before = "stage: prod\nword: Excento\nrun: false\n";
        let after = "stage: prod\nword: Exento\nrun: false\n";
        let excerpt = changed_excerpt(before, after);
        assert!(excerpt.contains("Excento"));
        assert!(excerpt.contains("Exento"));
        assert!(!excerpt.contains("run: false"));
    }
    #[tokio::test]
    async fn activity_index_uses_fresh_commits_without_a_network_call() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("activity.db")).unwrap();
        store
            .save_activity_cache("repo", "{\"value\":[{\"commitId\":\"abc\"}]}")
            .unwrap();
        let result = cached_read(&store, "repo", 300, async {
            anyhow::bail!("network should not be called")
        })
        .await
        .unwrap();
        assert_eq!(result["value"][0]["commitId"], "abc");
    }
    #[test]
    fn catalog_validates_org_and_allows_more_projects() {
        let valid = "[[sources]]\norganization='https://dev.azure.com/example/'\nprojects=['A','B']\nauthor_email='x@example.com'\n";
        assert!(Catalog::parse(valid).is_ok());
        assert!(Catalog::parse(&valid.replace("dev.azure.com", "evil.example")).is_err());
    }
    #[test]
    fn hierarchy_and_personal_build_identity_are_extracted() {
        let item = json!({"relations":[{"rel":"System.LinkTypes.Hierarchy-Reverse","url":"https://dev.azure.com/x/_apis/wit/workItems/42"}]});
        assert_eq!(
            relation_ids(&item, "System.LinkTypes.Hierarchy-Reverse"),
            vec![42]
        );
        assert!(same_user(
            &json!({"uniqueName":"USER@example.com"}),
            "user@example.com"
        ));
        assert!(!same_user(
            &json!({"uniqueName":"someone@example.com"}),
            "user@example.com"
        ));
    }
    #[test]
    fn weekly_question_and_wildcard_project_scope() {
        assert_eq!(recent_window("¿Qué hice esta semana?"), 7);
        assert_eq!(recent_window("¿Qué hice la última semana?"), 7);
        assert_eq!(recent_window("Estado de mis proyectos"), 14);
        assert_eq!(recent_window("What did I do this week?"), 7);
        assert_eq!(recent_window("¿Qué hice hoy?"), 1);
        assert_eq!(recent_window("trabajo de ayer sin registrar"), 2);
        assert_eq!(recent_window("¿qué hice este mes sin tarea?"), 30);
        assert_eq!(recent_window("los últimos 3 días"), 3);
        assert_eq!(recent_window("last 90 days"), 31);
        assert_eq!(recent_window("las últimas 3 semanas"), 21);
        assert_eq!(recent_window("¿qué hice en la semana 40?"), 14);
        let catalog = "[[sources]]\norganization='https://dev.azure.com/example/'\nprojects=['*']\nauthor_email='x@example.com'\n";
        assert!(Catalog::parse(catalog).is_ok());
        assert!(Catalog::parse(&catalog.replace("['*']", "['*','A']")).is_err());
    }
}
