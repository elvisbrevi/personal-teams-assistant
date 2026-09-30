//! Bounded, read-only evidence from Azure DevOps. The catalog lives in the private knowledge repo.
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
        .clone())
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

fn same_user(identity: &Value, email: &str) -> bool {
    identity["uniqueName"]
        .as_str()
        .or_else(|| identity["mailAddress"].as_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(email))
}

pub(crate) fn recent_window(question: &str) -> i64 {
    let q = question.to_lowercase();
    if q.contains("esta semana") || q.contains("última semana") || q.contains("ultima semana") {
        7
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
        "Antes (desde línea {}): {}. Después: {}",
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

async fn commit_detail(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    commit: &CommitRef,
) -> Result<String> {
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
        "Detalle del commit {} ({}): {}. ",
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
        let kind = change["changeType"].as_str().unwrap_or("cambio");
        output.push_str(&format!(
            "Archivo {}: {}. ",
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
            output.push_str(&format!(
                "Cambio de contenido: {}. ",
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
                "PR #{pr_id}: {} (estado {}). ",
                pr["title"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(160)
                    .collect::<String>(),
                pr["status"].as_str().unwrap_or("desconocido")
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
                    output.push_str(&format!(
                        "Work item relacionado {} {}. ",
                        field(&item, "System.WorkItemType"),
                        label(&item)
                    ));
                }
            }
        }
    }
    Ok(output.chars().take(1200).collect())
}

async fn work_item_activity(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    since: chrono::DateTime<Utc>,
) -> Result<(String, usize, bool)> {
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
    let mut blocked = false;
    for item in &selected {
        output.push_str(&format!(
            "Work item {} {} (última modificación registrada {}). ",
            field(item, "System.WorkItemType"),
            label(item),
            field(item, "System.ChangedDate")
        ));
        if let Some(parent) = relation_ids(item, "System.LinkTypes.Hierarchy-Reverse")
            .first()
            .and_then(|id| by_id.get(id))
        {
            output.push_str(&format!("Contexto padre: {}. ", label(parent)));
        }
        let due = field(item, "Microsoft.VSTS.Scheduling.TargetDate");
        if !due.is_empty() {
            output.push_str(&format!(
                "Fecha objetivo registrada: {due}; no es compromiso personal. "
            ));
        }
        let tags = field(item, "System.Tags");
        if tags.to_lowercase().contains("bloque") || tags.to_lowercase().contains("imped") {
            blocked = true;
            output.push_str(&format!(
                "Impedimento etiquetado: {}. ",
                tags.chars().take(100).collect::<String>()
            ));
        }
        output.push('\n');
    }
    Ok((output, selected.len(), blocked))
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
            Err(_) => {
                incomplete = true;
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
                .unwrap_or("sin mensaje")
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
                "Commit personal en {name}: {} ({}): {}.\n",
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
) -> Result<(String, usize)> {
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
        count += 1;
        build_ids.insert(id);
        let relationship = if requested_by {
            "iniciado por el usuario"
        } else if requested_for {
            "ejecutado a nombre del usuario"
        } else {
            "asociado a commit personal"
        };
        output.push_str(&format!(
            "Pipeline {} (build #{id}, {relationship}): resultado {}, fecha {}.\n",
            build["definition"]["name"]
                .as_str()
                .unwrap_or("sin nombre")
                .chars()
                .take(100)
                .collect::<String>(),
            build["result"]
                .as_str()
                .or_else(|| build["status"].as_str())
                .unwrap_or("desconocido"),
            build["finishTime"]
                .as_str()
                .or_else(|| build["queueTime"].as_str())
                .unwrap_or("fecha no disponible")
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
                    output.push_str(&format!(
                        "Etapa {}: {}.\n",
                        stage["name"]
                            .as_str()
                            .unwrap_or("sin nombre")
                            .chars()
                            .take(80)
                            .collect::<String>(),
                        stage["result"]
                            .as_str()
                            .or_else(|| stage["state"].as_str())
                            .unwrap_or("desconocida")
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
                        "Release {} asociado a build personal: estado {}.\n",
                        release["name"]
                            .as_str()
                            .unwrap_or("sin nombre")
                            .chars()
                            .take(80)
                            .collect::<String>(),
                        release["status"].as_str().unwrap_or("desconocido")
                    ));
                }
            }
        }
    }
    Ok((output, count))
}

async fn release_definition_activity(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    since: chrono::DateTime<Utc>,
) -> Result<(String, usize)> {
    let mut url = project_url(
        source,
        project,
        "vsrm.dev.azure.com",
        &["_apis", "release", "definitions"],
    )?;
    url.query_pairs_mut().append_pair("$top", "100");
    let list = request(client, key, Method::GET, url, None).await?;
    let mut output = String::new();
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
        output.push_str(&format!(
            "Definición de release #{} {} (modificada por el usuario {}, revisión {}). ",
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
            output.push_str("Es la primera revisión registrada de esta definición. ");
        }
        for stage in detail["environments"]
            .as_array()
            .into_iter()
            .flatten()
            .take(4)
        {
            output.push_str(&format!(
                "Stage configurado {}. ",
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
                    "Fase {}. ",
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
                        "Tarea configurada: {}. ",
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
    Ok((output, count))
}

pub async fn status(
    client: &Client,
    key: &str,
    catalog_text: &str,
    question: &str,
    store: Arc<Store>,
) -> Result<String> {
    let catalog = Catalog::parse(catalog_text)?;
    let days = recent_window(question);
    let since = Utc::now() - Duration::days(days);
    let mut sections = Vec::new();
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
                let (items, item_count, item_blocked) = items.unwrap_or_default();
                let (commits, commit_count, commit_ids, mut commit_refs, repo_incomplete) =
                    commits.unwrap_or_default();
                let pipelines =
                    pipeline_activity(&client, &key, &source, &project, since, &commit_ids).await;
                let incomplete = incomplete || repo_incomplete || pipelines.is_err();
                let (pipelines, pipeline_count) = pipelines.unwrap_or_default();
                let (definitions, definition_count) = definitions.unwrap_or_default();
                commit_refs.sort_by(|a, b| b.date.cmp(&a.date));
                let mut details = String::new();
                for commit in commit_refs.iter().take(4) {
                    if let Ok(detail) =
                        commit_detail(&client, &key, &source, &project, commit).await
                    {
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
                )
            });
            if jobs.len() >= 8 {
                let (project, score, content, item_blocked, incomplete) =
                    jobs.join_next()
                        .await
                        .context("ADO project worker missing")??;
                if score > 0 {
                    sections.push((
                        score,
                        format!("Proyecto: {project}.\n{}", bounded_lines(&content, 5_000)),
                    ));
                }
                blocked |= item_blocked;
                partial_projects += usize::from(incomplete);
            }
        }
    }
    while let Some(result) = jobs.join_next().await {
        let (project, score, content, item_blocked, incomplete) = result?;
        if score > 0 {
            sections.push((
                score,
                format!("Proyecto: {project}.\n{}", bounded_lines(&content, 5_000)),
            ));
        }
        blocked |= item_blocked;
        partial_projects += usize::from(incomplete);
    }
    sections.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut output = format!(
        "Actividad de Azure DevOps desde {} (últimos {days} días).\n",
        since.format("%Y-%m-%d")
    );
    if sections.is_empty() {
        output.push_str(
            "No se pudo verificar actividad personal reciente en los proyectos consultados.\n",
        );
    } else {
        for (_, section) in sections {
            output.push_str(&section);
            output.push('\n');
        }
    }
    if question.to_lowercase().contains("imped") && !blocked {
        output.push_str("Impedimentos: no se encontró un bloqueo explícito en la actividad recuperada; el estado real requiere confirmación personal.\n");
    }
    if partial_projects > 0 {
        output.push_str(&format!("Cobertura parcial: falló al menos una consulta en {partial_projects} proyecto(s); no inferir ausencia de actividad en ellos.\n"));
    }
    Ok(bounded_lines(&output, 11_000))
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
        let catalog = "[[sources]]\norganization='https://dev.azure.com/example/'\nprojects=['*']\nauthor_email='x@example.com'\n";
        assert!(Catalog::parse(catalog).is_ok());
        assert!(Catalog::parse(&catalog.replace("['*']", "['*','A']")).is_err());
    }
}
