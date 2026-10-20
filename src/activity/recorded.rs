//! The work the Activities section recorded, by day, as context for status answers and
//! activity reviews: existing Azure tasks saved in daily snapshots, plus legacy records.
//! Legacy unregistered work still requires every originating source to be authorized;
//! recorded tasks require the activity record audience.
use super::Entry;
use crate::ado::link::{Linkable, Target};
use crate::evidence::{Block, Evidence, Reference};
use chrono::{Datelike, Duration, NaiveDate};

/// The source an activity was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Azure DevOps activity: commits, pull requests, runs, releases, approvals.
    Activity,
    Wiki,
    Teams,
}
/// Where an activity's verified link points; anything else has no known source.
pub fn origin(activity: &Linkable) -> Option<Origin> {
    let url = url::Url::parse(&activity.url).ok()?;
    match url.host_str()? {
        "teams.microsoft.com" => Some(Origin::Teams),
        "dev.azure.com" if url.path().contains("/_wiki/") => Some(Origin::Wiki),
        "dev.azure.com" => Some(Origin::Activity),
        _ => None,
    }
}

/// Characters of the whole section, and of a description or completion evidence.
const CHARS: usize = 5000;
const DETAIL: usize = 240;

fn short(text: &str, limit: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        out.push('…');
    }
    out
}
/// A verified work item link for `id` in `target`'s organization and project.
fn work_item(target: &Target, id: u64, label: String, url: &str) -> Option<Reference> {
    let reference = Reference {
        id: format!(
            "work_item:{}:{}:{id}",
            target.organization.trim_end_matches('/'),
            target.project
        ),
        kind: "work_item".into(),
        label,
        url: url.into(),
        organization: target.organization.trim_end_matches('/').into(),
        project: target.project.clone(),
        aliases: vec![format!("#{id}"), format!("work item {id}")],
        parent: None,
        revision: None,
        authority: None,
        author: None,
        author_role: None,
    };
    reference.validate().ok().map(|_| reference)
}

/// Whether an entry's work is in Azure DevOps: a task the app created (or may have created),
/// or work already linked to one.
fn registered(state: &str) -> bool {
    matches!(
        state,
        "recorded" | "recorded_empty" | "done" | "uncertain" | "already_registered"
    )
}
/// One line for an entry and the references it names. Registered tasks and their HU are
/// named by number only when their link verifies.
fn line(entry: &Entry) -> Option<(String, Vec<Reference>)> {
    if entry.state == "recorded_empty" {
        return Some(("- No tasks were recorded for this day.\n".into(), vec![]));
    }
    let registered = match entry.state.as_str() {
        state if registered(state) => true,
        "pending" | "refining" | "failed" => false,
        _ => return None,
    };
    let mut references = Vec::new();
    let parent = entry
        .parents
        .iter()
        .find(|p| Some(p.id) == entry.draft.parent);
    let hu = parent.and_then(|p| {
        let label = format!("#{} {} — {}", p.id, p.title, p.state);
        let reference = work_item(p, p.id, label, &p.url)?;
        references.push(reference);
        Some(format!(" under #{} «{}»", p.id, p.title))
    });
    let scope = if entry.state == "recorded" {
        entry.parents.iter().find(|p| Some(p.id) == entry.task_id)
    } else {
        parent
    };
    let task = match (entry.task_id, entry.task_url.as_deref(), scope) {
        (Some(id), Some(url), Some(parent)) => {
            work_item(parent, id, format!("#{id} {}", entry.draft.title), url).map(|r| {
                references.push(r);
                format!(" #{id}")
            })
        }
        _ => None,
    };
    let what = match (registered, entry.state.as_str()) {
        (true, "recorded") => format!("Recorded task{}", task.as_deref().unwrap_or("")),
        (true, "uncertain") => format!(
            "Registration as task{} uncertain",
            task.as_deref().unwrap_or("")
        ),
        (true, "already_registered") => "Already linked to a task".to_owned(),
        (true, _) => format!("Registered as task{}", task.as_deref().unwrap_or("")),
        (false, _) => "Detected, not registered yet".to_owned(),
    };
    let mut text = format!(
        "- {what}{}: «{}»",
        hu.as_deref().unwrap_or(""),
        short(&entry.draft.title, 160)
    );
    if let Some(hours) = entry.draft.hours {
        text.push_str(&format!(" ({hours} h)"));
    }
    let detail = if entry.draft.evidence.trim().is_empty() {
        &entry.draft.description
    } else {
        &entry.draft.evidence
    };
    if !detail.trim().is_empty() {
        text.push_str(&format!(": {}", short(detail, DETAIL)));
    }
    text.push('\n');
    Some((text, references))
}

