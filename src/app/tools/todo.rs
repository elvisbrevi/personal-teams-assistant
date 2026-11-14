//! Host adapters for the typed To-Do tools. Reads work without a running Teams assistant.
use super::super::{DesktopState, fail, github};
use crate::store::settings::SettingsStore;
use crate::{
    knowledge::Access,
    security,
    state::Store,
    tools::{
        ToolSpec,
        todo::{DetailRequest, Filters},
    },
};
use serde_json::Value;

/// The enabled Azure source the implementation writes through: its read key, catalog and the
/// user's work item guides.
struct Azure {
    client: reqwest::Client,
    read_key: String,
    catalog: String,
    guides: Vec<crate::work_items::Guide>,
}
fn azure(state: &DesktopState, source: Option<&str>) -> anyhow::Result<Azure> {
    let config = state.settings.config()?;
    let map = state.settings.map()?;
    let sources: Vec<_> = map
        .resources
        .iter()
        .filter(|r| {
            r.enabled
                && source.is_none_or(|id| r.id == id)
                && matches!(
                    &r.access,
                    Access::Tool {
                        tool: ToolSpec::AzureDevopsStatus { .. }
                    }
                )
        })
        .collect();
    anyhow::ensure!(sources.len() == 1, "Select one enabled Azure source.");
    let Access::Tool {
        tool: tool @ ToolSpec::AzureDevopsStatus { secret_ref, .. },
    } = &sources[0].access
    else {
        unreachable!()
    };
    Ok(Azure {
        read_key: security::resolve(secret_ref, &config.secrets)?,
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        catalog: tool.catalog(&map.repositories)?,
        guides: map.work_item_guides,
    })
}

/// The enabled Azure source whose catalog names an organization (`https://dev.azure.com/NAME`):
/// the source a read of that organization needs when several are enabled. `None` when at most
/// one is enabled (a read then picks it, or says there is none); a refusal when several are and
/// none, or more than one, names the organization.
pub(in crate::app) fn source_for(
    state: &DesktopState,
    organization: &str,
) -> Result<Option<String>, String> {
    #[derive(serde::Deserialize)]
    struct Named {
        organization: String,
    }
    #[derive(serde::Deserialize)]
    struct Catalog {
        sources: Vec<Named>,
    }
    let map = state.settings.map().map_err(super::super::fail)?;
    let enabled: Vec<_> = map
        .resources
        .iter()
        .filter(|r| r.enabled)
        .filter_map(|r| match &r.access {
            Access::Tool {
                tool: tool @ ToolSpec::AzureDevopsStatus { .. },
            } => Some((r.id.as_str(), tool)),
            _ => None,
        })
        .collect();
    if enabled.len() <= 1 {
        return Ok(None);
    }
    let wanted = crate::workflows::organization_name(organization);
    let holders: Vec<&str> = enabled
        .iter()
        .filter(|(_, tool)| {
            tool.catalog(&map.repositories)
                .ok()
                .and_then(|text| toml::from_str::<Catalog>(&text).ok())
                .is_some_and(|c| {
                    c.sources
                        .iter()
                        .any(|s| crate::workflows::organization_name(&s.organization) == wanted)
                })
        })
        .map(|(id, _)| *id)
        .collect();
    match holders.as_slice() {
        [one] => Ok(Some((*one).to_owned())),
        [] => Err(format!(
            "[invalid_input] No enabled Azure DevOps source names the organization {organization}."
        )),
        _ => Err(format!(
            "[invalid_input] Several enabled Azure DevOps sources name the organization {organization}; keep one enabled."
        )),
    }
}

/// One read of a project of the enabled Azure source, with its read credential and the To-Do
/// cache, bounded to `seconds`: `what` names it in the errors, `purpose` in the refusal when no
/// single source is enabled. A project outside the source's configured organizations is
/// `[invalid_input] invalid project: …`.
async fn project_read<T, F, Fut>(
    state: &DesktopState,
    source: Option<&str>,
    purpose: &str,
    what: &str,
    seconds: u64,
    read: F,
) -> Result<T, String>
where
    F: FnOnce(Azure, Store) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let azure = azure(state, source).map_err(|_| {
        format!(
            "[not_ready] Enable one Azure DevOps source with its read credential before {purpose}."
        )
    })?;
    let config = state.settings.config().map_err(fail)?;
    let cache = Store::open(&config.server.data_dir.join("todo-cache.db")).map_err(fail)?;
    tokio::time::timeout(
        std::time::Duration::from_secs(seconds),
        read(azure, cache),
    )
    .await
    .map_err(|_| format!("[network] The project's {what} could not be read in time."))?
    .map_err(|error| {
        let text = error.to_string();
        if text.starts_with("invalid") {
            format!("[invalid_input] {text}")
        } else if text.contains("the repository catalog would be incomplete") {
            text
        } else {
            format!("[network] The project's {what} could not be read; check the catalog and the Azure read credential.")
        }
    })
}

