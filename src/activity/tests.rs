use super::*;
use crate::config::Config;
use serde_json::json;

#[test]
fn schedule_honors_zone_window_weekdays_and_durable_slots() {
    let mut s = Settings {
        enabled: true,
        time_zone: "America/Santiago".into(),
        sources: vec!["activity".into()],
        write_source: String::new(),
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

#[test]
fn daily_snapshots_replace_atomically_keep_legacy_data_and_supply_verified_recap_links() {
    use crate::tools::{
        tasks::{Day, Task},
        todo::Item,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity-registration.db");
    let db = store::Database::open(&path).unwrap();
    let day = "2026-10-14";
    let task = Task {
        item: Item {
            id: 7,
            title: "Reviewed receipts".into(),
            url: "https://dev.azure.com/example/Payments/_workitems/edit/7".into(),
            project: "Payments".into(),
            state: "Done".into(),
            kind: "Task".into(),
            created_at: None,
            dates: vec![],
        },
        source: "ado".into(),
        organization: "https://dev.azure.com/example".into(),
        user: "me@example.test".into(),
        description: "Verified receipt output.".into(),
        hours: 30.,
        parent: None,
    };
    let record = Day {
        day: day.into(),
        tasks: vec![task],
        hours: 30.,
        target_hours: 9.,
        saved_at: 1,
        ..Default::default()
    };
    db.save_day(&record).unwrap();
    db.save_day(&record).unwrap();
    drop(db);
    let db = store::Database::open(&path).unwrap();
    assert_eq!(db.days(day, day).unwrap()[0].tasks.len(), 1);
    let entries = store::Database::recorded(&path, day, day).unwrap();
    let evidence = recorded::evidence(&entries, &[], day, day).unwrap();
    assert!(
        evidence
            .text
            .contains("Recorded task #7: «Reviewed receipts» (30 h)"),
        "{}",
        evidence.text
    );
    assert_eq!(
        evidence.references.len(),
        1,
        "tasks without a parent still get a verified task link"
    );
    evidence.references[0].validate().unwrap();
    let mut related = record.clone();
    let mut parent = related.tasks[0].item.clone();
    parent.id = 50;
    parent.project = "Other".into();
    parent.url = "https://dev.azure.com/example/Other/_workitems/edit/50".into();
    related.tasks[0].parent = Some(parent);
    db.save_day(&related).unwrap();
    let entries = store::Database::recorded(&path, day, day).unwrap();
    let linked = recorded::evidence(&entries, &[], day, day).unwrap();
    assert_eq!(
        linked.references.len(),
        2,
        "cross-project parents retain both verified links"
    );
    for reference in &linked.references {
        reference.validate().unwrap();
    }
    db.save_day(&Day {
        day: day.into(),
        saved_at: 2,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(db.days(day, day).unwrap().len(), 1);
    let empty = store::Database::recorded(&path, day, day).unwrap();
    assert!(
        recorded::evidence(&empty, &[], day, day)
            .unwrap()
            .text
            .contains("No tasks were recorded")
    );
    assert!(
        store::Database::recorded(&path, "2026-10-13", "2026-10-13")
            .unwrap()
            .is_empty()
    );
}
