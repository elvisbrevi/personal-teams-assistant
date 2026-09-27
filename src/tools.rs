use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
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
    Rabbitmq {
        url: String,
        secret_ref: String,
    },
    Http {
        url: String,
        secret_ref: Option<String>,
    },
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
            Self::Rabbitmq { .. } => "get_queue_status",
            Self::Http { .. } => "http_get",
        }
    }
}
#[async_trait]
pub trait ReadOnlyTool: Send + Sync {
    async fn execute(&self, spec: &ToolSpec, question: &str) -> Result<String>;
}
pub struct Tools {
    pub bindings: BTreeMap<String, String>,
    pub client: reqwest::Client,
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
    async fn execute(&self, spec: &ToolSpec, question: &str) -> Result<String> {
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
                        "https://dev.azure.com/{organization}/{project}/_apis/wit/workitems/{id}?fields=System.Id,System.Title,System.State&api-version=7.1"
                    );
                    let response = self
                        .client
                        .get(url)
                        .basic_auth("", Some(key))
                        .send()
                        .await?;
                    ensure!(response.status().is_success(), "Azure DevOps read failed");
                    let v: Value = crate::adapters::bounded_json(response, 64_000).await?;
                    Ok(serde_json::to_string(&v["fields"])?)
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
        tokio::time::timeout(std::time::Duration::from_secs(5), operation).await?
    }
}

/// Rig tool facade scoped to the resource already authorized by the pipeline.
/// It is invoked by code after Jev routing, never exposed for autonomous LLM use.
pub struct ScopedTool {
    pub executor: Arc<dyn ReadOnlyTool>,
    pub spec: ToolSpec,
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
impl rig::tool::Tool for ScopedTool {
    const NAME: &'static str = "authorized_read";
    type Error = ToolError;
    type Args = ToolArgs;
    type Output = String;
    async fn definition(&self, _prompt: String) -> rig::completion::ToolDefinition {
        rig::completion::ToolDefinition {
            name: Self::NAME.into(),
            description: "Execute the single authorized read-only resource".into(),
            parameters: json!({"type":"object","properties":{"question":{"type":"string"}},"required":["question"],"additionalProperties":false}),
        }
    }
    async fn call(&self, args: ToolArgs) -> std::result::Result<String, ToolError> {
        self.executor
            .execute(&self.spec, &args.question)
            .await
            .map_err(|_| ToolError)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_argument_is_closed() {
        assert_eq!(lookup_id("estado id: ABC-123").unwrap(), "ABC-123");
        assert!(lookup_id("id: 1 id: 2").is_err());
        assert!(lookup_id("delete everything").is_err());
    }
}
