use anyhow::{Result, ensure};
use fs2::FileExt;
use personal_teams_assistant::{
    adapters::{
        graph::Graph,
        oauth::OAuth,
        webhook::{self, WebState},
    },
    config::Config,
    decision::Jev,
    knowledge::KnowledgeMap,
    llm::DeepSeek,
    pipeline::Pipeline,
    security::{self, Redactor, Vault},
    state::{Audit, Store},
    tools::Tools,
};
use std::{sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    // Provider crates can trace prompts. Only our own fixed, content-free audit events are enabled.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter("personal_teams_assistant=info")
        .init();
    let args: Vec<_> = std::env::args().skip(1).collect();
    let path = args.get(1).map(String::as_str).unwrap_or("config.toml");
    let config = Arc::new(Config::load(path)?);
    let knowledge = KnowledgeMap::load(&config.knowledge_map)?;
    if args.first().is_some_and(|s| s == "local-info") {
        println!(
            "{}",
            serde_json::json!({"bind":config.server.bind,"client_id":config.graph.client_id,"tenant_id":config.graph.tenant_id,"additional_secrets":config.secrets.values().collect::<Vec<_>>()})
        );
        return Ok(());
    }

    if args.first().is_some_and(|s| s == "check") {
        println!(
            "Configuration and knowledge map valid ({} resources).",
            knowledge.resources.len()
        );
        return Ok(());
    }
    ensure!(
        args.is_empty() || args.first().is_some_and(|s| s == "serve"),
        "usage: personal-teams-assistant [serve|check] [config.toml]"
    );
    let entra = security::secret("ENTRA_CLIENT_SECRET")?;
    let jev_key = security::secret("TYPESAFE_API_KEY")?;
    let llm_key = security::secret("DEEPSEEK_API_KEY")?;
    let admin_key = security::secret("ADMIN_AUTH_KEY")?;
    let client_state = security::secret("GRAPH_WEBHOOK_SECRET")?;
    let encryption_key = security::secret("STATE_ENCRYPTION_KEY")?;
    ensure!(
        admin_key.len() >= 32 && client_state.len() >= 32 && client_state.len() <= 128,
        "admin and webhook secrets must have at least 32 characters (webhook max 128)"
    );
    let mut exact_secrets = vec![
        entra.clone(),
        jev_key.clone(),
        llm_key.clone(),
        admin_key.clone(),
        client_state.clone(),
        encryption_key.clone(),
    ];
    for name in config.secrets.values() {
        exact_secrets.push(security::secret(name)?);
    }
    let redactor = Arc::new(Redactor::new(
        &config.policy.sensitive_patterns,
        exact_secrets,
    )?);
    ensure!(
        redactor.clean(&config.llm.style) && redactor.clean(&config.policy.greeting),
        "style or greeting contains sensitive data"
    );
    security::private_dir(&config.server.data_dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(config.server.data_dir.join("instance.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("another instance is using this state directory"))?;
    let store = Arc::new(Store::open(&config.server.data_dir.join("assistant.db"))?);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let oauth = Arc::new(OAuth::new(
        config.clone(),
        client.clone(),
        entra,
        store.clone(),
        Vault::new(&encryption_key)?,
    )?);
    let graph = Arc::new(Graph {
        client: client.clone(),
        token: oauth.clone(),
        base_url: "https://graph.microsoft.com/v1.0".into(),
        config: config.clone(),
        store: store.clone(),
        client_state,
    });
    let gate = Arc::new(Jev {
        client: client.clone(),
        endpoint: "https://api.typesafe.ai/v1/systemone".into(),
        api_key: jev_key,
        model: config.jev.model.clone(),
    });
    let llm = Arc::new(DeepSeek::new(
        &llm_key,
        &config.llm.model,
        &config.llm.style,
        "https://api.deepseek.com",
    )?);
    let tools = Arc::new(Tools {
        bindings: config.secrets.clone(),
        repositories: knowledge.repositories.clone(),
        client,
        store: store.clone(),
        graph: graph.clone(),
    });
    let pipeline = Arc::new(Pipeline {
        config: config.clone(),
        store: store.clone(),
        adapter: graph.clone(),
        gate,
        knowledge,
        llm,
        tools,
        redactor,
    });
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let worker = {
        let graph = graph.clone();
        let store = store.clone();
        let pipeline = pipeline.clone();
        let mut stop = stop_rx.clone();
        tokio::spawn(async move {
            loop {
                if *stop.borrow() {
                    break;
                }
                match store.next_job() {
                    Ok(Some(job)) => {
                        let result = if let Some(suffix) = job.resource.strip_prefix("recovery:") {
                            let id = suffix.split(':').next().unwrap_or("");
                            let recovery = async {
                                if let Some(sub) =
                                    store.subscriptions()?.into_iter().find(|s| s.id == id)
                                {
                                    graph.recover_missed(&sub.resource).await?;
                                }
                                store.record(
                                    &job.resource,
                                    &Audit {
                                        status: "recovered".into(),
                                        reason: "bounded_latest_page".into(),
                                        ..Default::default()
                                    },
                                )
                            };
                            recovery.await
                        } else {
                            pipeline.process(&job.resource).await
                        };
                        if result.is_err() {
                            tracing::warn!(event = "processing_failed", attempt = job.attempts + 1);
                            if store.retry(&job).is_err() {
                                tracing::error!(event = "state_write_failed");
                            }
                        }
                    }
                    Ok(None) => {
                        tokio::select! {_=tokio::time::sleep(Duration::from_millis(250))=>{},_=stop.changed()=>{}}
                    }
                    Err(_) => {
                        tracing::error!(event = "state_read_failed");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        })
    };
    let renewer = {
        let graph = graph.clone();
        let mut stop = stop_rx;
        tokio::spawn(async move {
            loop {
                if *stop.borrow() {
                    break;
                }
                if let Err(error) = graph.reconcile_subscriptions().await {
                    tracing::warn!(
                        event = "subscription_sync_failed",
                        error = %error,
                        action = "check_authorization_and_permissions"
                    );
                }
                tokio::select! {_=tokio::time::sleep(Duration::from_secs(60))=>{},_=stop.changed()=>{}}
            }
        })
    };
    let app = webhook::router(Arc::new(WebState {
        graph,
        oauth,
        admin_key,
        pipeline: Some(pipeline),
    }));
    let listener = tokio::net::TcpListener::bind(&config.server.bind).await?;
    tracing::info!(event = "started", dry_run = config.policy.dry_run);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown().await;
            let _ = stop_tx.send(true);
        })
        .await?;
    worker.await?;
    renewer.await?;
    drop(lock);
    Ok(())
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("signal handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
