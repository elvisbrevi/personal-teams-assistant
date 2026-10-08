use crate::{
    adapters::{
        graph::Graph,
        oauth::OAuth,
        webhook::{self, WebState},
    },
    config::Config,
    knowledge::KnowledgeMap,
    pipeline::Pipeline,
    security::{self, Redactor, Vault},
    state::{Audit, Store},
    tools::Tools,
};
use anyhow::{Result, ensure};
use fs2::FileExt;
use std::{sync::Arc, time::Duration};

fn stop_requested(stop: &tokio::sync::watch::Receiver<bool>) -> bool {
    *stop.borrow() || stop.has_changed().is_err()
}

/// Run the Teams service owned by the desktop host until `stop_rx` turns true or closes.
/// `ready` fires once the listener is bound and background tasks are running.
pub async fn serve(
    path: &str,
    stop_rx: tokio::sync::watch::Receiver<bool>,
    ready: tokio::sync::oneshot::Sender<Arc<Graph>>,
) -> Result<()> {
    let config = Arc::new(Config::load(path)?);
    let knowledge = KnowledgeMap::load(&config.knowledge_map)?;
    let (llm, llm_secrets) = crate::llm::from_config(&config.llm)?;
    let client_state = security::secret("GRAPH_WEBHOOK_SECRET")?;
    let encryption_key = security::secret("STATE_ENCRYPTION_KEY")?;
    ensure!(
        client_state.len() >= 32 && client_state.len() <= 128,
        "webhook secret must have 32-128 characters"
    );
    let mut exact_secrets = vec![client_state.clone(), encryption_key.clone()];
    exact_secrets.extend(llm_secrets);
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
        store.clone(),
        Vault::new(&encryption_key)?,
    )?);
    let graph = Arc::new(Graph {
        client: client.clone(),
        token: oauth,
        base_url: "https://graph.microsoft.com/v1.0".into(),
        config: config.clone(),
        store: store.clone(),
        client_state,
    });
    graph.verify_account().await?;
    let llm = Arc::new(llm);
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
        knowledge,
        llm,
        tools,
        redactor,
    });
    let listener = tokio::net::TcpListener::bind(&config.server.bind).await?;
    let worker = {
        let graph = graph.clone();
        let store = store.clone();
        let pipeline = pipeline.clone();
        let mut stop = stop_rx.clone();
        tokio::spawn(async move {
            loop {
                if stop_requested(&stop) {
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
    let personal_poller = {
        let graph = graph.clone();
        let mut stop = stop_rx.clone();
        tokio::spawn(async move {
            loop {
                if stop_requested(&stop) {
                    break;
                }
                if graph.poll_self_chat().await.is_err() {
                    let _ = graph.store.event("self_chat", "poll_failed");
                }
                tokio::select! { _=tokio::time::sleep(Duration::from_secs(10))=>{}, _=stop.changed()=>{} }
            }
        })
    };
    let renewer = {
        let graph = graph.clone();
        let mut stop = stop_rx.clone();
        tokio::spawn(async move {
            loop {
                if stop_requested(&stop) {
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
    struct Background(Vec<tokio::task::AbortHandle>);
    impl Drop for Background {
        fn drop(&mut self) {
            for task in &self.0 {
                task.abort();
            }
        }
    }
    let _background = Background(vec![
        worker.abort_handle(),
        renewer.abort_handle(),
        personal_poller.abort_handle(),
    ]);
    let _ = ready.send(graph.clone());
    let app = webhook::router(Arc::new(WebState { graph }));
    tracing::info!(event = "started", dry_run = config.policy.dry_run);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let mut stop = stop_rx;
            if !stop_requested(&stop) {
                let _ = stop.changed().await;
            }
        })
        .await?;
    worker.await?;
    renewer.await?;
    personal_poller.await?;
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::stop_requested;

    #[test]
    fn cancelled_startup_closes_stop_channel_and_terminates_background_tasks() {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        assert!(!stop_requested(&receiver));
        sender.send(true).unwrap();
        assert!(stop_requested(&receiver));

        let (sender, receiver) = tokio::sync::watch::channel(false);
        drop(sender);
        assert!(stop_requested(&receiver));
    }
}
