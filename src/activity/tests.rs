use super::*;
use crate::{
    ado::{
        link::LinkOffer,
        registration::{Field, TaskSchema},
    },
    knowledge::Resource,
    llm::GenerationInput,
};
use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

fn parent() -> Target {
    Target {
        id: 40,
        title: "Payments documentation".into(),
        organization: "https://dev.azure.com/example".into(),
        project: "Payments".into(),
        kind: "User Story".into(),
        state: "Active".into(),
        url: "https://dev.azure.com/example/Payments/_workitems/edit/40".into(),
        ..Default::default()
    }
}
fn schema(required: bool) -> TaskSchema {
    let mut fields: Vec<_> = [
        "System.Title",
        "System.Description",
        "System.State",
        "System.AssignedTo",
        "System.AreaPath",
        "System.IterationPath",
        "System.Tags",
        "Microsoft.VSTS.Scheduling.CompletedWork",
        "Microsoft.VSTS.Scheduling.RemainingWork",
    ]
    .into_iter()
    .map(|name| Field {
        name: name.into(),
        reference: name.into(),
        ..Default::default()
    })
    .collect();
    if required {
        fields.push(Field {
            name: "Activity".into(),
            reference: "Custom.Activity".into(),
            required: true,
            ..Default::default()
        });
    }
    TaskSchema {
        fields,
        completed_states: vec!["Done".into()],
    }
}
struct Model;
#[async_trait]
impl LlmProvider for Model {
    fn name(&self) -> &str {
        "test"
    }
    async fn generate(&self, _: GenerationInput<'_>) -> Result<String> {
        Ok(String::new())
    }
    async fn plan_activity_tasks(
        &self,
        activities: &[Linkable],
        _: &[Target],
        _: &str,
        _: f64,
        context: &str,
    ) -> Result<Vec<Draft>> {
        Ok(activities
            .iter()
            .map(|a| Draft {
                activities: vec![a.key.clone()],
                parent: Some(40),
                title: "Document payment flow".into(),
                description: "Created payment documentation based on the verified work.".into(),
                hours: Some(1.5),
                confidence: 0.95,
                question: if a.context_required && context.is_empty() {
                    "What was the call about, and did you attend?".into()
                } else {
                    String::new()
                },
                suggested_parents: vec![40],
                ignore: false,
            })
            .collect())
    }
}
struct TestTools {
    db: Arc<store::Database>,
    writes: AtomicUsize,
    existing: f64,
    required: bool,
    uncertain: bool,
    call: bool,
    activity_override: std::sync::Mutex<Option<Vec<Linkable>>>,
    registered_activities: std::sync::Mutex<Vec<Linkable>>,
    daily_error: std::sync::Mutex<Option<String>>,
    review_error: std::sync::Mutex<Option<String>>,
    review_days: AtomicI64,
}
#[async_trait]
impl ReadOnlyTool for TestTools {
    async fn execute(&self, _: &ToolSpec, _: &str, _: &str) -> Result<String> {
        unreachable!()
    }
    async fn review(&self, _: &ToolSpec, days: i64) -> Result<String> {
        self.review_days.store(days, Ordering::SeqCst);
        if let Some(error) = self.review_error.lock().unwrap().as_ref() {
            anyhow::bail!("{error}");
        }
        let at = Utc::now() - chrono::Duration::days(1);
        let mut activities = vec![Linkable {
            label: "Created Wiki payment guide".into(),
            short: "Payment guide".into(),
            date: at.date_naive().to_string(),
            occurred_at: at.to_rfc3339(),
            organization: parent().organization,
            url: "https://dev.azure.com/example/Payments/_wiki/wikis/docs?pagePath=payments".into(),
            ..Default::default()
        }];
        if self.call {
            activities.push(Linkable {
                label: "Call without a purpose".into(),
                short: "Call".into(),
                date: at.date_naive().to_string(),
                occurred_at: at.to_rfc3339(),
                organization: "https://teams.microsoft.com".into(),
                url: "https://teams.microsoft.com/l/message/chat/1".into(),
                context_required: true,
                ..Default::default()
            });
        }
        if let Some(custom) = self.activity_override.lock().unwrap().as_ref() {
            activities = custom.clone();
        }
        Ok(serde_json::to_string(&Evidence {
            text: "The user created the payment guide.".into(),
            links: Some(LinkOffer {
                activities,
                work_items: vec![parent()],
                ..Default::default()
            }),
            ..Default::default()
        })?)
    }
    async fn daily_work(
        &self,
        _: &ToolSpec,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
        _: &str,
        day: &str,
    ) -> Result<DailyWork> {
        if let Some(error) = self.daily_error.lock().unwrap().as_ref() {
            anyhow::bail!("{error}");
        }
        Ok(DailyWork {
            organization: parent().organization,
            day: day.into(),
            hours: self.existing,
            parents: vec![parent()],
            ..Default::default()
        })
    }
    async fn work_item(&self, _: &ToolSpec, id: u64) -> Result<Option<Target>> {
        Ok((id == 40).then(parent))
    }
    async fn task_fields(&self, _: &ToolSpec, _: &Target, _: &str) -> Result<TaskSchema> {
        Ok(schema(self.required))
    }
    async fn validate_task(
        &self,
        _: &ToolSpec,
        _: &Target,
        _: &TaskFields,
        _: &[Linkable],
    ) -> Result<bool> {
        Ok(true)
    }
    async fn register_task(
        &self,
        _: &ToolSpec,
        _: &Target,
        task: &TaskFields,
        activities: &[Linkable],
    ) -> Result<(LinkStatus, Option<u64>)> {
        assert_eq!(task.state, "Done");
        assert_eq!(task.hours, 1.5);
        assert!(
            self.db.entries(None)?.iter().any(|e| e.state == "sending"),
            "write must be checkpointed before the request"
        );
        self.registered_activities
            .lock()
            .unwrap()
            .extend_from_slice(activities);
        let index = self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(if self.uncertain {
            (LinkStatus::Uncertain, None)
        } else {
            (LinkStatus::Linked, Some(501 + index as u64))
        })
    }
}
fn recorder(
    dir: &std::path::Path,
    existing: f64,
    required: bool,
    uncertain: bool,
    call: bool,
) -> (Recorder, Arc<TestTools>) {
    let mut config = Config::desktop_template().unwrap();
    config.policy.dry_run = false;
    config.activity_registration = Settings {
        sources: vec!["activity".into()],
        write_source: "activity".into(),
        time_zone: "UTC".into(),
        ..Default::default()
    };
    let db = Arc::new(store::Database::open(&dir.join("activity.db")).unwrap());
    let tools = Arc::new(TestTools {
        db: db.clone(),
        writes: AtomicUsize::new(0),
        existing,
        required,
        uncertain,
        call,
        activity_override: std::sync::Mutex::new(None),
        registered_activities: std::sync::Mutex::new(Vec::new()),
        daily_error: std::sync::Mutex::new(None),
        review_error: std::sync::Mutex::new(None),
        review_days: AtomicI64::new(0),
    });
    let map = KnowledgeMap {
        repositories: BTreeMap::new(),
        resources: vec![Resource {
            id: "activity".into(),
            description: "Activity".into(),
            topics: vec![],
            enabled: true,
            external_processing: true,
            allowed_conversations: vec![],
            allowed_senders: vec![],
            access: Access::Tool {
                tool: ToolSpec::AzureDevopsStatus {
                    repository: "knowledge".into(),
                    path: "ado.toml".into(),
                    secret_ref: "secret://reader".into(),
                    link_secret_ref: Some("secret://writer".into()),
                },
            },
        }],
    };
    (
        Recorder {
            config,
            map,
            db,
            tools: tools.clone(),
            llm: Arc::new(Model),
            redactor: Arc::new(Redactor::new(&[], vec![]).unwrap()),
        },
        tools,
    )
}
fn yesterday() -> NaiveDate {
    (Utc::now() - chrono::Duration::days(1)).date_naive()
}

