//! Bounded, read-only evidence from Azure DevOps. The catalog lives in the private knowledge repo.
use anyhow::{Context, Result, ensure};
use chrono::{Duration, Utc};
use reqwest::{Client, Method};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    sources: Vec<Source>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    organization: String,
    projects: Vec<String>,
    author_email: String,
    in_progress_states: Vec<String>,
    recently_done_states: Vec<String>,
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
                source.author_email.len() <= 254 && source.author_email.contains('@'),
                "invalid ADO author email"
            );
            ensure!(
                !source.projects.is_empty() && source.projects.len() <= 10,
                "invalid ADO projects"
            );
            total += source.projects.len();
            for value in source
                .projects
                .iter()
                .chain(&source.in_progress_states)
                .chain(&source.recently_done_states)
            {
                ensure!(
                    !value.trim().is_empty()
                        && value.len() <= 120
                        && !value.chars().any(char::is_control),
                    "invalid ADO catalog value"
                );
            }
            ensure!(
                !source.in_progress_states.is_empty() && !source.recently_done_states.is_empty(),
                "ADO states required"
            );
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
fn linked_repositories(item: &Value) -> BTreeSet<String> {
    let re = regex::Regex::new(
        r"(?i)^vstfs:///Git/(?:PullRequestId|Commit)/[0-9a-f-]{36}%2f([0-9a-f-]{36})",
    )
    .unwrap();
    item["relations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["rel"] == "ArtifactLink")
        .filter_map(|r| re.captures(r["url"].as_str()?))
        .map(|c| c[1].to_ascii_lowercase())
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

pub async fn status(client: &Client, key: &str, catalog_text: &str) -> Result<String> {
    let catalog = Catalog::parse(catalog_text)?;
    let since = Utc::now() - Duration::days(14);
    let mut output = format!(
        "Fuente Azure DevOps consultada {}. Ventana de actividad reciente: últimos 14 días. Las fechas objetivo no son promesas personales; ausencia de bloqueo no demuestra que no exista.\n\n",
        Utc::now().to_rfc3339()
    );
    for source in &catalog.sources {
        for project in &source.projects {
            let active_query = "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND [System.AssignedTo] = @Me AND [System.State] <> 'Done' AND [System.State] <> 'Removido' ORDER BY [System.ChangedDate] DESC";
            let recent_query = "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND [System.ChangedBy] = @Me AND [System.ChangedDate] >= @Today - 14 ORDER BY [System.ChangedDate] DESC";
            let (active, recent_ids) = tokio::try_join!(
                wiql(client, key, source, project, active_query),
                wiql(client, key, source, project, recent_query)
            )?;
            let ids: BTreeSet<_> = active.into_iter().chain(recent_ids).take(80).collect();
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
            let mut selected: Vec<_> = items
                .iter()
                .filter(|v| {
                    field(v, "System.WorkItemType") == "Product Backlog Item"
                        && (source
                            .in_progress_states
                            .iter()
                            .any(|s| s.eq_ignore_ascii_case(field(v, "System.State")))
                            || (source
                                .recently_done_states
                                .iter()
                                .any(|s| s.eq_ignore_ascii_case(field(v, "System.State")))
                                && recent(v, since)))
                        && (belongs_to_user(v, &source.author_email)
                            || items.iter().any(|t| {
                                relation_ids(t, "System.LinkTypes.Hierarchy-Reverse")
                                    .contains(&v["id"].as_u64().unwrap_or(0))
                                    && belongs_to_user(t, &source.author_email)
                            }))
                })
                .collect();
            selected.sort_by(|a, b| {
                let active = |item: &&Value| {
                    source
                        .in_progress_states
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(field(item, "System.State")))
                };
                active(b).cmp(&active(a)).then_with(|| {
                    field(b, "System.ChangedDate").cmp(field(a, "System.ChangedDate"))
                })
            });
            selected.truncate(8);
            output.push_str(&format!(
                "Proyecto: {} / {}\n",
                source.organization.trim_end_matches('/'),
                project
            ));
            let in_progress = selected
                .iter()
                .filter(|hu| {
                    source
                        .in_progress_states
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(field(hu, "System.State")))
                })
                .count();
            if in_progress == 0 {
                output.push_str("No se encontraron HUs en desarrollo vinculadas al usuario en el alcance consultado de este proyecto; puede existir trabajo fuera de ese alcance.\n");
            } else {
                output.push_str(&format!("HUs en desarrollo vinculadas al usuario en el alcance consultado de este proyecto: {in_progress}.\n"));
            }
            if selected.is_empty() {
                output.push_str("No se encontraron HUs en desarrollo o desarrollo terminado reciente vinculadas de forma verificable al usuario.\n\n");
                continue;
            }
            let child_ids: BTreeSet<_> = selected
                .iter()
                .flat_map(|hu| relation_ids(hu, "System.LinkTypes.Hierarchy-Forward"))
                .take(100)
                .collect();
            let children = work_items(client, key, source, project, &child_ids).await?;
            let by_id: BTreeMap<u64, &Value> = items
                .iter()
                .chain(children.iter())
                .filter_map(|v| Some((v["id"].as_u64()?, v)))
                .collect();
            let mut linked_repos = BTreeSet::new();
            for hu in &selected {
                linked_repos.extend(linked_repositories(hu));
                output.push_str(&format!(
                    "HU {}. Cambiada: {}.\n",
                    label(hu),
                    field(hu, "System.ChangedDate")
                ));
                let due = field(hu, "Microsoft.VSTS.Scheduling.TargetDate");
                if !due.is_empty() {
                    output.push_str(&format!("Fecha objetivo registrada: {due}. No implica compromiso personal confirmado.\n"));
                }
                let tags = field(hu, "System.Tags");
                if tags.to_lowercase().contains("bloque") || tags.to_lowercase().contains("imped") {
                    output.push_str(&format!(
                        "Etiqueta de impedimento explícita: {}.\n",
                        tags.chars().take(160).collect::<String>()
                    ));
                } else {
                    output.push_str("No hay impedimento explícito en campos revisados; estado de impedimentos desconocido.\n");
                }
                let mut tasks: Vec<_> = relation_ids(hu, "System.LinkTypes.Hierarchy-Forward")
                    .into_iter()
                    .filter_map(|id| by_id.get(&id).copied())
                    .filter(|task| belongs_to_user(task, &source.author_email))
                    .collect();
                tasks.sort_by_key(|task| {
                    let state = field(task, "System.State");
                    if state.eq_ignore_ascii_case("En Desarrollo") {
                        0
                    } else if state.eq_ignore_ascii_case("Desarrollo Terminado") {
                        1
                    } else if state.eq_ignore_ascii_case("Done") {
                        3
                    } else if state.eq_ignore_ascii_case("Removido") {
                        4
                    } else {
                        2
                    }
                });
                if tasks.is_empty() {
                    output.push_str(
                        "Sin tareas personales relacionadas en el conjunto reciente consultado.\n",
                    );
                } else {
                    for task in tasks.into_iter().take(8) {
                        output.push_str(&format!("Tarea relacionada: {}.\n", label(task)));
                    }
                }
                if !linked_repositories(hu).is_empty() {
                    output.push_str("La HU tiene enlace a un repositorio mediante PR/commit.\n");
                }
                output.push('\n');
            }
            if !linked_repos.is_empty() {
                let from = since.to_rfc3339();
                for repo in linked_repos.iter().take(3) {
                    let metadata_url =
                        organization_url(source, &["_apis", "git", "repositories", repo])?;
                    let metadata = match request(client, key, Method::GET, metadata_url, None).await
                    {
                        Ok(v) => v,
                        Err(_) => {
                            output.push_str(
                                "No se pudo verificar el repositorio enlazado a la HU.\n",
                            );
                            continue;
                        }
                    };
                    let Some(repo_project) = metadata["project"]["name"]
                        .as_str()
                        .filter(|p| !p.is_empty())
                    else {
                        output
                            .push_str("El proyecto del repositorio enlazado no está disponible.\n");
                        continue;
                    };
                    let mut commit_ids = BTreeSet::new();
                    let mut url = project_url(
                        source,
                        repo_project,
                        "dev.azure.com",
                        &["_apis", "git", "repositories", repo, "commits"],
                    )?;
                    url.query_pairs_mut()
                        .append_pair("searchCriteria.author", &source.author_email)
                        .append_pair("searchCriteria.fromDate", &from)
                        .append_pair("searchCriteria.$top", "10");
                    let commits = match request(client, key, Method::GET, url, None).await {
                        Ok(v) => v,
                        Err(_) => {
                            output.push_str("No se pudo verificar actividad de commits del repositorio enlazado.\n");
                            continue;
                        }
                    };
                    for commit in commits["value"].as_array().into_iter().flatten().take(5) {
                        if let Some(id) = commit["commitId"].as_str() {
                            commit_ids.insert(id.to_ascii_lowercase());
                        }
                        output.push_str(&format!("Commit reciente del usuario en repositorio enlazado a HU: {} — {}. La relación exacta con la HU requiere enlace explícito.\n",commit["commitId"].as_str().unwrap_or("").chars().take(8).collect::<String>(),commit["comment"].as_str().unwrap_or("").chars().take(100).collect::<String>()));
                    }
                    let mut builds_url = project_url(
                        source,
                        repo_project,
                        "dev.azure.com",
                        &["_apis", "build", "builds"],
                    )?;
                    builds_url
                        .query_pairs_mut()
                        .append_pair("minTime", &from)
                        .append_pair("$top", "50");
                    let builds = match request(client, key, Method::GET, builds_url, None).await {
                        Ok(v) => v,
                        Err(_) => {
                            output.push_str("No se pudo verificar ejecuciones de pipelines.\n");
                            continue;
                        }
                    };
                    let mut relevant_builds = BTreeSet::new();
                    for build in builds["value"].as_array().into_iter().flatten() {
                        let build_repo = build["repository"]["id"].as_str().unwrap_or("");
                        let commit = build["sourceVersion"]
                            .as_str()
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if build_repo.eq_ignore_ascii_case(repo) && commit_ids.contains(&commit) {
                            if let Some(id) = build["id"].as_u64() {
                                relevant_builds.insert(id);
                            }
                            output.push_str(&format!("Pipeline de commit del usuario: build #{} — resultado {}, finalizado {}. La ejecución no demuestra por sí sola que hubo pruebas.\n",build["id"],build["result"].as_str().unwrap_or("desconocido"),build["finishTime"].as_str().unwrap_or("fecha desconocida")));
                        }
                    }
                    if !relevant_builds.is_empty() {
                        let mut releases_url = project_url(
                            source,
                            repo_project,
                            "vsrm.dev.azure.com",
                            &["_apis", "release", "releases"],
                        )?;
                        releases_url
                            .query_pairs_mut()
                            .append_pair("minCreatedTime", &from)
                            .append_pair("$top", "50");
                        let releases =
                            match request(client, key, Method::GET, releases_url, None).await {
                                Ok(v) => v,
                                Err(_) => {
                                    output.push_str("No se pudo verificar releases clásicos.\n");
                                    continue;
                                }
                            };
                        for release in releases["value"].as_array().into_iter().flatten() {
                            let linked =
                                release["artifacts"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .any(|a| {
                                        a["definitionReference"]["version"]["id"]
                                            .as_str()
                                            .and_then(|id| id.parse::<u64>().ok())
                                            .is_some_and(|id| relevant_builds.contains(&id))
                                    });
                            if linked {
                                output.push_str(&format!(
                                    "Release asociado al build: {} — estado {}.\n",
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
            }
            output.push_str("Compromisos personales vigentes (qué y para cuándo): no hay un compromiso personal explícito verificable en los campos consultados; requiere confirmación personal. La fecha objetivo de una HU no equivale a un compromiso personal. Riesgo de incumplimiento: no hay evaluación explícita verificable; requiere confirmación personal.\n\n");
            if output.len() > 18_000 {
                break;
            }
        }
    }
    Ok(output.chars().take(18_000).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_validates_org_and_allows_more_projects() {
        let valid = "[[sources]]\norganization='https://dev.azure.com/example/'\nprojects=['A','B']\nauthor_email='x@example.com'\nin_progress_states=['En Desarrollo']\nrecently_done_states=['Desarrollo Terminado']\n";
        assert!(Catalog::parse(valid).is_ok());
        assert!(Catalog::parse(&valid.replace("dev.azure.com", "evil.example")).is_err());
    }
    #[test]
    fn hierarchy_and_repository_links_are_extracted() {
        let item = json!({"relations":[{"rel":"System.LinkTypes.Hierarchy-Reverse","url":"https://dev.azure.com/x/_apis/wit/workItems/42"},{"rel":"ArtifactLink","url":"vstfs:///Git/PullRequestId/11111111-1111-1111-1111-111111111111%2F22222222-2222-2222-2222-222222222222%2F3"}]});
        assert_eq!(
            relation_ids(&item, "System.LinkTypes.Hierarchy-Reverse"),
            vec![42]
        );
        assert!(linked_repositories(&item).contains("22222222-2222-2222-2222-222222222222"));
    }
}
