//! A work item a status request names («avance de la HU #23553»), read directly in the
//! catalog's scope whoever holds it now: its state and since when, the current owner, state
//! and assignment changes, planned dates, child tasks, linked code and recent comments. The
//! user's own recent activity alone misses an HU already handed over to someone else.
use super::{Source, field, item_reference, label, organization_url, project_url, request};
use crate::evidence::Reference;
use anyhow::{Context, Result};
use reqwest::{Client, Method};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Named work items read per request.
const REQUESTED: usize = 5;
/// Child tasks read per work item, and how many of them are listed one by one.
const CHILDREN: usize = 30;
const LISTED_CHILDREN: usize = 10;
const COMMENTS: usize = 5;
/// Custom date fields whose display name is looked up per status read.
const FIELD_NAMES: usize = 8;

/// Work item IDs the request names: «HU #23553», «bug 4512», «AB#77» or a bare «#23553», but
/// not a run, build, pull request or release number («run #345»).
pub(super) fn requested(question: &str) -> Vec<u64> {
    let mut ids: Vec<u64> = Vec::new();
    let mut push = |id: Option<u64>| {
        if let Some(id) = id
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    };
    if let Ok(keyword) = regex::Regex::new(
        r"(?i)\b(?:HU|US|PBI|bug|task|tarea|work item|historia|incidente|incident)\s*#?\s*(\d{3,9})\b",
    ) {
        for c in keyword.captures_iter(question) {
            push(c[1].parse().ok());
        }
    }
    if let Ok(hash) = regex::Regex::new(r"(?i)(?:\b(\p{L}+)\s*)?(?:AB)?#(\d{3,9})\b") {
        for c in hash.captures_iter(question) {
            let before = c.get(1).map(|m| m.as_str().to_lowercase());
            let other = before.as_deref().is_some_and(|w| {
                matches!(
                    w,
                    "run"
                        | "build"
                        | "pr"
                        | "release"
                        | "pipeline"
                        | "commit"
                        | "ejecución"
                        | "ejecucion"
                        | "stage"
                )
            });
            if !other {
                push(c[2].parse().ok());
            }
        }
    }
    ids.truncate(REQUESTED);
    ids
}

/// Plain text of a comment's HTML, on one line and bounded.
fn plain(html: &str, limit: usize) -> String {
    let text = regex::Regex::new(r"<[^>]*>")
        .map(|tags| tags.replace_all(html, " ").into_owned())
        .unwrap_or_else(|_| html.to_owned())
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        out.push('…');
    }
    out
}
fn day(date: &str) -> &str {
    date.get(..10).unwrap_or(date)
}
fn display_name(identity: &Value) -> String {
    identity["displayName"]
        .as_str()
        .or_else(|| identity.as_str())
        .unwrap_or("")
        .chars()
        .take(80)
        .collect()
}
/// A readable name for a field reference without a lookup: «TargetDate» → «Target Date».
fn humanized(reference: &str) -> String {
    let last = reference.rsplit('.').next().unwrap_or(reference);
    let mut out = String::new();
    let mut previous_lower = false;
    for c in last.chars() {
        if c.is_uppercase() && previous_lower {
            out.push(' ');
        }
        previous_lower = c.is_lowercase();
        out.push(if c == '_' { ' ' } else { c });
    }
    out
}
/// Dates a process records on the item (target, due, commitment, resolution…), as
/// `(reference, yyyy-mm-dd)`. System dates and the state change date are reported apart.
pub(super) fn planned_dates(item: &Value) -> Vec<(String, String)> {
    let Some(fields) = item["fields"].as_object() else {
        return Vec::new();
    };
    fields
        .iter()
        .filter(|(name, _)| {
            !name.starts_with("System.") && name.as_str() != "Microsoft.VSTS.Common.StateChangeDate"
        })
        .filter_map(|(name, value)| {
            let date = value.as_str()?;
            chrono::DateTime::parse_from_rfc3339(date).ok()?;
            Some((name.clone(), day(date).to_owned()))
        })
        .take(8)
        .collect()
}
/// Display names of custom fields (`Custom.<GUID>` says nothing to the model); Microsoft
/// fields read well humanized. A failed lookup keeps the humanized reference.
pub(super) async fn field_names(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    references: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    for reference in references {
        let custom = !reference.starts_with("Microsoft.");
        let mut name = humanized(reference);
        if custom
            && names.len() < FIELD_NAMES
            && let Ok(url) = project_url(
                source,
                project,
                "dev.azure.com",
                &["_apis", "wit", "fields", reference],
            )
            && let Ok(found) = request(client, key, Method::GET, url, None).await
            && let Some(found) = found["name"].as_str().filter(|n| !n.trim().is_empty())
        {
            name = found.chars().take(80).collect();
        }
        names.insert(reference.clone(), name);
    }
    names
}
/// Commits and pull requests linked to `items` on or after `since` (yyyy-mm-dd), and the
/// latest date one was linked.
fn linked_code(items: &[&Value], since: &str) -> (usize, usize, Option<String>) {
    let (mut commits, mut pulls, mut latest): (usize, usize, Option<String>) = (0, 0, None);
    for relation in items
        .iter()
        .flat_map(|item| item["relations"].as_array().into_iter().flatten())
        .filter(|r| r["rel"].as_str() == Some("ArtifactLink"))
    {
        let url = relation["url"].as_str().unwrap_or("");
        let date = day(relation["attributes"]["authorizedDate"]
            .as_str()
            .unwrap_or(""));
        if date < since {
            continue;
        }
        if url.starts_with("vstfs:///Git/Commit/") {
            commits += 1;
        } else if url.starts_with("vstfs:///Git/PullRequestId/") {
            pulls += 1;
        } else {
            continue;
        }
        if latest.as_deref().is_none_or(|l| date > l) {
            latest = Some(date.to_owned());
        }
    }
    (commits, pulls, latest)
}