#[tokio::test]
async fn registration_uses_two_weeks_of_context_but_only_records_the_selected_day() {
    let dir = tempfile::tempdir().unwrap();
    let (r, tools) = recorder(dir.path(), 0., false, false, false);
    let activity = |day: NaiveDate, id| Linkable {
        organization: parent().organization,
        url: format!("https://dev.azure.com/example/Payments/_build/results?buildId={id}"),
        date: day.to_string(),
        occurred_at: day.and_hms_opt(9, 0, 0).unwrap().and_utc().to_rfc3339(),
        ..Default::default()
    };
    *tools.activity_override.lock().unwrap() = Some(vec![
        activity(yesterday(), 1),
        activity(yesterday() - chrono::Duration::days(7), 2),
    ]);
    let summary = r.run("daily-window", yesterday(), "").await.unwrap();
    assert_eq!(tools.review_days.load(Ordering::SeqCst), 14);
    assert_eq!(summary.created, 1);
    assert_eq!(
        tools.registered_activities.lock().unwrap()[0].date,
        yesterday().to_string()
    );
    *tools.daily_error.lock().unwrap() = Some("daily read must not run for an older day".into());
    let old_day = Utc::now().date_naive() - chrono::Duration::days(14);
    let error = r.run("outside-window", old_day, "").await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("choose a day within the last 14 days")
    );
    assert_eq!(tools.writes.load(Ordering::SeqCst), 1);
}