/// The recorded work between `since` and `until` (yyyy-mm-dd, inclusive), newest day first.
/// A registered task shows as the user approved it and wrote it to Azure DevOps; work not
/// registered yet shows only when its activities all come from `allowed` sources, because its
/// text is drawn from them. `None` when nothing can be shown.
pub fn evidence(
    entries: &[Entry],
    allowed: &[Origin],
    since: &str,
    until: &str,
) -> Option<Evidence> {
    let mut shown: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.day.as_str() >= since && e.day.as_str() <= until)
        .filter(|e| {
            registered(&e.state)
                || (!e.activities.is_empty()
                    && e.activities
                        .iter()
                        .all(|a| origin(a).is_some_and(|o| allowed.contains(&o))))
        })
        .collect();
    shown.sort_by(|a, b| b.day.cmp(&a.day).then(b.updated_at.cmp(&a.updated_at)));
    let header = format!(
        "Work recorded in the app's Activities section, by day, from {since} to {until} (the user's own verified activity; recorded tasks are read from Azure DevOps; the app does not create tasks; days without lines have no recorded work):"
    );
    let mut evidence = Evidence {
        text: format!("{header}\n"),
        blocks: vec![Block {
            text: header,
            source_ids: Vec::new(),
        }],
        ..Default::default()
    };
    let mut any = false;
    let mut day = "";
    let mut block: Option<Block> = None;
    let mut left_out = false;
    for entry in shown {
        let Some((text, references)) = line(entry) else {
            continue;
        };
        if evidence.text.chars().count() + text.chars().count() + 12 > CHARS {
            left_out = true;
            break;
        }
        if entry.day != day {
            day = &entry.day;
            evidence.blocks.extend(block.take());
            evidence.text.push_str(&format!("{day}:\n"));
            block = Some(Block {
                text: format!("Activities section, {day}:\n"),
                source_ids: Vec::new(),
            });
        }
        any = true;
        evidence.text.push_str(&text);
        if let Some(block) = &mut block {
            block.text.push_str(&text);
            block
                .source_ids
                .extend(references.iter().map(|r| r.id.clone()));
        }
        for reference in references {
            if !evidence.references.iter().any(|r| r.id == reference.id) {
                evidence.references.push(reference);
            }
        }
    }
    evidence.blocks.extend(block.take());
    if !any {
        return None;
    }
    if left_out {
        let note = "Older recorded days were left out to fit.";
        evidence.text.push_str(note);
        evidence.text.push('\n');
        evidence.blocks.push(Block {
            text: note.into(),
            source_ids: Vec::new(),
        });
    }
    Some(evidence)
}

/// Month names in Spanish and English, with English abbreviations, by month number - 1.
const MONTHS: [&[&str]; 12] = [
    &["enero", "january", "jan"],
    &["febrero", "february", "feb"],
    &["marzo", "march", "mar"],
    &["abril", "april", "apr"],
    &["mayo", "may"],
    &["junio", "june", "jun"],
    &["julio", "july", "jul"],
    &["agosto", "august", "aug"],
    &["septiembre", "setiembre", "september", "sept", "sep"],
    &["octubre", "october", "oct"],
    &["noviembre", "november", "nov"],
    &["diciembre", "december", "dec"],
];
const WEEKDAYS: [&[&str]; 7] = [
    &["lunes", "monday"],
    &["martes", "tuesday"],
    &["miercoles", "wednesday"],
    &["jueves", "thursday"],
    &["viernes", "friday"],
    &["sabado", "saturday"],
    &["domingo", "sunday"],
];

