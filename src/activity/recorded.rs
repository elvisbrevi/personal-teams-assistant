//! The work the Activities section recorded, by day, as context for status answers and
//! activity reviews: tasks the app registered and the user's verified work it detected that
//! is not registered yet. An entry is shown only where every source its activities come from
//! is authorized, so a Teams conversation never sees work drawn from chats it may not read.
use super::Entry;
use crate::ado::link::{Linkable, Target};
use crate::evidence::{Block, Evidence, Reference};

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

/// One line for an entry and the references it names. Registered tasks and their HU are
/// named by number only when their link verifies.
fn line(entry: &Entry) -> Option<(String, Vec<Reference>)> {
    let registered = match entry.state.as_str() {
        "done" | "uncertain" | "already_registered" => true,
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
    let task = match (entry.task_id, entry.task_url.as_deref(), parent) {
        (Some(id), Some(url), Some(parent)) => {
            work_item(parent, id, format!("#{id} {}", entry.draft.title), url).map(|r| {
                references.push(r);
                format!(" #{id}")
            })
        }
        _ => None,
    };
    let what = match (registered, entry.state.as_str()) {
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

/// The recorded work between `since` and `until` (yyyy-mm-dd, inclusive), newest day first,
/// keeping only entries whose activities all come from `allowed` sources. `None` when
/// nothing can be shown.
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
            !e.activities.is_empty()
                && e.activities
                    .iter()
                    .all(|a| origin(a).is_some_and(|o| allowed.contains(&o)))
        })
        .collect();
    shown.sort_by(|a, b| b.day.cmp(&a.day).then(b.updated_at.cmp(&a.updated_at)));
    let header = format!(
        "Work recorded in the app's Activities section, by day, from {since} to {until} (the user's own verified activity; registered tasks were created in Azure DevOps by the app):"
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
            entry("2026-10-12", "dismissed", commit, None),
            entry("2026-08-01", "done", commit, Some(1)),
        ];
        let evidence = evidence(&entries, &[Origin::Activity], "2026-09-09", "2026-10-14").unwrap();
        // Newest day first; a chat-based entry, a dismissed one and an old one are left out.
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
        assert!(!evidence.text.contains("26402") && !evidence.text.contains("2026-08-01"));
        let urls: Vec<&str> = evidence.references.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://dev.azure.com/example/Payments/_workitems/edit/23553",
                "https://dev.azure.com/example/Payments/_workitems/edit/26401"
            ]
        );
        // With the chat authorized too, its entry shows; with nothing to show, no section.
        let both = super::evidence(
            &entries,
            &[Origin::Activity, Origin::Teams],
            "2026-09-09",
            "2026-10-14",
        )
        .unwrap();
        assert!(both.text.contains("#26402"));
        assert!(super::evidence(&entries, &[Origin::Wiki], "2026-09-09", "2026-10-14").is_none());
    }
}
