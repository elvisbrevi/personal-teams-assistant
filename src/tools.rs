use crate::{adapters::graph::Graph, state::Store};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio_util::compat::TokioAsyncWriteCompatExt;

pub mod tasks;
pub mod todo;

#[derive(Clone, Debug, Deserialize, Serialize)]
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
        /// The catalog as a TOML file in a knowledge repository (`repository` + `path`), or
        /// inline in `catalog`; exactly one of them.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        repository: String,
        #[serde(default, skip_serializing_if = "crate::tools::empty_path")]
        path: PathBuf,
        /// Organizations, projects and author written in the map itself, so no knowledge
        /// repository is needed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        catalog: Vec<CatalogSource>,
        secret_ref: String,
        /// A separate credential with permission to edit work items. When set, the user may
        /// link work an activity review found to one of their work items, from the personal
        /// chat and after confirming the exact plan. The only write the app makes outside Teams.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link_secret_ref: Option<String>,
    },
    AzureDevopsWiki {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        repository: String,
        #[serde(default, skip_serializing_if = "crate::tools::empty_path")]
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        catalog: Vec<CatalogSource>,
        secret_ref: String,
        #[serde(default)]
        wiki_ids: Vec<String>,
        #[serde(default)]
        author_mode: crate::ado::wiki::AuthorMode,
        /// Also read the README and OpenAPI files at the root of up to two repositories whose
        /// name carries the search topic, in the catalog's projects (needs `vso.code`). Off by
        /// default: it widens what the source sends to the model.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        repository_docs: bool,
        /// Exact Wiki page URLs; when set, no Search or repository discovery is performed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_urls: Option<Vec<String>>,
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
    /// What the Activities section recorded by day: the tasks the user approved, with their HU
    /// and hours, and verified work not registered yet. Read locally from
    /// `activity-registration.db`; no credential or network.
    ActivityRecord {},
}
/// One Azure DevOps organization of a catalog: the projects to read (`*` = the projects of the
/// teams the user belongs to) and the user's author email.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogSource {
    pub organization: String,
    pub projects: Vec<String>,
    pub author_email: String,
}
pub(crate) fn empty_path(path: &std::path::Path) -> bool {
    path.as_os_str().is_empty()
}
/// An inline catalog as the TOML text the Azure DevOps readers parse.
pub fn catalog_text(catalog: &[CatalogSource]) -> Result<String> {
    #[derive(Serialize)]
    struct Catalog<'a> {
        sources: &'a [CatalogSource],
    }
    Ok(toml::to_string(&Catalog { sources: catalog })?)
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
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
                secret_ref,
                link_secret_ref,
                ..
            } => {
                ensure!(
                    self.valid_catalog_reference()
                        && secret_ref.starts_with("secret://")
                        && link_secret_ref
                            .as_ref()
                            .is_none_or(|s| s.starts_with("secret://") && s != secret_ref),
                    "invalid ADO status catalog reference"
                );
            }
            Self::AzureDevopsWiki {
                secret_ref,
                wiki_ids,
                page_urls,
                ..
            } => {
                ensure!(
                    self.valid_catalog_reference()
                        && secret_ref.starts_with("secret://")
                        && wiki_ids.len() <= 100,
                    "invalid Wiki catalog reference"
                );
                for id in wiki_ids {
                    crate::ado::wiki::validate_id(id)?;
                }
                ensure!(
                    page_urls
                        .as_ref()
                        .is_none_or(|urls| !urls.is_empty() && urls.len() <= 10),
                    "at most 10 pinned Wiki pages are allowed"
                );
                for url in page_urls.iter().flatten() {
                    crate::ado::wiki::validate_page_url(url)?;
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
            Self::TeamsMessages {} | Self::ActivityRecord {} => {}
        }
        Ok(())
    }
    /// A catalog file in a knowledge repository or an inline catalog, never both; an inline
    /// one must also be a valid catalog.
    fn valid_catalog_reference(&self) -> bool {
        let (Self::AzureDevopsStatus {
            repository,
            path,
            catalog,
            ..
        }
        | Self::AzureDevopsWiki {
            repository,
            path,
            catalog,
            ..
        }) = self
        else {
            return false;
        };
        if catalog.is_empty() {
            !repository.is_empty() && path.extension().is_some_and(|e| e == "toml")
        } else {
            repository.is_empty()
                && empty_path(path)
                && catalog_text(catalog)
                    .is_ok_and(|text| crate::ado::validate_catalog(&text).is_ok())
        }
    }
    /// The Azure DevOps catalog (TOML) of an activity or Wiki source: inline, or read from its
    /// file in a declared knowledge repository.
    pub fn catalog(&self, repositories: &BTreeMap<String, PathBuf>) -> Result<String> {
        match self {
            Self::AzureDevopsStatus {
                repository,
                path,
                catalog,
                ..
            }
            | Self::AzureDevopsWiki {
                repository,
                path,
                catalog,
                ..
            } => {
                if catalog.is_empty() {
                    crate::knowledge::read_repository_file(repositories, repository, path)
                } else {
                    catalog_text(catalog)
                }
            }
            _ => anyhow::bail!("this source has no Azure DevOps catalog"),
        }
    }
    /// The knowledge repository whose file holds the catalog, if it is not inline.
    pub fn catalog_repository(&self) -> Option<&str> {
        match self {
            Self::AzureDevopsStatus {
                repository,
                catalog,
                ..
            }
            | Self::AzureDevopsWiki {
                repository,
                catalog,
                ..
            } if catalog.is_empty() => Some(repository),
            _ => None,
        }
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
            Self::ActivityRecord {} => "read_recorded_activities",
        }
    }
    /// Sources an activity review reads: Azure DevOps activity, Wiki edits, own messages.
    pub fn reviews_activity(&self) -> bool {
        matches!(
            self,
            Self::AzureDevopsStatus { .. } | Self::TeamsMessages {}
        ) || matches!(
            self,
            Self::AzureDevopsWiki {
                page_urls: None,
                ..
            }
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
    async fn registration_review(
        &self,
        spec: &ToolSpec,
        days: i64,
        _context: &str,
    ) -> Result<String> {
        self.review(spec, days).await
    }
    /// For a status answer: the user's own recent Wiki pages with their content (production
    /// deployment plans, dates and pending items), as `Evidence` JSON.
    async fn status_pages(&self, _spec: &ToolSpec, _question: &str) -> Result<String> {
        anyhow::bail!("status pages unsupported")
    }
    async fn daily_work(
        &self,
        _spec: &ToolSpec,
        _start: chrono::DateTime<chrono::Utc>,
        _end: chrono::DateTime<chrono::Utc>,
        _effort: &str,
        _day: &str,
    ) -> Result<crate::ado::registration::DailyWork> {
        anyhow::bail!("daily work reads unsupported")
    }
    async fn task_fields(
        &self,
        _spec: &ToolSpec,
        _parent: &crate::ado::link::Target,
        _kind: &str,
    ) -> Result<crate::ado::registration::TaskSchema> {
        anyhow::bail!("task field reads unsupported")
    }
    /// What Azure DevOps rules still ask for the completed task, without writing.
    async fn validate_task(
        &self,
        _spec: &ToolSpec,
        _parent: &crate::ado::link::Target,
        _task: &crate::ado::registration::TaskFields,
        _activities: &[crate::ado::link::Linkable],
    ) -> Result<crate::ado::registration::Check> {
        anyhow::bail!("task validation unsupported")
    }
    async fn register_task(
        &self,
        _spec: &ToolSpec,
        _parent: &crate::ado::link::Target,
        _task: &crate::ado::registration::TaskFields,
        _activities: &[crate::ado::link::Linkable],
        _check: &crate::ado::registration::Check,
    ) -> Result<crate::ado::registration::Written> {
        anyhow::bail!("task registration unsupported")
    }
    /// Whether the source can link work to work items (it has a write credential).
    fn links_work(&self, spec: &ToolSpec) -> bool {
        matches!(
            spec,
            ToolSpec::AzureDevopsStatus {
                link_secret_ref: Some(_),
                ..
            }
        )
    }
    /// Read a work item the user named, inside the catalog's scope.
    async fn work_item(
        &self,
        _spec: &ToolSpec,
        _id: u64,
    ) -> Result<Option<crate::ado::link::Target>> {
        anyhow::bail!("work item reads unsupported")
    }
    /// Add one confirmed link. The only write; the caller records the attempt first and
    /// never calls it twice for the same link.
    async fn add_link(
        &self,
        _spec: &ToolSpec,
        _target: &crate::ado::link::Target,
        _activity: &crate::ado::link::Linkable,
    ) -> Result<crate::ado::link::LinkStatus> {
        anyhow::bail!("linking unsupported")
    }
    /// Create one confirmed task under `parent` with the activities linked; returns its ID.
    /// Like `add_link`, recorded first by the caller and never called twice.
    async fn create_task(
        &self,
        _spec: &ToolSpec,
        _parent: &crate::ado::link::Target,
        _task: &crate::ado::link::NewTask,
        _activities: &[&crate::ado::link::Linkable],
    ) -> Result<(crate::ado::link::LinkStatus, Option<u64>)> {
        anyhow::bail!("task creation unsupported")
    }
}
pub struct Tools {
    pub bindings: BTreeMap<String, String>,
    pub repositories: BTreeMap<String, PathBuf>,
    pub client: reqwest::Client,
    pub store: Arc<Store>,
    pub graph: Arc<Graph>,
}
impl Tools {
    fn registration_catalog(&self, spec: &ToolSpec, write: bool) -> Result<(String, String)> {
        let ToolSpec::AzureDevopsStatus {
            secret_ref,
            link_secret_ref,
            ..
        } = spec
        else {
            anyhow::bail!("registration needs Azure DevOps activity");
        };
        let reference = if write {
            link_secret_ref
                .as_ref()
                .context("write credential unavailable")?
        } else {
            secret_ref
        };
        Ok((
            spec.catalog(&self.repositories)?,
            crate::security::resolve(reference, &self.bindings)?,
        ))
    }
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
    // Azure writes are retired, including plans persisted by an older host.
    fn links_work(&self, _spec: &ToolSpec) -> bool {
        false
    }

    async fn registration_review(
        &self,
        spec: &ToolSpec,
        days: i64,
        context: &str,
    ) -> Result<String> {
        spec.validate()?;
        ensure!(
            spec.reviews_activity(),
            "this source does not allow activity discovery"
        );
        let days = days.clamp(1, crate::ado::registration::LOOKBACK_DAYS);
        if let ToolSpec::AzureDevopsStatus { secret_ref, .. } = spec {
            let operation = async {
                let catalog = spec.catalog(&self.repositories)?;
                let key = crate::security::resolve(secret_ref, &self.bindings)?;
                let evidence = crate::ado::review::review(
                    &self.client,
                    &key,
                    &catalog,
                    days,
                    self.store.clone(),
                    context,
                )
                .await?;
                Ok::<_, anyhow::Error>(serde_json::to_string(&evidence)?)
            };
            tokio::time::timeout(std::time::Duration::from_secs(180), operation).await?
        } else if let ToolSpec::AzureDevopsWiki {
            secret_ref,
            wiki_ids,
            ..
        } = spec
        {
            let operation = async {
                let catalog = spec.catalog(&self.repositories)?;
                let key = crate::security::resolve(secret_ref, &self.bindings)?;
                let reader = crate::ado::wiki::Reader::new(&self.client, &key, &catalog, wiki_ids)?
                    .with_deadline(90)
                    .own_scope()
                    .await?;
                let (edits, linked, partial) = reader
                    .own_edits(chrono::Utc::now() - chrono::Duration::days(days))
                    .await?;
                Ok::<_, anyhow::Error>(serde_json::to_string(
                    &crate::ado::wiki::registration_edits_evidence(&edits, &linked, partial, days),
                )?)
            };
            tokio::time::timeout(std::time::Duration::from_secs(180), operation).await?
        } else if matches!(spec, ToolSpec::TeamsMessages {}) {
            let since = chrono::Utc::now() - chrono::Duration::days(days);
            let found = tokio::time::timeout(
                std::time::Duration::from_secs(180),
                self.graph.registration_activity(since),
            )
            .await??;
            Ok(serde_json::to_string(&found)?)
        } else {
            self.review(spec, days).await
        }
    }
    async fn daily_work(
        &self,
        spec: &ToolSpec,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
        effort: &str,
        day: &str,
    ) -> Result<crate::ado::registration::DailyWork> {
        let (catalog, key) = self.registration_catalog(spec, false)?;
        tokio::time::timeout(
            std::time::Duration::from_secs(180),
            crate::ado::registration::daily_work(
                &self.client,
                &key,
                &catalog,
                start,
                end,
                effort,
                day,
            ),
        )
        .await?
    }
    async fn task_fields(
        &self,
        spec: &ToolSpec,
        parent: &crate::ado::link::Target,
        kind: &str,
    ) -> Result<crate::ado::registration::TaskSchema> {
        let (catalog, key) = self.registration_catalog(spec, false)?;
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            crate::ado::registration::task_schema(&self.client, &key, &catalog, parent, kind),
        )
        .await?
    }

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
                ToolSpec::AzureDevopsStatus { secret_ref, .. } => {
                    let catalog = spec.catalog(&self.repositories)?;
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
                    secret_ref,
                    wiki_ids,
                    author_mode,
                    repository_docs,
                    page_urls,
                    ..
                } => {
                    if let Some(page_urls) = page_urls {
                        let catalog = spec.catalog(&self.repositories)?;
                        let key = crate::security::resolve(secret_ref, &self.bindings)?;
                        let reader =
                            crate::ado::wiki::Reader::new(&self.client, &key, &catalog, wiki_ids)?
                                .own_scope()
                                .await?;
                        return Ok(serde_json::to_string(&reader.read_urls(page_urls).await?)?);
                    }
                    let query = match crate::ado::wiki::question_query(question) {
                        Ok(query)=>query,
                        Err(_)=>return Ok(serde_json::to_string(&crate::ado::wiki::WikiResult{partial:true,warnings:vec!["The Wiki search topic is missing; ask to specify the procedure or project.".into()],..Default::default()})?),
                    };
                    let catalog = spec.catalog(&self.repositories)?;
                    let key = crate::security::resolve(secret_ref, &self.bindings)?;
                    let reader =
                        crate::ado::wiki::Reader::new(&self.client, &key, &catalog, wiki_ids)?
                            .own_scope()
                            .await?;
                    let input = crate::ado::wiki::SearchInput {
                        query,
                        wiki_id: None,
                        author_mode: None,
                    };
                    let mut result = reader.search(&input, *author_mode).await?;
                    if *repository_docs {
                        // Its own deadline: a slow Wiki search must not leave it without time.
                        match crate::ado::wiki::Reader::new(&self.client, &key, &catalog, wiki_ids)?
                            .own_scope()
                            .await
                        {
                            Ok(docs) => docs.repository_docs(&mut result).await,
                            Err(error) => result.warn(&format!(
                                "Team membership unreadable; repositories not read: {error}"
                            )),
                        }
                    }
                    Ok(serde_json::to_string(&result)?)
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
                // The pipeline reads the record itself: it knows the days asked and which
                // sources the conversation may see.
                ToolSpec::ActivityRecord {} => {
                    anyhow::bail!("the activity record is read by the pipeline")
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
        ensure!(
            spec.reviews_activity(),
            "this source does not allow activity discovery"
        );
        let days = days.clamp(1, crate::ado::MAX_WINDOW_DAYS);
        let since = chrono::Utc::now() - chrono::Duration::days(days);
        let operation = async {
            let evidence = match spec {
                ToolSpec::AzureDevopsStatus { secret_ref, .. } => {
                    let catalog = spec.catalog(&self.repositories)?;
                    let key = crate::security::resolve(secret_ref, &self.bindings)?;
                    crate::ado::review::review(
                        &self.client,
                        &key,
                        &catalog,
                        days,
                        self.store.clone(),
                        "",
                    )
                    .await?
                }
                ToolSpec::AzureDevopsWiki {
                    secret_ref,
                    wiki_ids,
                    ..
                } => {
                    let catalog = spec.catalog(&self.repositories)?;
                    let key = crate::security::resolve(secret_ref, &self.bindings)?;
                    let reader =
                        crate::ado::wiki::Reader::new(&self.client, &key, &catalog, wiki_ids)?
                            .with_deadline(90)
                            .own_scope()
                            .await?;
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
    async fn status_pages(&self, spec: &ToolSpec, question: &str) -> Result<String> {
        spec.validate()?;
        ensure!(
            !matches!(spec, ToolSpec::AzureDevopsWiki { page_urls, .. } if page_urls.is_some()),
            "pinned Wiki pages do not allow activity discovery"
        );
        let ToolSpec::AzureDevopsWiki {
            secret_ref,
            wiki_ids,
            ..
        } = spec
        else {
            anyhow::bail!("status pages unsupported");
        };
        // A deployment plan is often written weeks before its date: the longest window.
        let days = crate::ado::MAX_WINDOW_DAYS;
        let since = chrono::Utc::now() - chrono::Duration::days(days);
        let operation = async {
            let catalog = spec.catalog(&self.repositories)?;
            let key = crate::security::resolve(secret_ref, &self.bindings)?;
            let reader = crate::ado::wiki::Reader::new(&self.client, &key, &catalog, wiki_ids)?
                .with_deadline(50)
                .own_scope()
                .await?;
            let (edits, linked, partial) = reader.own_edits(since).await?;
            Ok::<_, anyhow::Error>(serde_json::to_string(&crate::ado::wiki::status_evidence(
                &edits,
                &linked,
                partial,
                question,
                &crate::ado::requested_work_items(question),
                days,
            ))?)
        };
        tokio::time::timeout(std::time::Duration::from_secs(60), operation).await?
    }
    async fn work_item(
        &self,
        spec: &ToolSpec,
        id: u64,
    ) -> Result<Option<crate::ado::link::Target>> {
        let ToolSpec::AzureDevopsStatus { secret_ref, .. } = spec else {
            anyhow::bail!("work item reads unsupported");
        };
        let catalog = spec.catalog(&self.repositories)?;
        let key = crate::security::resolve(secret_ref, &self.bindings)?;
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            crate::ado::link::verify_work_item(&self.client, &key, &catalog, id),
        )
        .await?
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
    pub async fn status_pages(&self, question: &str) -> std::result::Result<String, ToolError> {
        self.executor
            .status_pages(&self.spec, question)
            .await
            .map_err(|_| ToolError)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn installed_tools_cannot_write_even_with_a_legacy_write_credential() {
        struct NoToken;
        #[async_trait]
        impl crate::adapters::oauth::AccessToken for NoToken {
            async fn access_token(&self) -> Result<String> {
                anyhow::bail!("no Graph session")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("cache.db")).unwrap());
        let client = reqwest::Client::new();
        let tools = Tools {
            bindings: BTreeMap::new(),
            repositories: BTreeMap::new(),
            client: client.clone(),
            store: store.clone(),
            graph: Arc::new(Graph {
                client,
                token: Arc::new(NoToken),
                config: Arc::new(crate::config::Config::desktop_template().unwrap()),
                store,
                base_url: "https://graph.microsoft.com/v1.0".into(),
                client_state: String::new(),
            }),
        };
        let spec = ToolSpec::AzureDevopsStatus {
            repository: String::new(),
            path: PathBuf::new(),
            catalog: vec![],
            secret_ref: "secret://reader".into(),
            link_secret_ref: Some("secret://writer".into()),
        };
        assert!(!tools.links_work(&spec));
        let target = crate::ado::link::Target::default();
        assert!(
            tools
                .add_link(&spec, &target, &crate::ado::link::Linkable::default())
                .await
                .is_err()
        );
        assert!(
            tools
                .create_task(
                    &spec,
                    &target,
                    &crate::ado::link::NewTask {
                        title: "Retired".into(),
                        kind: "Task".into()
                    },
                    &[]
                )
                .await
                .is_err()
        );
    }

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
    fn the_activity_record_is_a_local_source_that_reviews_nothing() {
        let map = crate::knowledge::KnowledgeMap::parse(
            "[[resources]]\nid = \"activity-record\"\ndescription = \"Approved work\"\ntopics = []\nenabled = false\nexternal_processing = false\nallowed_conversations = []\nkind = \"tool\"\n[resources.tool]\ntype = \"activity_record\"\n",
        )
        .unwrap();
        let crate::knowledge::Access::Tool { tool } = &map.resources[0].access else {
            panic!("tool expected");
        };
        assert_eq!(tool.name(), "read_recorded_activities");
        // Registration reviews read Azure DevOps, the Wiki and Teams; the record is their result.
        assert!(!tool.reviews_activity());
    }
    #[test]
    fn an_azure_devops_catalog_may_live_in_the_map() {
        let source = |tool: &str| {
            format!(
                "[[resources]]\nid = \"azure-devops-status\"\ndescription = \"Activity\"\ntopics = []\nenabled = false\nexternal_processing = false\nallowed_conversations = []\nkind = \"tool\"\n[resources.tool]\ntype = \"azure_devops_status\"\nsecret_ref = \"secret://ado\"\n{tool}"
            )
        };
        let inline = "[[resources.tool.catalog]]\norganization = \"https://dev.azure.com/example/\"\nprojects = [\"*\"]\nauthor_email = \"me@example.test\"\n";
        // No repository is declared: an inline catalog needs none.
        let map = crate::knowledge::KnowledgeMap::parse(&source(inline)).unwrap();
        let crate::knowledge::Access::Tool { tool } = &map.resources[0].access else {
            panic!("tool expected");
        };
        assert_eq!(tool.catalog_repository(), None);
        let text = tool.catalog(&BTreeMap::new()).unwrap();
        crate::ado::validate_catalog(&text).unwrap();
        assert!(text.contains("me@example.test"), "{text}");
        // Saved again, it keeps the catalog and writes no empty repository or path.
        let saved = toml::to_string(&map).unwrap();
        assert!(!saved.contains("repository"), "{saved}");
        assert!(!saved.contains("path"), "{saved}");
        crate::knowledge::KnowledgeMap::parse(&saved).unwrap();
        // A file reference still needs its declared repository.
        let file = "repository = \"knowledge\"\npath = \"azure-devops.toml\"\n";
        assert!(crate::knowledge::KnowledgeMap::parse(&source(file)).is_err());
        let declared = format!(
            "[repositories]\nknowledge = \"/tmp/knowledge\"\n{}",
            source(file)
        );
        let map = crate::knowledge::KnowledgeMap::parse(&declared).unwrap();
        let crate::knowledge::Access::Tool { tool } = &map.resources[0].access else {
            panic!("tool expected");
        };
        assert_eq!(tool.catalog_repository(), Some("knowledge"));
        // Both at once, neither, or an invalid inline catalog are rejected.
        let both = format!(
            "[repositories]\nknowledge = \"/tmp/knowledge\"\n{}",
            source(&format!("{file}{inline}"))
        );
        assert!(crate::knowledge::KnowledgeMap::parse(&both).is_err());
        assert!(crate::knowledge::KnowledgeMap::parse(&source("")).is_err());
        let foreign = inline.replace("dev.azure.com", "example.com");
        assert!(crate::knowledge::KnowledgeMap::parse(&source(&foreign)).is_err());
    }
    #[test]
    fn tool_argument_is_closed() {
        assert_eq!(lookup_id("estado id: ABC-123").unwrap(), "ABC-123");
        assert!(lookup_id("id: 1 id: 2").is_err());
        assert!(lookup_id("delete everything").is_err());
    }
}
