//! Host adapters for the typed To-Do tools. Reads work without a running Teams assistant.
use super::super::{DesktopState, fail, github, read_config};
use crate::{
    knowledge::{Access, KnowledgeMap},
    security,
    state::Store,
    tools::{
        ToolSpec,
        todo::{DetailRequest, Filters},
    },
};
use serde_json::Value;

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
    let config = read_config(&state.config_path)?;
    let map = KnowledgeMap::load(&state.map_path)?;
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
    let read_key = security::resolve(secret_ref, &config.secrets)?;
    let write_key = security::secret("AZURE_IMPLEMENTATION_WRITE_TOKEN")?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    crate::ado::implementation::create_child(
        &client,
        &read_key,
        &write_key,
        &tool.catalog(&map.repositories)?,
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
        let config = read_config(&state.config_path).map_err(fail)?;
        let map = KnowledgeMap::load(&state.map_path).map_err(fail)?;
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
            } else {
            match &detail {
                Some(args) => serde_json::to_value(crate::ado::detail::get(&client, &key, &catalog, &cache, args).await?),
                None => serde_json::to_value(crate::ado::todo::list(&client, &key, &catalog, &cache, &filters).await?),
            }.map_err(anyhow::Error::from)
            }
        }).await.map_err(|_| "[network] Azure work item read timed out.".to_owned())?
            .map_err(|error| if error.to_string().starts_with("invalid") { format!("[invalid_input] {error}") } else { "[network] Azure work items could not be read; check the catalog, team membership and read credential.".into() })?
    };
    Ok(result)
}