async fn items(
    client: &Client,
    key: &str,
    source: &Source,
    ids: &BTreeSet<u64>,
) -> Result<Vec<Value>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut url = organization_url(source, &["_apis", "wit", "workitems"])?;
    url.query_pairs_mut()
        .append_pair(
            "ids",
            &ids.iter().map(u64::to_string).collect::<Vec<_>>().join(","),
        )
        .append_pair("$expand", "all")
        .append_pair("errorPolicy", "Omit");
    let value = request(client, key, Method::GET, url, None).await?;
    Ok(value["value"]
        .as_array()
        .context("invalid ADO work items")?
        .iter()
        .filter(|item| item["id"].as_u64().is_some())
        .filter(|item| {
            let project = field(item, "System.TeamProject");
            !project.is_empty()
                && (source.projects == ["*"]
                    || source
                        .projects
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(project)))
        })
        .cloned()
        .collect())
}

/// What a work item's update history says, for the window and about the user.
#[derive(Debug, Default)]
struct History {
    /// State changes on or after the window's start, oldest first.
    states: Vec<String>,
    /// Assignment changes on or after the window's start, oldest first.
    assignments: Vec<String>,
    /// The user was assigned to it at some point.
    assigned: bool,
    /// The user changed it at some point.
    changed: bool,
}
/// Reads the item's update history; facts older than `since` (yyyy-mm-dd) are left out, but
/// the whole history tells whether the user ever held or changed it.
fn changes(updates: &Value, email: &str, since: &str) -> History {
    let mut history = History::default();
    for update in updates["value"].as_array().into_iter().flatten() {
        let fields = &update["fields"];
        let date = day(fields["System.ChangedDate"]["newValue"]
            .as_str()
            .or_else(|| update["revisedDate"].as_str())
            .unwrap_or(""));
        history.changed |= super::same_user(&update["revisedBy"], email);
        let by = display_name(&update["revisedBy"]);
        let recent = date >= since;
        let state = &fields["System.State"];
        if let Some(new) = state["newValue"].as_str()
            && recent
        {
            history.states.push(match state["oldValue"].as_str() {
                Some(old) => format!("{date} {old} → {new} (by {by})"),
                None => format!("{date} created as {new} (by {by})"),
            });
        }
        let assigned = &fields["System.AssignedTo"];
        if assigned.get("newValue").is_some() || assigned.get("oldValue").is_some() {
            for side in ["oldValue", "newValue"] {
                history.assigned |= super::same_user(&assigned[side], email);
            }
            if recent {
                let old = display_name(&assigned["oldValue"]);
                let new = display_name(&assigned["newValue"]);
                history.assignments.push(format!(
                    "{date} {} → {} (by {by})",
                    if old.is_empty() { "unassigned" } else { &old },
                    if new.is_empty() { "unassigned" } else { &new },
                ));
            }
        }
    }
    history
}

