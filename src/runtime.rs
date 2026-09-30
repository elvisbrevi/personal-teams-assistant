use crate::{
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
use anyhow::{Result, ensure};
use fs2::FileExt;
use std::{sync::Arc, time::Duration};

pub async fn serve(path: &str, stop_rx: tokio::sync::watch::Receiver<bool>) -> Result<()> {
    serve_mode(path, stop_rx, false).await
}

pub async fn serve_desktop(path: &str, stop_rx: tokio::sync::watch::Receiver<bool>) -> Result<()> {
    serve_mode(path, stop_rx, true).await
}

async fn serve_mode(
    path: &str,
    stop_rx: tokio::sync::watch::Receiver<bool>,
    desktop: bool,
) -> Result<()> {
    let config = Arc::new(if desktop {
        Config::load_desktop(path)?
    } else {
        Config::load(path)?
    });
    let knowledge = KnowledgeMap::load(&config.knowledge_map)?;
    ensure!(
        !desktop || config.graph.channels.is_empty(),
        "desktop does not request channel scopes"
    );
    let entra = if desktop {
        None
    } else {
        Some(security::secret("ENTRA_CLIENT_SECRET")?)
    };
    let jev_key = security::secret("TYPESAFE_API_KEY")?;
    let llm_key = security::secret("DEEPSEEK_API_KEY")?;
    let admin_key = if desktop {
        String::new()
    } else {
        security::secret("ADMIN_AUTH_KEY")?
    };
    let client_state = security::secret("GRAPH_WEBHOOK_SECRET")?;
    let encryption_key = security::secret("STATE_ENCRYPTION_KEY")?;
    ensure!(
        (desktop || admin_key.len() >= 32) && client_state.len() >= 32 && client_state.len() <= 128,
        "admin and webhook secrets must have at least 32 characters (webhook max 128)"
    );
    let mut exact_secrets = vec![
        jev_key.clone(),
        llm_key.clone(),
        client_state.clone(),
        encryption_key.clone(),
    ];
    if let Some(entra) = &entra {
        exact_secrets.push(entra.clone());
    }
    if !admin_key.is_empty() {
        exact_secrets.push(admin_key.clone());
    }
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
    let oauth = Arc::new(if let Some(entra) = entra {
        OAuth::new(
            config.clone(),
            client.clone(),
            entra,
            store.clone(),
            Vault::new(&encryption_key)?,
        )?
    } else {
        OAuth::new_public(
            config.clone(),
            client.clone(),
            store.clone(),
            Vault::new(&encryption_key)?,
        )?
    });
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
        let mut stop = stop_rx.clone();
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
    let state = Arc::new(WebState {
        graph,
        oauth,
        admin_key,
        pipeline: Some(pipeline),
    });
    let app = if desktop {
        webhook::public_router(state)
    } else {
        webhook::router(state)
    };
    let listener = tokio::net::TcpListener::bind(&config.server.bind).await?;
    tracing::info!(event = "started", dry_run = config.policy.dry_run);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let mut stop = stop_rx;
            if !*stop.borrow() {
                let _ = stop.changed().await;
            }
        })
        .await?;
    worker.await?;
    renewer.await?;
    drop(lock);
    Ok(())
}