/// The Git repositories of a project of the enabled Azure source (`workflow_repositories`),
/// read with its read credential and cached like the process metadata.
pub(in crate::app) async fn repositories(
    state: &DesktopState,
    source: Option<&str>,
    organization: Option<&str>,
    project: &str,
) -> Result<Value, String> {
    project_read(
        state,
        source,
        "listing a project's repositories",
        "Git repositories",
        110,
        |azure, cache| async move {
            crate::ado::repositories::list(
                &azure.client,
                &azure.read_key,
                &azure.catalog,
                &cache,
                organization,
                project,
            )
            .await
        },
    )
    .await
}

/// The workflow states of a project's requirement, bug and listed task types
/// (`workflow_settings` and its update): the ready states are chosen from them. Read with the
/// enabled Azure source's read credential and cached like the process metadata.
pub(in crate::app) async fn process_states(
    state: &DesktopState,
    source: Option<&str>,
    organization: Option<&str>,
    project: &str,
) -> Result<Value, String> {
    project_read(
        state,
        source,
        "choosing a project's ready states",
        "process states",
        60,
        |azure, cache| async move {
            crate::ado::todo::process_states(
                &azure.client,
                &azure.read_key,
                &azure.catalog,
                &cache,
                organization,
                project,
                &azure.guides,
            )
            .await
        },
    )
    .await
}

/// Check a reviewed child against the provider's live types before anything is written.
pub(in crate::app) async fn check_child(
    state: &DesktopState,
    provider: &str,
    source: Option<&str>,
    parent: &crate::tools::todo::Detail,
    child: &Value,
) -> anyhow::Result<()> {
    if provider == "github" {
        return Ok(());
    }
    let azure = azure(state, source)?;
    crate::ado::implementation::check_child(
        &azure.client,
        &azure.read_key,
        &azure.catalog,
        &azure.guides,
        parent,
        child,
    )
    .await
}

pub(in crate::app) async fn create_child(
    state: &DesktopState,
    provider: &str,
    source: Option<&str>,
    parent: &crate::tools::todo::Detail,
    child: &Value,
) -> anyhow::Result<Value> {
    if provider == "github" {
        let _guard = state.github_api.lock().await;
        return github::create_child(parent, child).await;
    }
    let azure = azure(state, source)?;
    let write_session = super::super::azure_cli::require().await?;
    crate::ado::implementation::create_child(
        &azure.client,
        &azure.read_key,
        write_session.token(),
        &azure.catalog,
        &azure.guides,
        parent,
        child,
    )
    .await
}

