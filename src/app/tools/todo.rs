//! Host adapters for the typed To-Do tools. Reads work without a running Teams assistant.
use super::super::{DesktopState, fail, github, read_config};
use crate::{
    knowledge::{Access, KnowledgeMap},
    security,
    state::Store,
    tools::{ToolSpec, todo::Filters},
};
use serde_json::Value;

pub(in crate::app) async fn list(
    state: &DesktopState,
    method: &str,
    args: &Value,
) -> Result<Value, String> {
    let filters: Filters = serde_json::from_value(args.clone())
        .map_err(|_| "[invalid_input] Invalid To-Do filters.".to_owned())?;
    filters
        .validate()
        .map_err(|e| format!("[invalid_input] {e}"))?;
    let result = if method == "list_github_epics" {
        if security::secret_source("GITHUB_OAUTH_TOKENS")
            .map_err(fail)?
            .is_none()
        {
            return Err("[not_ready] Connect GitHub in Knowledge before listing epics.".into());
        }
        let _guard = state.github_api.lock().await;
        tokio::time::timeout(std::time::Duration::from_secs(90), github::epics(&filters)).await
            .map_err(|_| "[network] GitHub Epic read timed out.".to_owned())?
            .map_err(|_| "[network] GitHub epics could not be read; check the connection and Issues read access.".to_owned())?
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
                    "[not_ready] Configure an Azure DevOps activity source before listing HUs."
                        .into(),
                );
            }
            [source] => source,
            _ => return Err("[invalid_input] Select an Azure DevOps source.".into()),
        };
        if !source.enabled {
            return Err(
                "[not_ready] Enable the selected Azure DevOps source before listing HUs.".into(),
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
        tokio::time::timeout(std::time::Duration::from_secs(90), crate::ado::todo::list(&client, &key, &catalog, &cache, &filters)).await
            .map_err(|_| "[network] Azure HU read timed out.".to_owned())?
            .map_err(|error| if error.to_string().starts_with("invalid") { format!("[invalid_input] {error}") } else { "[network] Azure HUs could not be read; check the catalog, team membership and read credential.".into() })?
    };
    serde_json::to_value(result).map_err(fail)
}
