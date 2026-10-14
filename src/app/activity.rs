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
    let config = read_config(&state.config_path)?;
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
        json!({"day":day,"running":active(state).await,"entries":entries,"runs":db.runs(Some(&day))?,"summary":db.summary(&day)?,"settings":config.activity_registration,"llm":config.llm,"observation_mode":config.policy.dry_run}),
    )
}
pub(super) async fn run(
    host: &Arc<Host>,
    date: Option<&str>,
    slot: Option<&str>,
    context: &str,
) -> Result<Value> {
    recover(&host.state).await?;
    let mut task = host.state.activity_task.lock().await;
    ensure!(
        task.as_ref().is_none_or(|t| t.is_finished()),
        Rejected::Conflict("An activity operation is already running. Wait for it to finish.")
    );
    let recorder = recorder(&host.state).await?;
    crate::activity::validate_context(context, &recorder.redactor).map_err(|_| {
        Rejected::Input("The context must be at most 6000 bytes and contain no sensitive data.")
    })?;
    let day = day(&host.state, date)?;
    let today = recorder
        .config
        .activity_registration
        .zoned(Utc::now())
        .date_naive();
    ensure!(
        day <= today && (today - day).num_days() < crate::ado::registration::LOOKBACK_DAYS,
        Rejected::Input("Choose a day within the last 14 days.")
    );
    let Some(id) = recorder.db.claim_run(slot, &day.to_string())? else {
        return Ok(json!({"already_run":true}));
    };
    // Failure to display an OS notification never prevents an already authorized run.
    let language = recorder.config.llm.language;
    let _ = host.shell.notify_activity(
        &language.say("Registering work", "Registrando trabajo"),
        &language.say(
            format!("Reviewing your activity for {day}."),
            format!("Revisando tu actividad del {day}."),
        ),
        None,
    );
    let h = host.clone();
    let run_id = id.clone();
    let context = context.trim().to_owned();
    *task = Some(tokio::spawn(async move {
        let result = recorder.run(&run_id, day, &context).await;
        let (title, body) = match result {
            Ok(summary) if summary.pending > 0 => (
                language.say("Activity details needed", "Faltan detalles de actividades"),
                language.say(
                    format!(
                        "{} activities need context or an HU. Open Activities to review them.",
                        summary.pending
                    ),
                    format!(
                        "{} actividades necesitan contexto o una HU. Abre Actividades para revisarlas.",
                        summary.pending
                    ),
                ),
            ),
            Ok(summary) => (
                language.say("Activity registration finished", "Registro de actividades terminado"),
                language.say(
                    format!(
                        "{} tasks created; {:.2} hours remain for {day}.",
                        summary.created, summary.remaining_hours
                    ),
                    format!(
                        "{} tareas creadas; quedan {:.2} horas para el {day}.",
                        summary.created, summary.remaining_hours
                    ),
                ),
            ),
            Err(error) => (
                language.say("Activity registration failed", "El registro de actividades falló"),
                recorder.error_detail(&error),
            ),
        };
        let _ = h.shell.notify_activity(&title, &body, None);
    }));
    Ok(json!({"id":id,"day":day.to_string(),"running":true}))
}
pub(super) async fn resolve(host: &Arc<Host>, id: String, resolution: Resolution) -> Result<Value> {
    recover(&host.state).await?;
    let mut task = host.state.activity_task.lock().await;
    ensure!(
        task.as_ref().is_none_or(|t| t.is_finished()),
        Rejected::Conflict("An activity operation is already running. Wait for it to finish.")
    );
    let recorder = recorder(&host.state).await?;
    let entry = recorder.db.entry(&id)?.ok_or(Rejected::Input(
        "The activity was not found. Refresh the list.",
    ))?;
    resolution
        .validate(&recorder.redactor)
        .map_err(|error| rejection(&error))?;
    ensure!(
        entry.state == "pending",
        Rejected::Conflict("This activity is no longer pending. Refresh the list.")
    );
    let h = host.clone();
    let entry_id = id.clone();
    let language = recorder.config.llm.language;
    *task = Some(tokio::spawn(async move {
        let result = recorder.resolve(&entry_id, resolution).await;
        let body = match result {
            Ok(e) if e.state == "done" => language.say(
                format!("Task #{} created in Done.", e.task_id.unwrap_or_default()),
                format!("Tarea #{} creada en Done.", e.task_id.unwrap_or_default()),
            ),
            Ok(e) if e.state == "dismissed" => {
                language.say("Activity dismissed.", "Actividad descartada.")
            }
            Ok(e) if !e.issue.is_empty() => e.issue,
            Ok(_) => language.say(
                "Review the updated suggestion and any required fields.",
                "Revisa la sugerencia actualizada y los campos obligatorios.",
            ),
            Err(error) => {
                let detail = recorder.error_detail(&error);
                if let Ok(Some(mut entry)) = recorder.db.entry(&entry_id)
                    && ["pending", "refining"].contains(&entry.state.as_str())
                {
                    let previous = entry.state.clone();
                    entry.state = "pending".into();
                    entry.issue = detail.clone();
                    let _ = recorder.db.update(&entry, &previous);
                }
                detail
            }
        };
        let _ = h.shell.notify_activity(
            &language.say("Activities", "Actividades"),
            &body,
            Some(&entry_id),
        );
    }));
    Ok(json!({"id":id,"running":true}))
}
/// The exact reason an activity resolution was refused, as a fixed text the GUI translates.
fn rejection(error: &anyhow::Error) -> Rejected {
    Rejected::Input(match error.to_string().as_str() {
        "task field too large" => {
            "A field value is too long (at most 8000 characters) or has control characters."
        }
        "task fields must be scalar values" => "Field values must be text, numbers or true/false.",
        "too many task fields" => "There are too many additional fields (at most 30).",
        "invalid additional task field" => {
            "An additional field name is invalid or reserved: the app sets the System fields."
        }
        "task fields contain sensitive data" => {
            "A field contains data that looks sensitive (an email address, phone number, ID number, token or password). Remove it and try again."
        }
        "invalid resolution action" => "The requested action is invalid.",
        _ => "The context must be at most 6000 bytes and contain no sensitive data.",
    })
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
                && run(&host, None, Some(&slot), "").await.is_err()
            {
                tracing::warn!(event = "activity_schedule_not_ready");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Resolution;

    #[test]
    fn a_refused_resolution_names_its_exact_reason() {
        let redactor = Redactor::new(&[], vec![]).unwrap();
        let resolution = |fields: serde_json::Value, context: &str| -> Resolution {
            serde_json::from_value(json!({"action":"create","context":context,"fields":fields}))
                .unwrap()
        };
        let reason = |r: Resolution| rejection(&r.validate(&redactor).unwrap_err()).to_string();
        assert!(
            reason(resolution(json!({"Custom.Evidence":"x".repeat(8001)}), ""))
                .contains("too long")
        );
        assert!(reason(resolution(json!({"Custom.Evidence":["a"]}), "")).contains("text, numbers"));
        assert!(reason(resolution(json!({"System.State":"Done"}), "")).contains("reserved"));
        assert!(
            reason(resolution(json!({"Custom.Contact":"ana@example.com"}), ""))
                .contains("sensitive")
        );
        assert!(reason(resolution(json!({}), &"x".repeat(6001))).contains("6000 bytes"));
        assert!(
            resolution(json!({"Custom.Evidence":"x".repeat(8000)}), "")
                .validate(&redactor)
                .is_ok()
        );
        // Custom fields of inherited processes may be named with a GUID.
        let guid = json!({"Custom.b505c83e-3745-4d8b-b76b-b3086a0c4c71":"<p>Done</p>"});
        assert!(resolution(guid, "").validate(&redactor).is_ok());
        assert!(reason(resolution(json!({"-Custom.X":"x"}), "")).contains("reserved"));
    }
}