pub(in crate::app) async fn read(
    state: &DesktopState,
    method: &str,
    args: &Value,
) -> Result<Value, String> {
    let task_query: Option<crate::tools::tasks::Query> = if method == "list_azure_tasks" {
        let query: crate::tools::tasks::Query = serde_json::from_value(args.clone())
            .map_err(|_| "[invalid_input] Invalid daily task arguments.".to_owned())?;
        query
            .validate()
            .map_err(|e| format!("[invalid_input] {e}"))?;
        Some(query)
    } else {
        None
    };
    let github_read = matches!(
        method,
        "list_github_epics" | "list_github_issues" | "get_github_epic" | "get_github_issue"
    );
    let detail: Option<DetailRequest> = if method.starts_with("get_") {
        let args: DetailRequest = serde_json::from_value(args.clone())
            .map_err(|_| "[invalid_input] Invalid work item arguments.".to_owned())?;
        args.validate(github_read)
            .map_err(|e| format!("[invalid_input] {e}"))?;
        Some(args)
    } else {
        None
    };
    let filters: Filters = match &detail {
        Some(args) => Filters {
            source: args.source.clone(),
            project: args.project.clone(),
            ..Default::default()
        },
        None if task_query.is_some() => Filters {
            source: task_query.as_ref().unwrap().source.clone(),
            project: task_query.as_ref().unwrap().project.clone(),
            ..Default::default()
        },
        None => serde_json::from_value(args.clone())
            .map_err(|_| "[invalid_input] Invalid To-Do filters.".to_owned())?,
    };
    filters
        .validate()
        .map_err(|e| format!("[invalid_input] {e}"))?;
    let result = if github_read {
        if security::secret_source("GITHUB_OAUTH_TOKENS")
            .map_err(fail)?
            .is_none()
        {
            return Err("[not_ready] Connect GitHub in Knowledge before listing epics.".into());
        }
        let _guard = state.github_api.lock().await;
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            match &detail {
                Some(args) if method == "get_github_issue" => serde_json::to_value(github::issue(args).await?),
                Some(args) => serde_json::to_value(github::epic(args).await?),
                None if method == "list_github_issues" => serde_json::to_value(github::issues(&filters).await?),
                None => serde_json::to_value(github::epics(&filters).await?),
            }.map_err(anyhow::Error::from)
        }).await.map_err(|_| "[network] GitHub Epic read timed out.".to_owned())?
            .map_err(|error| if error.to_string().starts_with("invalid") { format!("[invalid_input] {error}") } else { "[network] GitHub epics could not be read; check the connection and Issues read access.".into() })?
    } else {
        let config = state.settings.config().map_err(fail)?;
        let map = state.settings.map().map_err(fail)?;
        let sources: Vec<_> = map
            .resources
            .iter()
            .filter(|r| {
                matches!(
                    r.access,
                    Access::Tool {
                        tool: ToolSpec::AzureDevopsStatus { .. }
                    }
                )
            })
            .filter(|r| filters.source.as_ref().map_or(r.enabled, |id| &r.id == id))
            .collect();
        let source = match sources.as_slice() {
            [] => {
                return Err(
                    "[not_ready] Configure an Azure DevOps activity source before reading work items."
                        .into(),
                );
            }
            [source] => source,
            _ => return Err("[invalid_input] Select an Azure DevOps source.".into()),
        };
        if !source.enabled {
            return Err(
                "[not_ready] Enable the selected Azure DevOps source before reading work items."
                    .into(),
            );
        }
        let Access::Tool {
            tool: tool @ ToolSpec::AzureDevopsStatus { secret_ref, .. },
        } = &source.access
        else {
            unreachable!()
        };
        let catalog = tool.catalog(&map.repositories).map_err(fail)?;
        let key = security::resolve(secret_ref, &config.secrets).map_err(|_| {
            "[not_ready] Azure read credential unavailable; inspect credentials list.".to_owned()
        })?;
        let cache = Store::open(&config.server.data_dir.join("todo-cache.db")).map_err(fail)?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(fail)?;
        tokio::time::timeout(std::time::Duration::from_secs(110), async {
            if let Some(query) = &task_query {
                let mut query = query.clone();
                query.source = Some(source.id.clone());
                serde_json::to_value(crate::ado::tasks::list(&client, &key, &catalog, &cache, &query, &config.activity_registration).await?).map_err(anyhow::Error::from)
            } else if method == "get_azure_task" {
                serde_json::to_value(crate::ado::detail::get_task(&client, &key, &catalog, &cache, detail.as_ref().unwrap()).await?).map_err(anyhow::Error::from)
            } else if method == "get_azure_item" {
                // Internal (the workflow director): an HU, bug or task in one read.
                serde_json::to_value(crate::ado::detail::get_any(&client, &key, &catalog, &cache, detail.as_ref().unwrap()).await?).map_err(anyhow::Error::from)
            } else {
            match &detail {
                Some(args) => serde_json::to_value(crate::ado::detail::get(&client, &key, &catalog, &cache, args).await?),
                None => serde_json::to_value(crate::ado::todo::list(&client, &key, &catalog, &cache, &filters, &map.work_item_guides).await?),
            }.map_err(anyhow::Error::from)
            }
        }).await.map_err(|_| "[network] Azure work item read timed out.".to_owned())?
            .map_err(|error| if error.to_string().starts_with("invalid") { format!("[invalid_input] {error}") } else { "[network] Azure work items could not be read; check the catalog, team membership and read credential.".into() })?
    };
    Ok(result)
}

/// The pull requests of one repository whose source branch a coding session reported, read
/// with the enabled Azure source's read credential (the workflow engine; never cached, nothing
/// written).
pub(in crate::app) async fn pull_requests(
    state: &DesktopState,
    source: Option<&str>,
    organization: &str,
    project: &str,
    repository: (&str, &str),
    branch: &str,
) -> Result<Vec<crate::ado::workflow::PullRequest>, String> {
    project_read(
        state,
        source,
        "reading a workflow's pull requests",
        "pull requests",
        60,
        |azure, _cache| async move {
            crate::ado::workflow::pull_requests(
                &azure.client,
                &azure.read_key,
                &azure.catalog,
                organization,
                project,
                repository,
                branch,
            )
            .await
        },
    )
    .await
}

/// Whether a reported branch exists in a workflow repository now, read with the enabled Azure
/// source's read credential (the workflow supervisor; never cached, nothing written).
pub(in crate::app) async fn branch_exists(
    state: &DesktopState,
    source: Option<&str>,
    organization: &str,
    project: &str,
    repository: &str,
    branch: &str,
) -> Result<bool, String> {
    project_read(
        state,
        source,
        "reading a workflow's branches",
        "branches",
        60,
        |azure, _cache| async move {
            crate::ado::workflow::branch_exists(
                &azure.client,
                &azure.read_key,
                &azure.catalog,
                organization,
                project,
                repository,
                branch,
            )
            .await
        },
    )
    .await
}

/// Whether an item read fresh is assigned to the enabled Azure source's user.
pub(in crate::app) fn assigned(
    state: &DesktopState,
    source: Option<&str>,
    detail: &crate::tools::todo::Detail,
) -> Result<bool, String> {
    let azure = azure(state, source).map_err(|_| {
        "[not_ready] Enable one Azure DevOps source with its read credential before starting a workflow.".to_owned()
    })?;
    crate::ado::workflow::assigned(&azure.catalog, detail)
        .map_err(|e| format!("[invalid_input] {e}"))
}