/// One line per section of a requested work item, and the references it names. Only a work
/// item the user took part in is described (assigned now or before, changed or created by
/// them, holding a child item of theirs or a comment of theirs), and only with facts from the
/// last five weeks besides its current state; any other is named as not the user's work.
async fn describe(
    client: &Client,
    key: &str,
    source: &Source,
    item: &Value,
) -> Result<(String, Vec<Reference>)> {
    let id = item["id"].as_u64().context("missing work item ID")?;
    let project = field(item, "System.TeamProject").to_owned();
    let id_text = id.to_string();
    let since = (chrono::Utc::now() - chrono::Duration::days(super::MAX_WINDOW_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let email = source.author_email.as_str();
    let updates_url = project_url(
        source,
        &project,
        "dev.azure.com",
        &["_apis", "wit", "workItems", &id_text, "updates"],
    )
    .map(|mut url| {
        url.query_pairs_mut().append_pair("$top", "200");
        url
    });
    let comments_url = project_url(
        source,
        &project,
        "dev.azure.com",
        &["_apis", "wit", "workItems", &id_text, "comments"],
    )
    .map(|mut url| {
        url.set_query(None);
        url.query_pairs_mut()
            .append_pair("$top", &COMMENTS.to_string())
            .append_pair("order", "desc")
            .append_pair("api-version", "7.1-preview.4");
        url
    });
    let children: BTreeSet<u64> = super::relation_ids(item, "System.LinkTypes.Hierarchy-Forward")
        .into_iter()
        .take(CHILDREN)
        .collect();
    let parent = super::relation_ids(item, "System.LinkTypes.Hierarchy-Reverse")
        .first()
        .copied();
    let related: BTreeSet<u64> = children.iter().copied().chain(parent).collect();
    let (updates, comments, related) = tokio::join!(
        async { request(client, key, Method::GET, updates_url?, None).await },
        async { request(client, key, Method::GET, comments_url?, None).await },
        items(client, key, source, &related)
    );
    let related = related.unwrap_or_default();
    let mut tasks: Vec<&Value> = related
        .iter()
        .filter(|i| i["id"].as_u64().is_some_and(|id| children.contains(&id)))
        .collect();
    tasks.sort_by(|a, b| field(b, "System.ChangedDate").cmp(field(a, "System.ChangedDate")));
    let history = updates
        .as_ref()
        .ok()
        .map(|updates| changes(updates, email, &since));
    let comments: Option<Vec<&Value>> = comments.as_ref().ok().map(|c| {
        c["comments"]
            .as_array()
            .into_iter()
            .flatten()
            .take(COMMENTS)
            .collect()
    });
    let holds = |identity: &Value| super::same_user(identity, email);
    let assigned_now = holds(&item["fields"]["System.AssignedTo"]);
    let took_part = assigned_now
        || holds(&item["fields"]["System.CreatedBy"])
        || holds(&item["fields"]["System.ChangedBy"])
        || history.as_ref().is_some_and(|h| h.assigned || h.changed)
        || tasks.iter().any(|t| {
            ["System.AssignedTo", "System.CreatedBy", "System.ChangedBy"]
                .iter()
                .any(|f| holds(&t["fields"][*f]))
        })
        || comments.iter().flatten().any(|c| holds(&c["createdBy"]));
    let mut references = vec![item_reference(source, &project, item)?];
    let mut text = format!(
        "Requested work item (named in the request): {} {}.\n",
        field(item, "System.WorkItemType"),
        label(item)
    );
    let state_since = field(item, "Microsoft.VSTS.Common.StateChangeDate");
    let owner = display_name(&item["fields"]["System.AssignedTo"]);
    let owner = if owner.is_empty() { "nobody" } else { &owner };
    if !took_part {
        text.push_str(&format!(
            "- Assigned to now: {owner}. The user has not taken part in it (never assigned, no change, child item or comment of theirs{}): it is not the user's work, so its details were not read; say so and who holds it.\n",
            if history.is_none() {
                "; its history could not be read"
            } else {
                ""
            }
        ));
        return Ok((text, references));
    }
    text.push_str(&format!(
        "- Current state: {}{}. Assigned to now: {owner}. Last change {} by {}.\n",
        field(item, "System.State"),
        if state_since.is_empty() {
            String::new()
        } else {
            format!(" since {}", day(state_since))
        },
        day(field(item, "System.ChangedDate")),
        display_name(&item["fields"]["System.ChangedBy"]),
    ));
    text.push_str(&format!(
        "- The user took part in it{}. Facts below are from {since} on (five weeks).\n",
        if assigned_now || history.as_ref().is_some_and(|h| h.assigned) {
            " and is or was assigned to it"
        } else {
            ""
        }
    ));
    match &history {
        Some(history) => {
            if !history.states.is_empty() {
                let recent = &history.states[history.states.len().saturating_sub(4)..];
                text.push_str(&format!("- State changes: {}.\n", recent.join("; ")));
            }
            if !history.assignments.is_empty() {
                let recent = &history.assignments[history.assignments.len().saturating_sub(3)..];
                text.push_str(&format!("- Assignment changes: {}.\n", recent.join("; ")));
            }
        }
        None => text.push_str("- Its history could not be read.\n"),
    }
    let dates = planned_dates(item);
    if !dates.is_empty() {
        let references: BTreeSet<String> = dates.iter().map(|(r, _)| r.clone()).collect();
        let names = field_names(client, key, source, &project, &references).await;
        text.push_str(&format!(
            "- Dates on the item: {}.\n",
            dates
                .iter()
                .map(|(r, d)| format!("{} {d}", names.get(r).map_or(r.as_str(), |n| n.as_str())))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if let Some(parent) = parent.and_then(|p| related.iter().find(|i| i["id"].as_u64() == Some(p)))
        && let Ok(reference) = item_reference(source, &project, parent)
    {
        references.push(reference);
        text.push_str(&format!(
            "- Parent: {} {}.\n",
            field(parent, "System.WorkItemType"),
            label(parent)
        ));
    }
    if !tasks.is_empty() {
        let mut states: BTreeMap<&str, usize> = BTreeMap::new();
        for task in &tasks {
            *states.entry(field(task, "System.State")).or_default() += 1;
        }
        text.push_str(&format!(
            "- Child items: {} ({}).\n",
            tasks.len(),
            states
                .iter()
                .map(|(state, n)| format!("{n} {state}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        for task in tasks
            .iter()
            .filter(|t| day(field(t, "System.ChangedDate")) >= since.as_str())
            .take(LISTED_CHILDREN)
        {
            if let Ok(reference) = item_reference(source, &project, task) {
                references.push(reference);
            }
            let owner = display_name(&task["fields"]["System.AssignedTo"]);
            text.push_str(&format!(
                "  - {} {} ({}changed {}).\n",
                field(task, "System.WorkItemType"),
                label(task),
                if owner.is_empty() {
                    String::new()
                } else {
                    format!("{owner}, ")
                },
                day(field(task, "System.ChangedDate"))
            ));
        }
    }
    let mut with_children = vec![item];
    with_children.extend(tasks.iter().copied());
    let (commits, pulls, latest) = linked_code(&with_children, &since);
    if commits + pulls > 0 {
        text.push_str(&format!(
            "- Code linked to the item or its children in the last five weeks: {commits} commit(s), {pulls} pull request(s){}.\n",
            latest.map_or_else(String::new, |d| format!("; latest linked {d}"))
        ));
    } else {
        text.push_str(
            "- No commit or pull request was linked to the item or its children in the last five weeks.\n",
        );
    }
    match &comments {
        Some(comments) => {
            let lines: Vec<String> = comments
                .iter()
                .filter(|c| day(c["createdDate"].as_str().unwrap_or("")) >= since.as_str())
                .map(|c| {
                    format!(
                        "{} {}: «{}»",
                        day(c["createdDate"].as_str().unwrap_or("")),
                        display_name(&c["createdBy"]),
                        plain(c["text"].as_str().unwrap_or(""), 300)
                    )
                })
                .collect();
            if lines.is_empty() {
                text.push_str("- No comments in the last five weeks.\n");
            } else {
                text.push_str(&format!(
                    "- Recent comments (newest first): {}.\n",
                    lines.join("; ")
                ));
            }
        }
        None => text.push_str("- Its comments could not be read.\n"),
    }
    Ok((text, references))
}

/// The requested work items found in `source`'s scope, in the order the request names them,
/// each with its description and the references it names.
pub(super) async fn read(
    client: &Client,
    key: &str,
    source: &Source,
    ids: &[u64],
) -> Result<Vec<(u64, String, Vec<Reference>)>> {
    let wanted: BTreeSet<u64> = ids.iter().copied().collect();
    let found = items(client, key, source, &wanted).await?;
    let mut described = Vec::new();
    for id in ids {
        if let Some(item) = found.iter().find(|i| i["id"].as_u64() == Some(*id)) {
            let (text, references) = describe(client, key, source, item).await?;
            described.push((*id, text, references));
        }
    }
    Ok(described)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requested_items_are_work_items_not_runs_or_releases() {
        assert_eq!(
            requested("1) Avance de lo que tienes en curso: HU #23553"),
            [23553]
        );
        assert_eq!(requested("¿cómo va la HU 4512 y el bug #778?"), [4512, 778]);
        assert_eq!(requested("estado de #23553 y AB#9001"), [23553, 9001]);
        assert!(requested("¿falló el run #345 o el build #346?").is_empty());
        assert!(requested("Release #12 y PR #88").is_empty());
        assert!(requested("1) avance 2) compromiso").is_empty());
    }

    #[test]
    fn history_names_state_and_owner_changes_and_whether_the_user_held_it() {
        let updates = json!({"value":[
            {"revisedBy":{"displayName":"Elvis Brevi","uniqueName":"elvis@example.com"},"fields":{
                "System.ChangedDate":{"newValue":"2026-09-20T12:00:00Z"},
                "System.State":{"newValue":"New"},
                "System.AssignedTo":{"newValue":{"displayName":"Elvis Brevi","uniqueName":"elvis@example.com"}}}},
            {"revisedBy":{"displayName":"Elvis Brevi"},"fields":{
                "System.ChangedDate":{"newValue":"2026-10-10T15:00:00Z"},
                "System.State":{"oldValue":"En desarrollo","newValue":"Enviado a validación"},
                "System.AssignedTo":{"oldValue":{"displayName":"Elvis Brevi","uniqueName":"elvis@example.com"},
                    "newValue":{"displayName":"Luis Felipe","uniqueName":"luis@example.com"}}}}
        ]});
        let history = changes(&updates, "Elvis@example.com", "2026-09-10");
        assert_eq!(
            history.states,
            [
                "2026-09-20 created as New (by Elvis Brevi)",
                "2026-10-10 En desarrollo → Enviado a validación (by Elvis Brevi)"
            ]
        );
        assert_eq!(
            history.assignments[1],
            "2026-10-10 Elvis Brevi → Luis Felipe (by Elvis Brevi)"
        );
        assert!(history.assigned);
        // Older facts stay out of the window; the whole history still says the user held it.
        let recent = changes(&updates, "Elvis@example.com", "2026-10-01");
        assert_eq!(
            recent.states,
            ["2026-10-10 En desarrollo → Enviado a validación (by Elvis Brevi)"]
        );
        assert_eq!(recent.assignments.len(), 1);
        assert!(recent.assigned);
        let other = changes(&updates, "other@example.com", "2026-09-10");
        assert!(!other.assigned && !other.changed);
    }

    #[test]
    fn planned_dates_skip_system_dates_and_read_comment_text() {
        let item = json!({"fields":{
            "System.ChangedDate":"2026-10-13T10:00:00Z",
            "Microsoft.VSTS.Common.StateChangeDate":"2026-10-10T10:00:00Z",
            "Microsoft.VSTS.Scheduling.TargetDate":"2026-10-20T03:00:00Z",
            "Custom.FechaCompromiso":"2026-10-24T03:00:00Z",
            "Custom.Note":"not a date"}});
        assert_eq!(
            planned_dates(&item),
            [
                ("Custom.FechaCompromiso".to_owned(), "2026-10-24".to_owned()),
                (
                    "Microsoft.VSTS.Scheduling.TargetDate".to_owned(),
                    "2026-10-20".to_owned()
                )
            ]
        );
        assert_eq!(
            humanized("Microsoft.VSTS.Scheduling.TargetDate"),
            "Target Date"
        );
        assert_eq!(
            plain(
                "<div>Faltan las <b>variables</b> en Consul&nbsp;prod</div>",
                100
            ),
            "Faltan las variables en Consul prod"
        );
    }

    #[tokio::test]
    async fn a_named_hu_is_described_only_if_the_user_took_part_and_within_five_weeks() {
        use wiremock::{
            Mock, MockServer, Request, ResponseTemplate,
            matchers::{method, path},
        };
        // Dates relative to today: the window is the last five weeks.
        let at = |days: i64| {
            (chrono::Utc::now() - chrono::Duration::days(days))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        };
        let d = |days: i64| at(days)[..10].to_owned();
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(url::Url::parse(&server.uri()).unwrap()));
        let source = Source {
            organization: "https://dev.azure.com/example".into(),
            projects: vec!["Payments".into()],
            author_email: "elvis@example.com".into(),
        };
        let elvis = json!({"displayName":"Elvis Brevi","uniqueName":"elvis@example.com"});
        let luis = json!({"displayName":"Luis Felipe","uniqueName":"luis@example.com"});
        let hu = json!({"id":23553,"fields":{
            "System.TeamProject":"Payments","System.WorkItemType":"User Story",
            "System.Title":"Pagos QR","System.State":"Enviado a validación",
            "System.AssignedTo":luis,"System.ChangedBy":elvis,"System.CreatedBy":luis,
            "System.ChangedDate":at(4),
            "Microsoft.VSTS.Common.StateChangeDate":at(4),
            "Custom.FechaCompromiso":at(-10)},
            "relations":[
                {"rel":"System.LinkTypes.Hierarchy-Forward","url":"https://dev.azure.com/example/_apis/wit/workItems/23601"},
                {"rel":"System.LinkTypes.Hierarchy-Forward","url":"https://dev.azure.com/example/_apis/wit/workItems/23602"},
                {"rel":"System.LinkTypes.Hierarchy-Reverse","url":"https://dev.azure.com/example/_apis/wit/workItems/23000"},
                {"rel":"ArtifactLink","url":"vstfs:///Git/PullRequestId/p%2Fr%2F12","attributes":{"name":"Pull Request","authorizedDate":at(6)}},
                {"rel":"ArtifactLink","url":"vstfs:///Git/PullRequestId/p%2Fr%2F3","attributes":{"name":"Pull Request","authorizedDate":at(60)}}]});
        let task = json!({"id":23601,"fields":{
            "System.TeamProject":"Payments","System.WorkItemType":"Task","System.Title":"Implementar QR",
            "System.State":"Done","System.AssignedTo":elvis,"System.ChangedDate":at(5)},
            "relations":[{"rel":"ArtifactLink","url":"vstfs:///Git/Commit/p%2Fr%2Fabc","attributes":{"name":"Fixed in Commit","authorizedDate":at(5)}}]});
        let old_task = json!({"id":23602,"fields":{
            "System.TeamProject":"Payments","System.WorkItemType":"Task","System.Title":"Diseñar QR",
            "System.State":"Done","System.AssignedTo":elvis,"System.ChangedDate":at(50)}});
        let epic = json!({"id":23000,"fields":{"System.TeamProject":"Payments","System.WorkItemType":"Feature",
            "System.Title":"Pagos","System.State":"Active","System.ChangedDate":at(13)}});
        // Someone else's HU: never assigned to the user, changed or commented by them.
        let foreign = json!({"id":24000,"fields":{"System.TeamProject":"Payments","System.WorkItemType":"User Story",
            "System.Title":"Reporte de cobros","System.State":"Active","System.AssignedTo":luis,
            "System.CreatedBy":luis,"System.ChangedBy":luis,"System.ChangedDate":at(2)}});
        let outside = json!({"id":999,"fields":{"System.TeamProject":"Other","System.Title":"x","System.State":"New"}});
        Mock::given(method("GET"))
            .and(path("/example/_apis/wit/workitems"))
            .respond_with(move |request: &Request| {
                let ids = request
                    .url
                    .query_pairs()
                    .find(|(k, _)| k == "ids")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default();
                let value: Vec<Value> = [&hu, &task, &old_task, &epic, &foreign, &outside]
                    .into_iter()
                    .filter(|item| {
                        ids.split(',')
                            .any(|id| id.parse::<u64>().ok() == item["id"].as_u64())
                    })
                    .cloned()
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({ "value": value }))
            })
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/example/Payments/_apis/wit/workItems/23553/updates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"revisedBy":luis,"fields":{
                    "System.ChangedDate":{"newValue":at(60)},
                    "System.State":{"oldValue":"New","newValue":"En desarrollo"},
                    "System.AssignedTo":{"newValue":elvis}}},
                {"revisedBy":elvis,"fields":{
                    "System.ChangedDate":{"newValue":at(4)},
                    "System.State":{"oldValue":"En desarrollo","newValue":"Enviado a validación"},
                    "System.AssignedTo":{"oldValue":elvis,"newValue":luis}}}]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/example/Payments/_apis/wit/workItems/23553/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"comments":[
                {"text":"<div>Faltan las variables de Consul en producción; las pedí a Francisco.</div>",
                 "createdBy":elvis,"createdDate":at(3)},
                {"text":"Comentario antiguo","createdBy":luis,"createdDate":at(50)}]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/example/Payments/_apis/wit/workItems/24000/updates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"revisedBy":luis,"fields":{"System.ChangedDate":{"newValue":at(2)},
                    "System.State":{"newValue":"Active"},"System.AssignedTo":{"newValue":luis}}}]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/example/Payments/_apis/wit/workItems/24000/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"comments":[]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/example/Payments/_apis/wit/fields/Custom.FechaCompromiso",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"name":"Fecha de compromiso"})),
            )
            .mount(&server)
            .await;
        let described = read(
            &reqwest::Client::new(),
            "key",
            &source,
            &[23553, 999, 24000],
        )
        .await
        .unwrap();
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
        // The item outside the catalog's projects is not described.
        assert_eq!(described.len(), 2);
        let (id, text, references) = &described[0];
        assert_eq!(*id, 23553);
        for expected in [
            "Requested work item (named in the request): User Story #23553 Pagos QR — Enviado a validación.".to_owned(),
            format!("Current state: Enviado a validación since {}. Assigned to now: Luis Felipe. Last change {} by Elvis Brevi.", d(4), d(4)),
            "The user took part in it and is or was assigned to it.".to_owned(),
            format!("{} En desarrollo → Enviado a validación (by Elvis Brevi)", d(4)),
            format!("{} Elvis Brevi → Luis Felipe (by Elvis Brevi)", d(4)),
            format!("Dates on the item: Fecha de compromiso {}.", d(-10)),
            "Parent: Feature #23000 Pagos — Active.".to_owned(),
            "Child items: 2 (2 Done).".to_owned(),
            format!("Task #23601 Implementar QR — Done (Elvis Brevi, changed {}).", d(5)),
            format!("1 commit(s), 1 pull request(s); latest linked {}.", d(5)),
            format!("{} Elvis Brevi: «Faltan las variables de Consul en producción; las pedí a Francisco.»", d(3)),
        ] {
            assert!(text.contains(&expected), "{expected}\n{text}");
        }
        // Nothing older than five weeks: the first state change, the old task, PR and comment.
        for older in [
            format!("{} New → En desarrollo", d(60)),
            "Diseñar QR".to_owned(),
            "Comentario antiguo".to_owned(),
        ] {
            assert!(!text.contains(&older), "{older}\n{text}");
        }
        let urls: Vec<&str> = references.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://dev.azure.com/example/Payments/_workitems/edit/23553",
                "https://dev.azure.com/example/Payments/_workitems/edit/23000",
                "https://dev.azure.com/example/Payments/_workitems/edit/23601",
            ]
        );
        // Someone else's HU is named with who holds it, without its details.
        let (id, text, references) = &described[1];
        assert_eq!(*id, 24000);
        assert!(
            text.contains("Assigned to now: Luis Felipe. The user has not taken part in it"),
            "{text}"
        );
        assert!(!text.contains("Current state") && !text.contains("Child items"));
        assert_eq!(references.len(), 1);
    }
}
