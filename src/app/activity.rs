//! Host-owned scheduler: independent of Teams reception, sharing its Graph token owner.
use super::*;
use crate::{
    activity::{Recorder, Resolution, store::Database},
    adapters::{graph::Graph, oauth::AccessToken},
    security::Redactor,
    tools::Tools,
};
use chrono::{NaiveDate, Utc};
use serde_json::{Value, json};

struct NoToken;
#[async_trait::async_trait]
impl AccessToken for NoToken {
    async fn access_token(&self) -> Result<String> {
        anyhow::bail!("connect Microsoft before reading chats and meetings")
    }
}
fn database(state: &DesktopState) -> Result<Arc<Database>> {
    let config = read_config(&state.config_path)?;
    security::private_dir(&config.server.data_dir)?;
    let path = config.server.data_dir.join("activity-registration.db");
    let db = Arc::new(Database::open(&path)?);
    security::protect_file(&path)?;
    Ok(db)
}
pub(super) async fn active(state: &DesktopState) -> bool {
    state
        .activity_task
        .lock()
        .await
        .as_ref()
        .is_some_and(|t| !t.is_finished())
}
async fn recover(state: &DesktopState) -> Result<()> {
    let path = read_config(&state.config_path)?.server.data_dir;
    let mut recovered = state.activity_recovered.lock().await;
    if recovered.as_ref() != Some(&path) {
        database(state)?.recover()?;
        *recovered = Some(path);
    }
    Ok(())
}
async fn recorder(state: &DesktopState) -> Result<Recorder> {
    let config = read_config(&state.config_path)?;
    let map = KnowledgeMap::load(&state.map_path)?;
    config.validate()?;
    crate::activity::validate_sources(&config.activity_registration, &map)?;
    let (llm, mut secrets) = crate::llm::from_config(&config.llm)?;
    for name in config.secrets.values() {
        secrets.push(security::secret(name)?);
    }
    let redactor = Arc::new(Redactor::new(&config.policy.sensitive_patterns, secrets)?);
    let db = database(state)?;
    let cache = Arc::new(Store::open(
        &config.server.data_dir.join("activity-cache.db"),
    )?);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let config_arc = Arc::new(config.clone());
    let mut graph = state.activity_graph.lock().await;
    if graph.is_none() {
        let path = config.server.data_dir.join("assistant.db");
        let token: Arc<dyn AccessToken> = if Store::has_token(&path)? {
            Arc::new(OAuth::new(
                config_arc.clone(),
                client.clone(),
                Arc::new(Store::attach(&path)?),
                Vault::new(&security::secret("STATE_ENCRYPTION_KEY")?)?,
            )?)
        } else {
            Arc::new(NoToken)
        };
        *graph = Some(Arc::new(Graph {
            client: client.clone(),
            token,
            config: config_arc,
            store: cache.clone(),
            base_url: "https://graph.microsoft.com/v1.0".into(),
            client_state: String::new(),
        }));
    }
    let tools = Arc::new(Tools {
        client,
        bindings: config.secrets.clone(),
        repositories: map.repositories.clone(),
        store: cache,
        graph: graph.as_ref().unwrap().clone(),
    });
    Ok(Recorder {
        config,
        map,
        db,
        tools,
        llm: Arc::new(llm),
        redactor,
    })
}
fn day(state: &DesktopState, value: Option<&str>) -> Result<NaiveDate> {
    let settings = read_config(&state.config_path)?.activity_registration;
    settings.validate()?;
    let today = settings.zoned(Utc::now()).date_naive();
    let day = value
        .map(str::parse::<NaiveDate>)
        .transpose()?
        .unwrap_or(today);
    Ok(day)
}
pub(super) async fn inspect(
    state: &DesktopState,
    date: Option<&str>,
    pending: bool,
) -> Result<Value> {
    recover(state).await?;
    let day = day(state, date)?.to_string();
    let db = database(state)?;
    let entries = if pending {
        db.pending()?
    } else {
        db.entries(Some(&day))?
    };
    let entries: Vec<_> = entries
        .into_iter()
        .filter(|e| !pending || e.state == "pending" || e.state == "refining")
        .map(|mut e| {
            e.evidence.clear();
            e
        })
        .collect();
    Ok(
        json!({"day":day,"running":active(state).await,"entries":entries,"runs":db.runs(Some(&day))?,"summary":db.summary(&day)?}),
    )
}
pub(super) async fn run(host: &Arc<Host>, date: Option<&str>, slot: Option<&str>) -> Result<Value> {
    recover(&host.state).await?;
    let mut task = host.state.activity_task.lock().await;
    ensure!(
        task.as_ref().is_none_or(|t| t.is_finished()),
        "an activity registration is already running"
    );
    let recorder = recorder(&host.state).await?;
    let day = day(&host.state, date)?;
    let today = recorder
        .config
        .activity_registration
        .zoned(Utc::now())
        .date_naive();
    ensure!(
        day <= today && (today - day).num_days() < 31,
        "choose a day within the last 31 days"
    );
    let Some(id) = recorder.db.claim_run(slot, &day.to_string())? else {
        return Ok(json!({"already_run":true}));
    };
    // Failure to display an OS notification never prevents an already authorized run.
    let _ = host.shell.notify_activity(
        "Registering work",
        &format!("Reviewing your activity for {day}."),
        None,
    );
    let h = host.clone();
    let run_id = id.clone();
    *task = Some(tokio::spawn(async move {
        let result = recorder.run(&run_id, day).await;
        let _ = recorder.db.finish_run(&run_id, result.as_ref().ok());
        let (title,body)=match result {Ok(summary) if summary.pending>0=>("Activity details needed",format!("{} activities need context or an HU. Open Activity registration to review them.",summary.pending)),Ok(summary)=>("Activity registration finished",format!("{} tasks created; {:.2} hours remain for {day}.",summary.created,summary.remaining_hours)),Err(_)=>("Activity registration failed","Check the selected sources, credentials and task fields. No uncertain write is retried.".into())};
        let _ = h.shell.notify_activity(title, &body, None);
    }));
    Ok(json!({"id":id,"day":day.to_string(),"running":true}))
}
pub(super) async fn resolve(host: &Arc<Host>, id: String, resolution: Resolution) -> Result<Value> {
    recover(&host.state).await?;
    let mut task = host.state.activity_task.lock().await;
    ensure!(
        task.as_ref().is_none_or(|t| t.is_finished()),
        "wait for the current activity operation"
    );
    let recorder = recorder(&host.state).await?;
    let entry = recorder.db.entry(&id)?.context("activity not found")?;
    resolution.validate(&recorder.redactor)?;
    ensure!(
        entry.state == "pending",
        "refresh: this activity is no longer pending"
    );
    let h = host.clone();
    let entry_id = id.clone();
    *task = Some(tokio::spawn(async move {
        let result = recorder.resolve(&entry_id, resolution).await;
        let body = match result {
            Ok(e) if e.state == "done" => "Task created in Done.",
            Ok(e) if e.state == "dismissed" => "Activity dismissed.",
            Ok(_) => "Review the updated suggestion and any required fields.",
            Err(_) => {
                "The operation did not complete. Refresh and check the activity status before proceeding."
            }
        };
        let _ = h
            .shell
            .notify_activity("Activity registration", body, Some(&entry_id));
    }));
    Ok(json!({"id":id,"running":true}))
}
pub(super) async fn shutdown(state: &DesktopState) {
    if let Some(task) = state.activity_task.lock().await.take() {
        task.abort();
        let _ = task.await;
    }
    if let Ok(db) = database(state) {
        let _ = db.recover();
    }
}
/// Settings are re-read at every tick. The host must stay on for scheduled runs.
pub(super) async fn schedule(host: Arc<Host>) {
    loop {
        {
            let _guard = host.state.operations.lock().await;
            let no_login = host
                .state
                .microsoft_login
                .lock()
                .await
                .as_ref()
                .is_none_or(|t| t.is_finished())
                && host
                    .state
                    .github_finish
                    .lock()
                    .await
                    .as_ref()
                    .is_none_or(|t| t.is_finished());
            if no_login
                && !active(&host.state).await
                && let Ok(config) = read_config(&host.state.config_path)
                && let Ok(Some(slot)) = config.activity_registration.slot(Utc::now())
                && run(&host, None, Some(&slot)).await.is_err()
            {
                tracing::warn!(event = "activity_schedule_not_ready");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}
