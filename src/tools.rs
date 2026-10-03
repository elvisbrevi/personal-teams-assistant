use crate::{adapters::graph::Graph, state::Store};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio_util::compat::TokioAsyncWriteCompatExt;

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolSpec {
    SqlServer {
        operation: SqlOperation,
        secret_ref: String,
    },
    AzureDevops {
        organization: String,
        project: String,
        secret_ref: String,
    },
    AzureDevopsStatus {
        repository: String,
        path: PathBuf,
        secret_ref: String,
    },
    AzureDevopsWiki {
        repository: String,
        path: PathBuf,
        secret_ref: String,
        #[serde(default)]
        wiki_ids: Vec<String>,
        #[serde(default)]
        author_mode: crate::ado::wiki::AuthorMode,
    },
    Rabbitmq {
        url: String,
        secret_ref: String,
    },
    Http {
        url: String,
        secret_ref: Option<String>,
    },
    /// The user's own messages in their Teams chats, read with the delegated Graph session
    /// (`Chat.Read`, already granted). Used to review the user's activity.
    TeamsMessages {},
}
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlOperation {
    GetPaymentStatus,
    FindTransaction,
    GetFailedIntegrations,
}
impl ToolSpec {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::SqlServer { secret_ref, .. } => ensure!(
                secret_ref.starts_with("secret://"),
                "invalid SQL credential reference"
            ),
            Self::AzureDevops {
                organization,
                project,
                secret_ref,
            } => {
                ensure!(
                    [organization, project].iter().all(|s| !s.is_empty()
                        && s.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))),
                    "invalid Azure DevOps scope"
                );
                ensure!(
                    secret_ref.starts_with("secret://"),
                    "invalid ADO credential reference"
                );
            }
            Self::AzureDevopsStatus {
                repository,
                path,
                secret_ref,
            } => {
                ensure!(
                    !repository.is_empty()
                        && secret_ref.starts_with("secret://")
                        && path.extension().is_some_and(|ext| ext == "toml"),
                    "invalid ADO status catalog reference"
                );
            }
            Self::AzureDevopsWiki {
                repository,
                path,
                secret_ref,
                wiki_ids,
                ..
            } => {
                ensure!(
                    !repository.is_empty()
                        && path.extension().is_some_and(|e| e == "toml")
                        && secret_ref.starts_with("secret://")
                        && wiki_ids.len() <= 100,
                    "invalid Wiki catalog reference"
                );
                for id in wiki_ids {
                    crate::ado::wiki::validate_id(id)?;
                }
            }
            Self::Rabbitmq { url, secret_ref } => {
                let u = crate::knowledge::validate_url(url)?;
                ensure!(
                    u.path().starts_with("/api/queues/") && u.query().is_none(),
                    "RabbitMQ only allows reading one queue"
                );
                ensure!(
                    secret_ref.starts_with("secret://"),
                    "invalid RabbitMQ credential reference"
                );
            }
            Self::Http { url, secret_ref } => {
                crate::knowledge::validate_url(url)?;
                if let Some(s) = secret_ref {
                    ensure!(
                        s.starts_with("secret://"),
                        "invalid HTTP credential reference"
                    );
                }
            }
            Self::TeamsMessages {} => {}
        }
        Ok(())
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::SqlServer { operation, .. } => match operation {
                SqlOperation::GetPaymentStatus => "get_payment_status",
                SqlOperation::FindTransaction => "find_transaction",
                SqlOperation::GetFailedIntegrations => "get_failed_integrations",
            },
            Self::AzureDevops { .. } => "get_work_item",
            Self::AzureDevopsStatus { .. } => "get_azure_devops_status",
            Self::AzureDevopsWiki { .. } => "search_azure_devops_wiki",
            Self::Rabbitmq { .. } => "get_queue_status",
            Self::Http { .. } => "http_get",
            Self::TeamsMessages {} => "get_own_teams_messages",
        }
    }
    /// Sources an activity review reads: Azure DevOps activity, Wiki edits, own messages.
    pub fn reviews_activity(&self) -> bool {
        matches!(
            self,
            Self::AzureDevopsStatus { .. } | Self::AzureDevopsWiki { .. } | Self::TeamsMessages {}
        )
    }
}
#[async_trait]
pub trait ReadOnlyTool: Send + Sync {
    async fn execute(&self, spec: &ToolSpec, question: &str, conversation: &str) -> Result<String>;
    /// The user's own activity in the last `days` days, as `Evidence` JSON that separates
    /// work with a linked work item from work without one.
    async fn review(&self, _spec: &ToolSpec, _days: i64) -> Result<String> {
        anyhow::bail!("activity review unsupported")
    }
}
pub struct Tools {
    pub bindings: BTreeMap<String, String>,
    pub repositories: BTreeMap<String, PathBuf>,
    pub client: reqwest::Client,
    pub store: Arc<Store>,
    pub graph: Arc<Graph>,
}
fn lookup_id(question: &str) -> Result<String> {
    // Deliberately explicit: the question must contain exactly one `id: ABC-123`.
    let re = regex::Regex::new(r"(?i)\bid:\s*([A-Z0-9_-]{1,64})\b")?;
    let matches: Vec<_> = re.captures_iter(question).collect();
    ensure!(
        matches.len() == 1,
        "tool needs exactly one explicit id: value"
    );
    Ok(matches[0][1].to_owned())
}
#[async_trait]
impl ReadOnlyTool for Tools {
    async fn execute(&self, spec: &ToolSpec, question: &str, conversation: &str) -> Result<String> {
        spec.validate()?;
        let operation = async {
            match spec {
                ToolSpec::SqlServer {
                    operation,
                    secret_ref,
                } => {
                    let connection = crate::security::resolve(secret_ref, &self.bindings)?;
                    let mut cfg = tiberius::Config::from_ado_string(&connection)?;
                    // Enforce encryption even if the referenced connection string tries to turn it off.
                    cfg.encryption(tiberius::EncryptionLevel::Required);
                    ensure!(
                        !connection.to_lowercase().contains("trustservercertificate"),
                        "SQL certificate trust overrides are forbidden"
                    );
                    let tcp = tokio::net::TcpStream::connect(cfg.get_addr()).await?;
                    tcp.set_nodelay(true)?;
                    let mut client = tiberius::Client::connect(cfg, tcp.compat_write()).await?;
                    // Expose only read-only reporting views with these columns. No arbitrary SQL input.
                    let id = match operation {
                        SqlOperation::GetFailedIntegrations => String::new(),
                        _ => lookup_id(question)?,
                    };
                    let query = match operation {
                        SqlOperation::GetPaymentStatus => {
                            "SELECT TOP (1) CAST(payment_id AS nvarchar(64)), CAST(status AS nvarchar(128)) FROM assistant_readonly.payments WHERE payment_id = @P1"
                        }
                        SqlOperation::FindTransaction => {
                            "SELECT TOP (1) CAST(transaction_id AS nvarchar(64)), CAST(status AS nvarchar(128)) FROM assistant_readonly.transactions WHERE transaction_id = @P1"
                        }
                        SqlOperation::GetFailedIntegrations => {
                            "SELECT TOP (20) CAST(integration_name AS nvarchar(64)), CAST(status AS nvarchar(128)) FROM assistant_readonly.failed_integrations WHERE status = 'failed' ORDER BY integration_name"
                        }
                    };
                    let rows = if matches!(operation, SqlOperation::GetFailedIntegrations) {
                        client.query(query, &[]).await?.into_first_result().await?
                    } else {
                        client
                            .query(query, &[&id])
                            .await?
                            .into_first_result()
                            .await?
                    };
                    let values: Vec<_> = rows
                        .iter()
                        .take(20)
                        .map(|row| json!({"id":row.get::<&str,_>(0),"status":row.get::<&str,_>(1)}))
                        .collect();
                    Ok(serde_json::to_string(&values)?)
                }
                ToolSpec::AzureDevops {
                    organization,
                    project,
                    secret_ref,
                } => {
                    let id = lookup_id(question)?;
                    ensure!(
                        id.chars().all(|c| c.is_ascii_digit()),
                        "work item ID must be numeric"
                    );
                    let key = crate::security::resolve(secret_ref, &self.bindings)?;
                    let url = format!(
                        "https://dev.azure.com/{organization}/{project}/_apis/wit/workitems/{id}?fields=System.Id,System.Title,System.State,System.TeamProject&api-version=7.1"
                    );
                    let response = self
                        .client
                        .get(url)
                        .basic_auth("", Some(key))
                        .send()
                        .await?;
                    ensure!(response.status().is_success(), "Azure DevOps read failed");
                    let v: Value = crate::adapters::bounded_json(response, 64_000).await?;
                    ensure!(
                        v["fields"]["System.TeamProject"]
                            .as_str()
                            .is_some_and(|p| p.eq_ignore_ascii_case(project)),
                        "work item outside configured project"
                    );
                    let reference = crate::evidence::Reference {
                        id: format!("work_item:{organization}:{project}:{id}"),
                        kind: "work_item".into(),
                        label: format!("Work item #{id}"),
                        url: format!(
                            "https://dev.azure.com/{organization}/{project}/_workitems/edit/{id}"
                        ),
                        organization: format!("https://dev.azure.com/{organization}"),
                        project: project.clone(),
                        aliases: vec![format!("#{id}"), format!("work item {id}")],
                        parent: None,
                        revision: None,
                        authority: None,
                        author: None,
                        author_role: None,
                    };
                    reference.validate()?;
                    Ok(serde_json::to_string(&crate::evidence::Evidence {
                        text: serde_json::to_string(&v["fields"])?,
                        references: vec![reference],
                        ..Default::default()
                    })?)
                }
                ToolSpec::AzureDevopsStatus {
                    repository,
                    path,
                    secret_ref,
                } => {
                    let catalog = crate::knowledge::read_repository_file(
                        &self.repositories,
                        repository,
                        path,
                    )?;
                    let key = crate::security::resolve(secret_ref, &self.bindings)?;
                    let mut evidence = crate::ado::status(
                        &self.client,
                        &key,
                        &catalog,
                        question,
                        self.store.clone(),
                    )
                    .await?;
                    let projects: Vec<String> = evidence
                        .text
                        .lines()
                        .filter_map(|line| {
                            line.strip_prefix("Project: ")?
                                .strip_suffix('.')
                                .map(str::to_owned)
                        })
                        .collect();
                    let since = chrono::Utc::now()
                        - chrono::Duration::days(crate::ado::recent_window(question));
                    if let Ok(context) = self
                        .graph
                        .recent_project_context(&projects, conversation, since)
                        .await
                        && !context.is_empty()
                    {
                        if let Ok(Ok(refs)) = tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            crate::ado::team_references(&self.client, &key, &catalog, &context),
                        )
                        .await
                        {
                            evidence.references.extend(refs);
                        }
                        evidence.teams = context;
                    }
                    Ok(serde_json::to_string(&evidence)?)
                }
                ToolSpec::AzureDevopsWiki {
                    repository,
                    path,
                    secret_ref,
                    wiki_ids,
                    author_mode,
                } => {
                    let query = match crate::ado::wiki::question_query(question) {
                        Ok(query)=>query,
                        Err(_)=>return Ok(serde_json::to_string(&crate::ado::wiki::WikiResult{partial:true,warnings:vec!["The Wiki search topic is missing; ask to specify the procedure or project.".into()],..Default::default()})?),
                    };
                    let catalog = crate::knowledge::read_repository_file(
                        &self.repositories,
                        repository,
                        path,
                    )?;
                    let key = crate::security::resolve(secret_ref, &self.bindings)?;
                    let reader =
                        crate::ado::wiki::Reader::new(&self.client, &key, &catalog, wiki_ids)?;
                    let input = crate::ado::wiki::SearchInput {
                        query,
                        wiki_id: None,
                        author_mode: None,
                    };
                    Ok(serde_json::to_string(
                        &reader.search(&input, *author_mode).await?,
                    )?)
                }
                ToolSpec::Rabbitmq { url, secret_ref } => {
                    let auth = crate::security::resolve(secret_ref, &self.bindings)?;
                    let (user, pass) = auth.split_once(':').ok_or_else(|| {
                        anyhow::anyhow!("RabbitMQ credential must be username:password")
                    })?;
                    let response = self
                        .client
                        .get(url)
                        .basic_auth(user, Some(pass))
                        .send()
                        .await?;
                    ensure!(response.status().is_success(), "RabbitMQ read failed");
                    let v: Value = crate::adapters::bounded_json(response, 64_000).await?;
                    Ok(json!({"name":v["name"],"messages":v["messages"],"messages_ready":v["messages_ready"],"consumers":v["consumers"],"state":v["state"]}).to_string())
                }
                ToolSpec::TeamsMessages {} => {
                    let days = crate::ado::recent_window(question);
                    let since = chrono::Utc::now() - chrono::Duration::days(days);
                    let (messages, chats, partial) = self.graph.own_messages(since).await?;
                    Ok(serde_json::to_string(
                        &crate::adapters::graph::own_messages_evidence(
                            &messages, chats, partial, days,
                        ),
                    )?)
                }
                ToolSpec::Http { url, secret_ref } => {
                    let mut req = self.client.get(url);
                    if let Some(s) = secret_ref {
                        req = req.bearer_auth(crate::security::resolve(s, &self.bindings)?);
                    }
                    let response = req.send().await?;
                    ensure!(response.status().is_success(), "internal API read failed");
                    // Fixed operator-configured URL; redirects disabled. Never accept a URL from model output.
                    let bytes = crate::adapters::bounded_bytes(response, 64_000).await?;
                    Ok(String::from_utf8(bytes)?)
                }
            }
        };
        let seconds = match spec {
            ToolSpec::AzureDevopsStatus { .. } => 180,
            ToolSpec::AzureDevopsWiki { .. } => 32,
            ToolSpec::TeamsMessages {} => 60,
            _ => 5,
        };
        tokio::time::timeout(std::time::Duration::from_secs(seconds), operation).await?
    }
    async fn review(&self, spec: &ToolSpec, days: i64) -> Result<String> {
        spec.validate()?;
        let days = days.clamp(1, 31);
        let since = chrono::Utc::now() - chrono::Duration::days(days);
        let operation = async {
            let evidence = match spec {
                ToolSpec::AzureDevopsStatus {
                    repository,
                    path,
                    secret_ref,
                } => {
                    let catalog = crate::knowledge::read_repository_file(
                        &self.repositories,
                        repository,
                        path,
                    )?;
                    let key = crate::security::resolve(secret_ref, &self.bindings)?;
                    crate::ado::review::review(
                        &self.client,
                        &key,
                        &catalog,
                        days,
                        self.store.clone(),
                    )
                    .await?
                }
                ToolSpec::AzureDevopsWiki {
                    repository,
                    path,
                    secret_ref,
                    wiki_ids,
                    ..
                } => {
                    let catalog = crate::knowledge::read_repository_file(
                        &self.repositories,
                        repository,
                        path,
                    )?;
                    let key = crate::security::resolve(secret_ref, &self.bindings)?;
                    let reader =
                        crate::ado::wiki::Reader::new(&self.client, &key, &catalog, wiki_ids)?
                            .with_deadline(90);
                    let (edits, linked, partial) = reader.own_edits(since).await?;
                    crate::ado::wiki::edits_evidence(&edits, &linked, partial, days)
                }
                ToolSpec::TeamsMessages {} => {
                    let (messages, chats, partial) = self.graph.own_messages(since).await?;
                    crate::adapters::graph::own_messages_evidence(&messages, chats, partial, days)
                }
                _ => anyhow::bail!("activity review unsupported"),
            };
            Ok::<_, anyhow::Error>(serde_json::to_string(&evidence)?)
        };
        tokio::time::timeout(std::time::Duration::from_secs(180), operation).await?
    }
}