#[test]
fn unfinished_run_has_no_summary_before_and_after_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.db");
    let db = store::Database::open(&path).unwrap();
    let interrupted = db.claim_run(None, "2026-10-13").unwrap().unwrap();
    db.stage_run(&interrupted, "Planning tasks with the configured agents")
        .unwrap();
    let running = &db.runs(Some("2026-10-13")).unwrap()[0];
    assert_eq!(running["status"], "running");
    assert_eq!(running["summary"], Value::Null);
    drop(db);

    let db = store::Database::open(&path).unwrap();
    db.recover().unwrap();
    let recovered = &db.runs(None).unwrap()[0];
    assert_eq!(recovered["status"], "interrupted");
    assert_eq!(recovered["summary"], Value::Null);
    assert_eq!(
        recovered["diagnostics"]["stage"],
        "Planning tasks with the configured agents"
    );

    let completed = db.claim_run(None, "2026-10-13").unwrap().unwrap();
    let summary = Summary {
        recorded_hours: 1.5,
        remaining_hours: 7.5,
        pending: 3,
        ..Default::default()
    };
    db.finish_run(&completed, Some(&summary), "").unwrap();
    let finished = &db.runs(None).unwrap()[0];
    assert_eq!(finished["status"], "completed");
    assert_eq!(finished["summary"], serde_json::to_value(summary).unwrap());

    let failed = db.claim_run(None, "2026-10-13").unwrap().unwrap();
    db.finish_run(&failed, None, "read failed").unwrap();
    assert_eq!(db.runs(None).unwrap()[0]["summary"], Value::Null);
}

#[tokio::test]
async fn failed_daily_read_keeps_stage_error_and_context_after_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.db");
    // Upgrade the exact runs schema used by installations before diagnostics.
    let legacy = rusqlite::Connection::open(&path).unwrap();
    legacy.execute_batch("CREATE TABLE runs(id TEXT PRIMARY KEY,slot TEXT UNIQUE,day TEXT NOT NULL,status TEXT NOT NULL,summary TEXT NOT NULL DEFAULT '{}',created_at INTEGER NOT NULL);
        INSERT INTO runs(id,day,status,summary,created_at) VALUES('legacy','2026-10-01','failed','null',1);").unwrap();
    drop(legacy);
    let (mut r, tools) = recorder(dir.path(), 0., false, false, false);
    let secret = "hidden-reader-token";
    r.redactor = Arc::new(Redactor::new(&[], vec![secret.into()]).unwrap());
    *tools.daily_error.lock().unwrap() = Some(format!(
        "Azure DevOps read /example/_apis/teams returned HTTP 401 {secret}"
    ));
    let id =
        r.db.claim_run(None, &yesterday().to_string())
            .unwrap()
            .unwrap();
    let context = "Reviewed the payment guide for HU 40.";
    assert!(r.run(&id, yesterday(), context).await.is_err());
    assert_eq!(tools.writes.load(Ordering::SeqCst), 0);
    let db = store::Database::open(&path).unwrap();
    let runs = db.runs(None).unwrap();
    let run = runs.iter().find(|run| run["id"] == id).unwrap();
    assert_eq!(run["status"], "failed");
    assert_eq!(
        run["diagnostics"]["stage"],
        "Reading daily effort and open HUs"
    );
    assert_eq!(run["diagnostics"]["context"], context);
    let error = run["diagnostics"]["error"].as_str().unwrap();
    assert!(error.contains("HTTP 401"));
    assert!(error.contains("daily effort"));
    assert!(!error.contains(secret));
    assert!(run["diagnostics"]["finished_at"].as_i64().is_some());
    assert_eq!(
        runs.iter().find(|run| run["id"] == "legacy").unwrap()["diagnostics"],
        json!({})
    );
}

