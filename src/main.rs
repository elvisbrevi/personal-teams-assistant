use anyhow::{Result, ensure};
use personal_teams_assistant::{config::Config, knowledge::KnowledgeMap, runtime};

#[tokio::main]
async fn main() -> Result<()> {
    // Provider crates can trace prompts. Only our own fixed, content-free audit events are enabled.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter("personal_teams_assistant=info")
        .init();
    let args: Vec<_> = std::env::args().skip(1).collect();
    let path = args.get(1).map(String::as_str).unwrap_or("config.toml");
    ensure!(
        args.is_empty()
            || args
                .first()
                .is_some_and(|s| matches!(s.as_str(), "serve" | "check" | "local-info")),
        "usage: personal-teams-assistant [serve|check] [config.toml]"
    );
    if args
        .first()
        .is_some_and(|s| s == "check" || s == "local-info")
    {
        let config = Config::load(path)?;
        let knowledge = KnowledgeMap::load(&config.knowledge_map)?;
        if args.first().is_some_and(|s| s == "check") {
            println!(
                "Configuration and knowledge map valid ({} resources).",
                knowledge.resources.len()
            );
        } else {
            println!(
                "{}",
                serde_json::json!({"bind":config.server.bind,"client_id":config.graph.client_id,"tenant_id":config.graph.tenant_id,"additional_secrets":config.secrets.values().collect::<Vec<_>>()})
            );
        }
        return Ok(());
    }
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let running = runtime::serve(path, stop_rx);
    tokio::pin!(running);
    tokio::select! {
        result = &mut running => result?,
        _ = shutdown() => {
            let _ = stop_tx.send(true);
            running.await?;
        }
    }
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
