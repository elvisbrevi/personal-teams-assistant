//! Host-owned read-only daily task snapshots, independent of Teams and language models.
use super::*;
use crate::{
    activity::{Summary, store::Database},
    tools::{ToolSpec, tasks::Day},
};
use chrono::{NaiveDate, Utc};
use serde_json::{Value, json};

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
fn day(state: &DesktopState, value: Option<&str>) -> Result<NaiveDate> {
    let settings = read_config(&state.config_path)?.activity_registration;
    settings.validate()?;
    let today = settings.zoned(Utc::now()).date_naive();
    Ok(value
        .map(str::parse::<NaiveDate>)
        .transpose()?
        .unwrap_or(today))
}
pub(super) async fn inspect(
    state: &DesktopState,
    date: Option<&str>,
    from: Option<&str>,
    pending: bool,
) -> Result<Value> {
    recover(state).await?;
    let config = read_config(&state.config_path)?;
    let until = day(state, date)?.to_string();
    let since = from
        .unwrap_or("0001-01-01")
        .parse::<NaiveDate>()?
        .to_string();
    ensure!(
        since.as_str() <= until.as_str(),
        Rejected::Input("The first day must not be after the last one.")
    );
    let db = database(state)?;
    let days = if pending {
        vec![]
    } else {
        db.days(&since, &until)?
    };
    let record = days.iter().find(|d| d.day == until);
    let summaries: Vec<_> = days.iter().map(|d| json!({"day":d.day,"tasks":d.tasks.len(),"hours":d.hours,"target_hours":d.target_hours,"saved_at":d.saved_at,"partial":d.partial})).collect();
    Ok(
        json!({"day":until,"running":active(state).await,"days":summaries,"record":record,"entries":[],"runs":db.runs(Some(&until))?,"last_run":db.runs(None)?.first(),"summary":db.summary(&until)?,"settings":config.activity_registration}),
    )
}
pub(super) async fn run(
    host: &Arc<Host>,
    date: Option<&str>,
    slot: Option<&str>,
    context: &str,
) -> Result<Value> {
    ensure!(
        authority::invocation(&host.state)?.scope.admitted,
        "{}",
        authority::EXECUTION_DENIED
    );
    recover(&host.state).await?;
    ensure!(
        context.trim().is_empty(),
        Rejected::Input("Activity context is retired. This operation only saves existing tasks.")
    );
    let mut task = host.state.activity_task.lock().await;
    ensure!(
        task.as_ref().is_none_or(|t| t.is_finished()),
        Rejected::Conflict("An activity operation is already running. Wait for it to finish.")
    );
    let config = read_config(&host.state.config_path)?;
    let map = KnowledgeMap::load(&host.state.map_path)?;
    let selected: Vec<_> = map
        .resources
        .iter()
        .filter(|r| {
            r.enabled
                && (config.activity_registration.sources.is_empty()
                    || config.activity_registration.sources.contains(&r.id))
                && matches!(
                    r.access,
                    Access::Tool {
                        tool: ToolSpec::AzureDevopsStatus { .. }
                    }
                )
        })
        .map(|r| r.id.clone())
        .collect();
    ensure!(
        !selected.is_empty(),
        Rejected::Input("Select an enabled Azure DevOps source for daily tasks.")
    );
    let date = day(&host.state, date)?;
    ensure!(
        date <= config.activity_registration.zoned(Utc::now()).date_naive(),
        Rejected::Input("Choose today or an earlier day.")
    );
    let db = database(&host.state)?;
    let Some(id) = db.claim_run(slot, &date.to_string())? else {
        return Ok(json!({"already_run":true}));
    };
    db.describe_run(
        &id,
        &json!({"stage":"Reading existing tasks","settings":config.activity_registration}),
    )?;
    let h = host.clone();
    let run_id = id.clone();
    *task = Some(tokio::spawn(async move {
        let result: Result<Day> = async {
            let mut day = Day {day:date.to_string(),time_zone:config.activity_registration.time_zone.clone(),target_hours:config.activity_registration.daily_hours,..Default::default()};
            let mut seen = std::collections::BTreeSet::new();
            for source in selected {
                ensure!(authority::invocation(&h.state)?.scope.admitted, "Account binding changed.");
                let value = tools::todo::read(&h.state, "list_azure_tasks", &json!({"day":date.to_string(),"source":source})).await.map_err(|_| anyhow::anyhow!("Existing Azure tasks could not be read. Check the selected sources and read credentials; the previous record was retained."))?;
                let read: Day = serde_json::from_value(value)?;
                for task in read.tasks {
                    if seen.insert(task.item.url.clone()) { day.hours += task.hours; day.tasks.push(task); }
                }
            }
            day.saved_at = Utc::now().timestamp();
            ensure!(authority::invocation(&h.state)?.scope.admitted, "Account binding changed.");
            db.save_day(&day)?;
            Ok(day)
        }.await;
        if !authority::invocation(&h.state).is_ok_and(|i| i.scope.admitted) {
            return;
        }
        let language = config.llm.language;
        let (summary, error, body) = match result {
            Ok(day) => {
                let remaining = (day.target_hours - day.hours).max(0.);
                let warnings = if remaining > 0. {
                    vec![format!(
                        "Daily target not reached: {:.2} of {:.2} hours. The day's tasks were saved.",
                        day.hours, day.target_hours
                    )]
                } else {
                    vec![]
                };
                let summary = Summary {
                    recorded_hours: day.hours,
                    remaining_hours: remaining,
                    warnings,
                    ..Default::default()
                };
                let _ = db.save_summary(&day.day, &summary);
                let body = language.say(
                    format!("Saved {} tasks for {}.", day.tasks.len(), day.day),
                    format!("Se guardaron {} tareas del {}.", day.tasks.len(), day.day),
                );
                (Some(summary), String::new(), body)
            }
            Err(_) => {
                let error = "Existing Azure tasks could not be saved. Check the selected sources and read credentials; the previous record was retained.".to_owned();
                let body = language.say(error.clone(),"No se pudieron guardar las tareas de Azure. Revisa las fuentes y credenciales de lectura; se conservó el registro anterior.");
                (None, error, body)
            }
        };
        let _ = db.finish_run(&run_id, summary.as_ref(), &error);
        let _ =
            h.shell
                .notify_activity(&language.say("Daily tasks", "Tareas del día"), &body, None);
    }));
    Ok(json!({"id":id,"day":date.to_string(),"running":true}))
}
pub(super) async fn resolve(_host: &Arc<Host>, _id: String, _resolution: Value) -> Result<Value> {
    anyhow::bail!(Rejected::Input(
        "Task creation is retired. Activities only saves existing tasks."
    ))
}
pub(super) async fn shutdown(state: &DesktopState) {
    if let Some(task) = state.activity_task.lock().await.take() {
        task.abort();
        let _ = task.await;
    }
    if authority::invocation(state).is_ok_and(|i| i.scope.admitted)
        && let Ok(db) = database(state)
    {
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
            if authority::invocation(&host.state).is_ok_and(|i| i.scope.admitted)
                && no_login
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