#[derive(Default)]
struct ContextModel {
    contexts: std::sync::Mutex<Vec<String>>,
    fail: bool,
}
#[async_trait]
impl LlmProvider for ContextModel {
    fn name(&self) -> &str {
        "test-agent"
    }
    async fn generate(&self, _: GenerationInput<'_>) -> Result<String> {
        unreachable!()
    }
    async fn plan_activity_tasks(
        &self,
        activities: &[Linkable],
        parents: &[Target],
        evidence: &str,
        remaining: f64,
        context: &str,
    ) -> Result<Vec<Draft>> {
        self.contexts.lock().unwrap().push(context.into());
        if self.fail {
            anyhow::bail!("agent unavailable");
        }
        Model
            .plan_activity_tasks(activities, parents, evidence, remaining, context)
            .await
    }
}

#[tokio::test]
async fn day_context_reaches_the_agent_and_survives_entry_refinement() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, tools) = recorder(dir.path(), 0., false, false, false);
    r.config.activity_registration.auto_register = false;
    let model = Arc::new(ContextModel::default());
    r.llm = model.clone();
    let context = "I spent one hour reviewing HU 40.";
    r.run("context", yesterday(), context).await.unwrap();
    assert_eq!(model.contexts.lock().unwrap()[0], context);
    let entry = r.db.pending().unwrap().remove(0);
    assert_eq!(entry.context, context);
    let resolution: Resolution =
        serde_json::from_value(json!({"action":"refine","context":"This was documentation work."}))
            .unwrap();
    r.resolve(&entry.id, resolution).await.unwrap();
    assert!(model.contexts.lock().unwrap()[1].contains(context));
    assert_eq!(tools.writes.load(Ordering::SeqCst), 0);
    assert!(
        r.run("too-long", yesterday(), &"x".repeat(6001))
            .await
            .is_err()
    );
    r.redactor = Arc::new(Redactor::new(&[], vec!["private-token".into()]).unwrap());
    assert!(
        r.run("sensitive", yesterday(), "private-token")
            .await
            .is_err()
    );
    assert_eq!(model.contexts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn source_and_agent_errors_remain_visible_as_partial_coverage() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, tools) = recorder(dir.path(), 0., false, false, false);
    *tools.review_error.lock().unwrap() = Some("Azure DevOps read returned HTTP 403".into());
    let summary = r.run("source-error", yesterday(), "").await.unwrap();
    assert!(summary.partial);
    assert!(summary.warnings[0].contains("activity"));
    assert!(summary.warnings[0].contains("HTTP 403"));
    *tools.review_error.lock().unwrap() = None;
    r.llm = Arc::new(ContextModel {
        fail: true,
        ..Default::default()
    });
    let summary = r.run("agent-error", yesterday(), "HU 40").await.unwrap();
    assert!(summary.partial);
    assert!(summary.warnings[0].contains("test-agent"));
    assert!(summary.warnings[0].contains("agent unavailable"));
    assert_eq!(summary.pending, 1);
    assert_eq!(tools.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn commits_prs_pipelines_releases_and_approvals_register_once_with_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let (r, tools) = recorder(dir.path(), 0., false, false, false);
    let at = Utc::now() - chrono::Duration::days(1);
    let activities: Vec<_> = [
        (
            "Own commit",
            "_git/payments/commit/abcdef12",
            Some(("vstfs:///Git/Commit/P%2Frepo%2Fabcdef12", "Fixed in Commit")),
        ),
        (
            "Own pull request",
            "_git/payments/pullrequest/12",
            Some(("vstfs:///Git/PullRequestId/P%2Frepo%2F12", "Pull Request")),
        ),
        (
            "Pipeline run queued by the user",
            "_build/results?buildId=12",
            Some(("vstfs:///Build/Build/12", "Build")),
        ),
        (
            "Release created by the user",
            "_releaseProgress?releaseId=12",
            None,
        ),
        (
            "Release stage approved by the user",
            "_releaseProgress?releaseId=13",
            None,
        ),
    ]
    .into_iter()
    .map(|(label, path, artifact)| Linkable {
        label: label.into(),
        short: label.into(),
        url: format!("https://dev.azure.com/example/Payments/{path}"),
        organization: parent().organization,
        date: at.date_naive().to_string(),
        occurred_at: at.to_rfc3339(),
        artifact: artifact.map(|(url, _)| url.into()),
        link_name: artifact.map(|(_, name)| name.into()),
        ..Default::default()
    })
    .collect();
    *tools.activity_override.lock().unwrap() = Some(activities.clone());
    let summary = r.run("all-activity-kinds", yesterday(), "").await.unwrap();
    assert_eq!(summary.created, 5);
    assert_eq!(summary.recorded_hours, 7.5);
    assert_eq!(summary.remaining_hours, 1.5);
    let entries = r.db.entries(Some(&yesterday().to_string())).unwrap();
    assert_eq!(entries.len(), 5);
    assert!(
        entries
            .iter()
            .all(|e| e.state == "done" && e.draft.parent == Some(40))
    );
    assert_eq!(
        entries
            .iter()
            .filter_map(|e| e.task_id)
            .collect::<BTreeSet<_>>()
            .len(),
        5
    );
    let written = tools.registered_activities.lock().unwrap().clone();
    for activity in &activities {
        assert!(
            written.iter().any(|a| a.url == activity.url
                && a.artifact == activity.artifact
                && a.link_name == activity.link_name),
            "preserve the verified evidence relation"
        );
    }
    r.run("repeat-all-activity-kinds", yesterday(), "")
        .await
        .unwrap();
    assert_eq!(tools.writes.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn automatic_wiki_task_is_done_once_and_call_stays_pending() {
    let dir = tempfile::tempdir().unwrap();
    let (r, tools) = recorder(dir.path(), 3., false, false, true);
    let summary = r.run("first", yesterday(), "").await.unwrap();
    assert_eq!(tools.writes.load(Ordering::SeqCst), 1);
    assert_eq!(summary.recorded_hours, 4.5);
    assert_eq!(summary.remaining_hours, 4.5);
    assert_eq!(summary.pending, 1);
    r.run("again", yesterday(), "").await.unwrap();
    assert_eq!(tools.writes.load(Ordering::SeqCst), 1);
    let pending =
        r.db.entries(None)
            .unwrap()
            .into_iter()
            .find(|e| e.state == "pending")
            .unwrap();
    let refined = r
        .resolve(
            &pending.id,
            Resolution {
                action: "refine".into(),
                context: "I attended for 1.5 hours and discussed payment documentation for HU 40."
                    .into(),
                parent: None,
                title: None,
                description: None,
                hours: None,
                fields: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
    assert!(refined.draft.question.is_empty());
    assert_eq!(refined.state, "pending");
    r.resolve(
        &pending.id,
        Resolution {
            action: "create".into(),
            context: String::new(),
            parent: None,
            title: None,
            description: None,
            hours: None,
            fields: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(tools.writes.load(Ordering::SeqCst), 2);
    let totals = r.db.summary(&yesterday().to_string()).unwrap().unwrap();
    assert_eq!(totals.remaining_hours, 3.);
    assert_eq!(totals.pending, 0);
}

#[tokio::test]
async fn recovery_keeps_attempts_reserved_and_restores_interrupted_questions() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, tools) = recorder(dir.path(), 0., false, false, true);
    r.config.activity_registration.auto_register = false;
    r.run("before_restart", yesterday(), "").await.unwrap();
    let mut entries = r.db.pending().unwrap();
    entries[0].state = "sending".into();
    r.db.update(&entries[0], "pending").unwrap();
    entries[1].state = "refining".into();
    r.db.update(&entries[1], "pending").unwrap();
    r.db.recover().unwrap();
    assert_eq!(r.db.pending().unwrap().len(), 1);
    assert_eq!(
        r.db.entry(&entries[0].id).unwrap().unwrap().state,
        "uncertain"
    );
    assert_eq!(
        r.db.entry(&entries[1].id).unwrap().unwrap().state,
        "pending"
    );
    let summary = r.run("after_restart", yesterday(), "").await.unwrap();
    assert_eq!(summary.remaining_hours, 7.5);
    assert_eq!(tools.writes.load(Ordering::SeqCst), 0);
}

#[test]
fn registration_checks_both_artifact_and_browser_links() {
    let activity = Linkable {
        url: "https://dev.azure.com/example/P/_git/repo/commit/abc".into(),
        artifact: Some("vstfs:///Git/Commit/P%2Frepo%2Fabc".into()),
        ..Default::default()
    };
    let mut work = DailyWork::default();
    work.linked_urls.insert(activity.artifact.clone().unwrap());
    assert!(already_linked(&activity, &work));
    work.linked_urls.clear();
    work.linked_urls.insert(activity.url.clone());
    assert!(already_linked(&activity, &work));
}
#[tokio::test]
async fn insufficient_hours_required_fields_and_observation_prevent_writes() {
    for (existing, required, observe) in [(8., false, false), (0., true, false), (0., false, true)]
    {
        let dir = tempfile::tempdir().unwrap();
        let (mut r, tools) = recorder(dir.path(), existing, required, false, false);
        r.config.policy.dry_run = observe;
        r.run("test", yesterday(), "").await.unwrap();
        assert_eq!(tools.writes.load(Ordering::SeqCst), 0);
        assert_eq!(r.db.entries(None).unwrap()[0].state, "pending");
    }
}
#[tokio::test]
async fn uncertain_creation_is_reserved_and_never_repeated_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (r, tools) = recorder(dir.path(), 0., false, true, false);
    let summary = r.run("first", yesterday(), "").await.unwrap();
    assert_eq!(summary.remaining_hours, 7.5);
    r.db.recover().unwrap();
    r.run("restart", yesterday(), "").await.unwrap();
    assert_eq!(tools.writes.load(Ordering::SeqCst), 1);
    assert_eq!(r.db.entries(None).unwrap()[0].state, "uncertain");
}
#[test]
fn schedule_honors_zone_window_weekdays_and_durable_slots() {
    let mut s = Settings {
        enabled: true,
        time_zone: "America/Santiago".into(),
        sources: vec!["activity".into()],
        write_source: "activity".into(),
        schedule: "interval".into(),
        interval_minutes: 60,
        ..Default::default()
    };
    s.validate().unwrap();
    let at = |time: &str| {
        DateTime::parse_from_rfc3339(time)
            .unwrap()
            .with_timezone(&Utc)
    };
    assert!(s.slot(at("2026-10-05T08:59:00-03:00")).unwrap().is_none());
    let first = s.slot(at("2026-10-05T10:20:00-03:00")).unwrap().unwrap();
    assert!(first.ends_with("10:00"));
    assert_eq!(
        Some(first.clone()),
        s.slot(at("2026-10-05T10:40:00-03:00")).unwrap()
    );
    assert!(s.slot(at("2026-10-05T18:01:00-03:00")).unwrap().is_none());
    assert!(s.slot(at("2026-10-04T10:00:00-03:00")).unwrap().is_none());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = store::Database::open(&path).unwrap();
    assert!(db.claim_run(Some(&first), "2026-10-05").unwrap().is_some());
    drop(db);
    let db = store::Database::open(&path).unwrap();
    db.recover().unwrap();
    assert!(db.claim_run(Some(&first), "2026-10-05").unwrap().is_none());
    s.schedule = "daily".into();
    assert!(s.slot(at("2026-10-05T17:59:00-03:00")).unwrap().is_none());
    assert!(s.slot(at("2026-10-05T18:01:00-03:00")).unwrap().is_some());
    let zone = Settings {
        time_zone: "America/New_York".into(),
        ..Default::default()
    };
    let (start, end) = zone.bounds("2026-03-08".parse().unwrap()).unwrap();
    assert_eq!((end - start).num_hours(), 23);
}
#[test]
fn legacy_profiles_get_disabled_registration_and_invalid_fields_are_rejected() {
    let text = include_str!("../../config.example.toml");
    let legacy = text.split("[activity_registration]").next().unwrap();
    let config: Config = toml::from_str(legacy).unwrap();
    assert!(!config.activity_registration.enabled);
    assert_eq!(config.activity_registration.daily_hours, 9.);
    let mut s = Settings::default();
    s.extra_fields
        .insert("../relations".into(), json!("unsafe"));
    assert!(s.validate().is_err());
    s.extra_fields.clear();
    s.effort_field = "Microsoft.VSTS.Scheduling.RemainingWork".into();
    assert!(
        s.validate().is_err(),
        "remaining effort cannot record completed work"
    );
}
