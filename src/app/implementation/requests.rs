//! Explicit request creation, independent from read-only evidence and daily activity runs.
use super::*;
use crate::{knowledge::Access, tools::ToolSpec};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    target: Value,
    kind: String,
    context: String,
    provider: String,
    model: String,
    effort: String,
    confirmed: bool,
}

fn azure(state: &DesktopState, source: &str) -> Result<(reqwest::Client, String, String)> {
    let config = read_config(&state.config_path)?;
    let map = KnowledgeMap::load(&state.map_path)?;
    let source = map
        .resources
        .iter()
        .find(|r| r.id == source && r.enabled)
        .context("Choose an enabled Azure source.")?;
    let Access::Tool {
        tool: tool @ ToolSpec::AzureDevopsStatus { secret_ref, .. },
    } = &source.access
    else {
        anyhow::bail!("Choose an Azure activity source.");
    };
    let key = security::resolve(secret_ref, &config.secrets)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    Ok((client, key, tool.catalog(&map.repositories)?))
}
async fn targets(host: &Host, provider: &str, source: Option<&str>) -> Result<Vec<Value>> {
    match provider {
        "github" => super::super::github_cli::repositories().await,
        "azure" => {
            let source = source.context("Choose an Azure source.")?;
            let (client, key, catalog) = azure(&host.state, source)?;
            let mut targets =
                crate::ado::implementation::request_targets(&client, &key, &catalog).await?;
            for target in &mut targets {
                target["provider"] = json!("azure");
                target["source"] = json!(source);
            }
            Ok(targets)
        }
        _ => anyhow::bail!("Choose Azure or GitHub."),
    }
}
fn validate(request: &Request) -> Result<()> {
    uuid(&request.id)?;
    ensure!(request.confirmed, "Confirm creation of this request.");
    ensure!(
        !request.context.trim().is_empty() && request.context.len() <= 32_000,
        "Enter request context within the allowed length."
    );
    ensure!(
        matches!(
            (request.target["provider"].as_str(), request.kind.as_str()),
            (Some("github"), "epic") | (Some("azure"), "hu" | "bug")
        ),
        "Invalid request type for this provider."
    );
    Ok(())
}
pub(super) async fn operate(host: &Arc<Host>, method: &str, args: &Value) -> Result<Value> {
    let state = &host.state.implementation;
    match method {
        "implementation_request_catalog" => {
            let provider = args["provider"].as_str().context("Choose a provider.")?;
            let mut catalog = crate::llm::catalog().await;
            catalog
                .providers
                .retain(|p| p.transport == "cli" && p.available);
            let azure_cli = if provider == "azure" {
                Some(super::super::azure_cli::status().await)
            } else {
                None
            };
            let github_cli = if provider == "github" {
                Some(super::super::github_cli::status().await)
            } else {
                None
            };
            let write_ready = azure_cli.as_ref().is_none_or(|s| s.authenticated)
                && github_cli
                    .as_ref()
                    .is_none_or(|s| s["authenticated"] == true);
            let destinations = if provider == "github" && !write_ready {
                Vec::new()
            } else {
                targets(host, provider, args["source"].as_str()).await?
            };
            let mut recent = Vec::new();
            for run in state.db.requests(None)? {
                let target = &run["target"];
                if destinations.contains(target) && !recent.contains(target) {
                    recent.push(target.clone());
                    if recent.len() == 3 {
                        break;
                    }
                }
            }
            Ok(
                json!({"targets":destinations,"providers":catalog.providers,"write_ready":write_ready,"azure_cli":azure_cli,"github_cli":github_cli,"recent":recent}),
            )
        }
        "implementation_request_status" => {
            let id = args["id"].as_str().context("Choose a run.")?;
            uuid(id)?;
            Ok(state.db.requests(Some(id))?.pop().unwrap_or(Value::Null))
        }
        "implementation_history" => {
            let mut runs = state.db.history()?;
            let registry = state.registry.lock().await;
            for run in &mut runs {
                run["online"] = json!(
                    run["machine"] == registry.id
                        || registry
                            .machines
                            .iter()
                            .any(|m| run["machine"] == m.id && now() - m.seen < 35)
                );
                run["machine_name"] = json!(if run["machine"] == registry.id {
                    registry.settings.name.as_str()
                } else {
                    registry
                        .machines
                        .iter()
                        .find(|m| run["machine"] == m.id)
                        .map_or("Unknown machine", |m| m.name.as_str())
                });
            }
            let mut partial = runs.len() >= 100;
            runs.truncate(100);
            let mut size = 0;
            runs.retain(|run| {
                size += run.to_string().len();
                if size > 800_000 {
                    partial = true;
                    false
                } else {
                    true
                }
            });
            Ok(json!({"runs":runs,"partial":partial}))
        }
        "implementation_request_start" => {
            let request: Request = serde_json::from_value(args.clone())?;
            validate(&request)?;
            if let Some(record) = state.db.requests(Some(&request.id))?.pop() {
                ensure!(
                    record["submission"] == args.clone(),
                    "This run ID belongs to a different request."
                );
                return Ok(record);
            }
            let provider = request.target["provider"].as_str().unwrap();
            let available = targets(host, provider, request.target["source"].as_str()).await?;
            ensure!(
                available.contains(&request.target),
                "Select a project or repository from the current catalog."
            );
            crate::llm::implementation::validate(
                &request.provider,
                &request.model,
                &request.effort,
            )
            .await?;
            // Resolve the process type and check the Azure CLI session before starting the model.
            let prepared = if provider == "azure" {
                let (client, key, catalog) =
                    azure(&host.state, request.target["source"].as_str().unwrap())?;
                super::super::azure_cli::require().await?;
                crate::ado::implementation::request_target(
                    &client,
                    &key,
                    &catalog,
                    &request.target,
                    &request.kind,
                )
                .await?
            } else {
                request.target.clone()
            };
            let id = request.id.clone();
            let directory = state.root.join("requests").join(&id);
            security::private_dir(&directory)?;
            let machine = state.registry.lock().await.id.clone();
            let record = json!({"id":id,"machine":machine,"purpose":"request","target":request.target,"project":request.target["project"],"kind":request.kind,"provider":request.provider,"model":request.model,"effort":request.effort,"state":"generating","created_at":chrono::Utc::now().to_rfc3339(),"started_at":null,"finished_at":null,"session_id":null,"item":null,"output":"","submission":args});
            state.db.request_insert(&record)?;
            let host = host.clone();
            let task = tokio::spawn(async move {
                run(&host, request, prepared, directory).await;
            });
            state
                .tasks
                .lock()
                .await
                .insert(format!("request:{id}"), task);
            Ok(record)
        }
        _ => anyhow::bail!("Unknown request operation."),
    }
}
async fn run(host: &Host, request: Request, prepared: Value, directory: PathBuf) {
    let db = &host.state.implementation.db;
    let result = async {
        let language = read_config(&host.state.config_path)?.llm.language;
        let input =
            json!({"context":request.context,"kind":request.kind,"language":language}).to_string();
        let observer = |session: Option<&str>| {
            db.request_update(&request.id, |record| {
                if let Some(session) = session {
                    ensure!(
                        (1..=200).contains(&session.len())
                            && session
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
                        "Invalid session ID."
                    );
                    record["session_id"] = json!(session);
                }
                if record["started_at"].is_null() {
                    record["started_at"] = json!(chrono::Utc::now().to_rfc3339());
                }
                Ok(())
            })
        };
        let draft = crate::llm::implementation::request(
            (&request.provider, &request.model, &request.effort),
            directory,
            &input,
            &observer,
        )
        .await?;
        validate_child(&draft)?;
        // Recheck catalog membership after generation: a model cannot authorize a destination.
        let provider = request.target["provider"].as_str().unwrap();
        let available = targets(host, provider, request.target["source"].as_str()).await?;
        ensure!(
            available.contains(&request.target),
            "The destination is no longer available."
        );
        let write_session = if provider == "azure" {
            Some(super::super::azure_cli::require().await?)
        } else {
            None
        };
        db.request_update(&request.id, |record| {
            record["state"] = json!("sending");
            record["draft"] = draft.clone();
            Ok(())
        })?;
        let item = if provider == "github" {
            let _guard = host.state.github_api.lock().await;
            github::create_request(request.target["project"].as_str().unwrap(), &draft).await?
        } else {
            let (client, _, _) = azure(&host.state, request.target["source"].as_str().unwrap())?;
            crate::ado::implementation::create_request(
                &client,
                write_session.as_ref().unwrap().token(),
                &prepared,
                &draft,
            )
            .await?
        };
        db.request_update(&request.id, |record| {
            record["state"] = item["state"].clone();
            record["item"] = item;
            record["finished_at"] = json!(chrono::Utc::now().to_rfc3339());
            Ok(())
        })?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = db.request_update(&request.id,|record| {
            record["state"] = json!(if record["state"] == "sending" { "uncertain" } else { "failed" });
            record["output"] = json!("Request creation did not complete. Check the agent login and destination access. Inspect the provider before starting another request.");
            record["finished_at"] = json!(chrono::Utc::now().to_rfc3339()); Ok(())
        });
    }
}
pub(super) async fn open_session(host: &Host, id: &str) -> Result<Value> {
    ensure!(
        !host.shell.headless(),
        "This host has no desktop. Open the session on its execution machine."
    );
    let record = host
        .state
        .implementation
        .db
        .requests(Some(id))?
        .pop()
        .context("Session record unavailable.")?;
    let session = record["session_id"]
        .as_str()
        .context("The agent has not reported a session ID yet.")?;
    match record["provider"].as_str() {
        Some("codex") => {
            uuid(session)?;
            host.shell.open_url(&format!("codex://threads/{session}"))?;
        }
        Some("claude") => {
            uuid(session)?;
            ensure!(
                !["generating", "sending"]
                    .iter()
                    .any(|s| record["state"] == *s),
                "This agent session is still running."
            );
            crate::llm::implementation::open_claude_session(
                session,
                &host.state.implementation.root.join("requests").join(id),
            )
            .await?;
        }
        _ => anyhow::bail!("This agent does not provide a verified desktop session link."),
    }
    Ok(json!({"opened":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_require_confirmation_context_and_a_provider_compatible_type() {
        let mut args = json!({"id":"44444444-4444-4444-8444-444444444444","target":{"provider":"azure","project":"Payments","source":"azure","organization":"https://dev.azure.com/example"},"kind":"hu","context":"Support receipt downloads","provider":"codex","model":"test-model","effort":"high","confirmed":true});
        let check = |v: &Value| validate(&serde_json::from_value::<Request>(v.clone()).unwrap());
        assert!(check(&args).is_ok());
        args["confirmed"] = json!(false);
        assert!(check(&args).is_err());
        args["confirmed"] = json!(true);
        args["context"] = json!("  ");
        assert!(check(&args).is_err());
        args["context"] = json!("Support receipt downloads");
        args["kind"] = json!("epic");
        assert!(check(&args).is_err());
        args["target"] = json!({"provider":"github","project":"example/work"});
        assert!(check(&args).is_ok());
        args["kind"] = json!("bug");
        assert!(check(&args).is_err());
    }
}