/// Tool facade scoped to the resource already authorized by the pipeline.
/// It is invoked by code after routing, never exposed for autonomous LLM use.
pub struct ScopedTool {
    pub executor: Arc<dyn ReadOnlyTool>,
    pub spec: ToolSpec,
    pub conversation: String,
}
#[derive(Deserialize)]
pub struct ToolArgs {
    pub question: String,
}
#[derive(Debug)]
pub struct ToolError;
impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "read-only tool failed")
    }
}
impl std::error::Error for ToolError {}
impl ScopedTool {
    pub async fn call(&self, args: ToolArgs) -> std::result::Result<String, ToolError> {
        self.executor
            .execute(&self.spec, &args.question, &self.conversation)
            .await
            .map_err(|_| ToolError)
    }
    pub async fn review(&self, days: i64) -> std::result::Result<String, ToolError> {
        self.executor
            .review(&self.spec, days)
            .await
            .map_err(|_| ToolError)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn own_teams_messages_source_parses_and_reviews_activity() {
        let map = crate::knowledge::KnowledgeMap::parse(
            "[[resources]]\nid = \"teams-activity\"\ndescription = \"Own messages\"\ntopics = []\nenabled = false\nexternal_processing = false\nallowed_conversations = []\nkind = \"tool\"\n[resources.tool]\ntype = \"teams_messages\"\n",
        )
        .unwrap();
        let crate::knowledge::Access::Tool { tool } = &map.resources[0].access else {
            panic!("tool expected");
        };
        assert_eq!(tool.name(), "get_own_teams_messages");
        assert!(tool.reviews_activity());
        assert!(
            crate::knowledge::KnowledgeMap::parse(
                "[[resources]]\nid = \"teams-activity\"\ndescription = \"x\"\ntopics = []\nenabled = false\nexternal_processing = false\nallowed_conversations = []\nkind = \"tool\"\n[resources.tool]\ntype = \"teams_messages\"\nchat = \"all\"\n",
            )
            .is_err()
        );
    }
    #[test]
    fn tool_argument_is_closed() {
        assert_eq!(lookup_id("estado id: ABC-123").unwrap(), "ABC-123");
        assert!(lookup_id("id: 1 id: 2").is_err());
        assert!(lookup_id("delete everything").is_err());
    }
}