/// The day or period a request names, in Spanish or English («ayer», «el martes», «el 10 de
/// octubre», «del 5 al 9 de octubre», «10/10», «la semana pasada», «en septiembre», "last 3
/// days", "October 10"), as inclusive dates in `today`'s calendar. A date without a year is
/// the latest one not after today; numeric dates are day/month. A period ends today at the
/// latest; `None` when the request names no past day.
pub fn requested_days(text: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let q: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' | 'º' | '°' => 'o',
            'ú' | 'ü' => 'u',
            c if c.is_alphanumeric() || c == '/' || c == '-' => c,
            _ => ' ',
        })
        .collect();
    let phrase = format!(" {} ", q.split_whitespace().collect::<Vec<_>>().join(" "));
    let month = |name: &str| {
        MONTHS
            .iter()
            .position(|m| m.contains(&name))
            .map(|i| i as u32 + 1)
    };
    let names = {
        let mut all: Vec<&str> = MONTHS.concat();
        all.sort_by_key(|n| std::cmp::Reverse(n.len()));
        all.join("|")
    };
    let year = |text: Option<regex::Match>| {
        text.and_then(|y| y.as_str().parse::<i32>().ok())
            .map(|y| if y < 100 { y + 2000 } else { y })
    };
    // A date without a year is the latest one that is not after today.
    let date = |year: Option<i32>, month: u32, day: u32| match year {
        Some(year) => NaiveDate::from_ymd_opt(year, month, day),
        None => NaiveDate::from_ymd_opt(today.year(), month, day)
            .filter(|d| *d <= today)
            .or_else(|| NaiveDate::from_ymd_opt(today.year() - 1, month, day)),
    };
    let number = |m: Option<regex::Match>| m.and_then(|m| m.as_str().parse::<u32>().ok());
    let mut days = Vec::new();
    let patterns = [
        // 2026-10-10
        r"\b(\d{4})-(\d{2})-(\d{2})\b".to_owned(),
        // 10/10, 10/10/2026: day first.
        r"\b(\d{1,2})/(\d{1,2})(?:/(\d{4}|\d{2}))?\b".to_owned(),
        // 10 de octubre, del 5 al 9 de octubre, 5 y el 9 de octubre de 2026
        format!(
            r"\b(\d{{1,2}})(?:\s*(?:al|a|y el|y|hasta el|hasta|-)\s*(\d{{1,2}}))?\s+de\s+({names})\b(?:\s+(?:de|del)\s+(\d{{4}}))?"
        ),
        // October 10, Oct 5-9, October 5 to 9, 2026
        format!(
            r"\b({names})\s+(\d{{1,2}})(?:st|nd|rd|th)?(?:\s*(?:-|to|and|through|until)\s*(\d{{1,2}})(?:st|nd|rd|th)?)?(?:\s+(\d{{4}}))?\b"
        ),
        // 10 October, 10th of October
        format!(r"\b(\d{{1,2}})(?:st|nd|rd|th)?\s+(?:of\s+)?({names})\b(?:\s+(\d{{4}}))?"),
    ];
    for (kind, pattern) in patterns.iter().enumerate() {
        let Ok(pattern) = regex::Regex::new(pattern) else {
            continue;
        };
        for c in pattern.captures_iter(&q) {
            let found = match kind {
                0 => vec![(year(c.get(1)), number(c.get(2)), number(c.get(3)))],
                1 => vec![(year(c.get(3)), number(c.get(2)), number(c.get(1)))],
                2 => {
                    let m = month(&c[3]);
                    vec![
                        (year(c.get(4)), m, number(c.get(1))),
                        (year(c.get(4)), m, number(c.get(2))),
                    ]
                }
                3 => {
                    let m = month(&c[1]);
                    vec![
                        (year(c.get(4)), m, number(c.get(2))),
                        (year(c.get(4)), m, number(c.get(3))),
                    ]
                }
                _ => vec![(year(c.get(3)), month(&c[2]), number(c.get(1)))],
            };
            days.extend(found.into_iter().filter_map(|(y, m, d)| date(y, m?, d?)));
        }
    }
    let has = |options: &[&str]| options.iter().any(|o| phrase.contains(&format!(" {o} ")));
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let month_start = |d: NaiveDate| d.with_day(1).unwrap_or(d);
    let period = if !days.is_empty() {
        days.iter().min().copied().zip(days.iter().max().copied())
    } else if let Some(c) = regex::Regex::new(r"\b(?:ultimos|ultimas|last|past)\s+(\d{1,3})\s+(dias|days|semanas|weeks)\b")
        .ok()
        .and_then(|r| r.captures(&q))
    {
        let n = number(c.get(1)).unwrap_or(1).max(1) as i64;
        let n = if c[2].starts_with('s') || c[2].starts_with('w') { n * 7 } else { n };
        Some((today - Duration::days(n - 1), today))
    } else if has(&["semana pasada", "semana anterior", "last week", "previous week"]) {
        Some((monday - Duration::days(7), monday - Duration::days(1)))
    } else if has(&["esta semana", "this week"]) {
        Some((monday, today))
    } else if has(&["mes pasado", "mes anterior", "last month", "previous month"]) {
        let start = month_start(month_start(today) - Duration::days(1));
        Some((start, month_start(today) - Duration::days(1)))
    } else if has(&["este mes", "this month"]) {
        Some((month_start(today), today))
    } else if let Some(c) = regex::Regex::new(&format!(
        r"\b(?:en|de|durante|in|during)\s+(?:el mes de\s+)?({names})\b(?:\s+(?:de|del)?\s*(\d{{4}}))?"
    ))
    .ok()
    .and_then(|r| r.captures(&q))
    {
        let start = month(&c[1]).and_then(|m| date(year(c.get(2)), m, 1));
        start.map(|start| {
            let next = start.checked_add_months(chrono::Months::new(1)).unwrap_or(start);
            (start, next - Duration::days(1))
        })
    } else {
        // Single days: today, yesterday, the day before, the last such weekday.
        let mut phrase = phrase.clone();
        let mut named = Vec::new();
        for (words, back) in [
            (&["antes de ayer", "anteayer", "antier", "day before yesterday"][..], 2),
            (&["ayer", "yesterday"][..], 1),
            (&["hoy", "today"][..], 0),
        ] {
            for word in words {
                let word = format!(" {word} ");
                if phrase.contains(&word) {
                    named.push(today - Duration::days(back));
                    phrase = phrase.replace(&word, " ");
                }
            }
        }
        for (index, words) in WEEKDAYS.iter().enumerate() {
            if words.iter().any(|w| phrase.contains(&format!(" {w} "))) {
                let back = (today.weekday().num_days_from_monday() as i64 - index as i64 + 6)
                    .rem_euclid(7)
                    + 1;
                named.push(today - Duration::days(back));
            }
        }
        named.iter().min().copied().zip(named.iter().max().copied())
    };
    let (since, until) = period?;
    let until = until.min(today);
    (since <= until).then_some((since, until))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(day: &str, state: &str, url: &str, task: Option<u64>) -> Entry {
        let hu = Target {
            id: 23553,
            title: "Pagos QR".into(),
            url: "https://dev.azure.com/example/Payments/_workitems/edit/23553".into(),
            organization: "https://dev.azure.com/example".into(),
            project: "Payments".into(),
            state: "Active".into(),
            ..Default::default()
        };
        Entry {
            id: format!("{day}-{state}-{url}"),
            run: "r".into(),
            day: day.into(),
            state: state.into(),
            draft: super::super::Draft {
                activities: vec!["a1".into()],
                parent: Some(23553),
                title: format!("Trabajo del {day}"),
                description: "Descripción del trabajo.".into(),
                evidence: String::new(),
                hours: Some(2.0),
                confidence: 0.9,
                question: String::new(),
                suggested_parents: Vec::new(),
                ignore: false,
            },
            activities: vec![Linkable {
                key: "a1".into(),
                url: url.into(),
                ..Default::default()
            }],
            parents: vec![hu],
            evidence: String::new(),
            context: String::new(),
            fields: Default::default(),
            issue: String::new(),
            task_id: task,
            task_url: task
                .map(|id| format!("https://dev.azure.com/example/Payments/_workitems/edit/{id}")),
            updated_at: 1,
            required: Vec::new(),
            date_attribution: String::new(),
        }
    }

    #[test]
    fn recorded_work_is_grouped_by_day_and_limited_to_authorized_sources() {
        let commit = "https://dev.azure.com/example/Payments/_git/api/commit/abc";
        let chat = "https://teams.microsoft.com/l/message/19:abc/1";
        let entries = [
            entry("2026-10-13", "done", commit, Some(26401)),
            entry("2026-10-14", "pending", commit, None),
            entry("2026-10-14", "done", chat, Some(26402)),
            entry("2026-10-11", "pending", chat, None),
            entry("2026-10-12", "dismissed", commit, None),
            entry("2026-08-01", "done", commit, Some(1)),
        ];
        let evidence = evidence(&entries, &[Origin::Activity], "2026-09-09", "2026-10-14").unwrap();
        // Newest day first; work not registered yet that came from a chat, a dismissed entry
        // and an old one are left out. A registered task shows wherever it came from.
        let days: Vec<&str> = evidence
            .blocks
            .iter()
            .skip(1)
            .map(|b| b.text.lines().next().unwrap())
            .collect();
        assert!(
            evidence.blocks[0]
                .text
                .starts_with("Work recorded in the app's Activities section")
        );
        assert_eq!(
            days,
            [
                "Activities section, 2026-10-14:",
                "Activities section, 2026-10-13:"
            ]
        );
        assert!(evidence.text.contains(
            "- Detected, not registered yet under #23553 «Pagos QR»: «Trabajo del 2026-10-14» (2 h): Descripción del trabajo."
        ));
        assert!(evidence.text.contains(
            "- Registered as task #26401 under #23553 «Pagos QR»: «Trabajo del 2026-10-13» (2 h)"
        ));
        assert!(evidence.text.contains("#26402"), "{}", evidence.text);
        assert!(!evidence.text.contains("2026-10-11") && !evidence.text.contains("2026-08-01"));
        let urls: Vec<&str> = evidence.references.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://dev.azure.com/example/Payments/_workitems/edit/23553",
                "https://dev.azure.com/example/Payments/_workitems/edit/26402",
                "https://dev.azure.com/example/Payments/_workitems/edit/26401"
            ]
        );
        // With the chat authorized too, its unregistered work shows; with nothing to show, no
        // section.
        let both = super::evidence(
            &entries,
            &[Origin::Activity, Origin::Teams],
            "2026-09-09",
            "2026-10-14",
        )
        .unwrap();
        assert!(both.text.contains("2026-10-11"));
        let pending = [entry("2026-10-14", "pending", chat, None)];
        assert!(super::evidence(&pending, &[Origin::Wiki], "2026-09-09", "2026-10-14").is_none());
    }

    #[test]
    fn a_request_names_its_day_or_period() {
        // A Thursday.
        let today = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        let day = |text: &str| requested_days(text, today).map(|(a, b)| format!("{a}..{b}"));
        for (text, expected) in [
            ("¿Qué hice ayer?", "2026-10-14..2026-10-14"),
            ("¿qué hice hoy?", "2026-10-15..2026-10-15"),
            ("¿Qué hiciste el martes?", "2026-10-13..2026-10-13"),
            ("¿y el jueves?", "2026-10-08..2026-10-08"),
            (
                "¿En qué trabajé el lunes y el miércoles?",
                "2026-10-12..2026-10-14",
            ),
            ("Mi actividad del 10 de octubre", "2026-10-10..2026-10-10"),
            ("¿Qué hice del 5 al 9 de octubre?", "2026-10-05..2026-10-09"),
            (
                "entre el 28 de septiembre y el 2 de octubre",
                "2026-09-28..2026-10-02",
            ),
            ("¿Qué hice el 10/10?", "2026-10-10..2026-10-10"),
            ("trabajo del 3/9/2026", "2026-09-03..2026-09-03"),
            ("actividad 2026-10-01", "2026-10-01..2026-10-01"),
            ("¿qué hice el 20 de diciembre?", "2025-12-20..2025-12-20"),
            ("¿Qué hice la semana pasada?", "2026-10-05..2026-10-11"),
            ("tareas de esta semana", "2026-10-12..2026-10-15"),
            ("¿qué hice el mes pasado?", "2026-09-01..2026-09-30"),
            ("¿qué hice este mes?", "2026-10-01..2026-10-15"),
            ("¿Qué hice en septiembre?", "2026-09-01..2026-09-30"),
            ("los últimos 3 días", "2026-10-13..2026-10-15"),
            ("las últimas 2 semanas", "2026-10-02..2026-10-15"),
            ("anteayer", "2026-10-13..2026-10-13"),
            ("What did I do yesterday?", "2026-10-14..2026-10-14"),
            (
                "What did I do the day before yesterday?",
                "2026-10-13..2026-10-13",
            ),
            ("What did I work on October 9?", "2026-10-09..2026-10-09"),
            ("my work from Oct 5 to 9", "2026-10-05..2026-10-09"),
            ("tasks on the 9th of October", "2026-10-09..2026-10-09"),
            ("What did you do on Monday?", "2026-10-12..2026-10-12"),
        ] {
            assert_eq!(day(text).as_deref(), Some(expected), "{text}");
        }
        for text in [
            "¿Qué hice?",
            "¿Cómo va la HU #23553?",
            "PR !4848 y build 10",
            "may I ask something?",
            "¿qué haré mañana?",
        ] {
            assert_eq!(day(text), None, "{text}");
        }
    }
}
