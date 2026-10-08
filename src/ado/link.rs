//! Registering unlinked work: a link from one of the user's work items to an artifact an
//! activity review found, or a new task under an existing work item (an HU) created with those
//! links. This personal-chat flow uses its own credential, and writes only after the user
//! confirms the exact plan. Local/scheduled registration is in `ado::registration`. Every target, parent and
//! artifact comes from data the app read and verified; the model only proposes titles and
//! reasons.
use super::{Catalog, item_reference, project_url, request};
use anyhow::{Context, Result, ensure};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Work a review found without a linked work item, ready to be linked.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Linkable {
    /// Closed key the model refers to (`a1`, `a2`…), assigned when the offer is built.
    #[serde(default)]
    pub key: String,
    /// Readable label, e.g. «PR #12 Fix the receipt (repo)».
    pub label: String,
    /// Its identifier as the user knows it, e.g. «PR #12», «run #345», «Release-6».
    #[serde(default)]
    pub short: String,
    /// Verified web address of the artifact.
    pub url: String,
    pub organization: String,
    /// Date of the activity (`YYYY-MM-DD`).
    pub date: String,
    /// Original timestamp, for registration in the user's configured time zone.
    #[serde(default)]
    pub occurred_at: String,
    /// Calls without verified attendance/purpose must be clarified before registration.
    #[serde(default)]
    pub context_required: bool,
    #[serde(default)]
    pub duration_hours: Option<f64>,
    /// `vstfs:///…` artifact and its link type (`Pull Request`, `Fixed in Commit`, `Build`);
    /// without one the link is a hyperlink to `url`.
    pub artifact: Option<String>,
    pub link_name: Option<String>,
    /// The user's work items code suggested for it, best first.
    #[serde(default)]
    pub suggested: Vec<u64>,
}
/// A work item a link may point from, or a new task's parent, as read from Azure DevOps.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Target {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub organization: String,
    pub project: String,
    /// Work item type, state, parent, area and iteration, to choose and create under it.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub parent: Option<u64>,
    #[serde(default)]
    pub area: String,
    #[serde(default)]
    pub iteration: String,
}
impl Target {
    /// Only an open HU (a user story or backlog item) may get a new task: not a task, a
    /// feature or an epic, and not one that is done, closed, aborted or removed.
    pub fn holds_tasks(&self) -> bool {
        let kind = self.kind.to_lowercase();
        let story = STORY_KINDS.iter().any(|k| kind == *k)
            || [
                "story",
                "historia",
                "backlog item",
                "requirement",
                "requisito",
            ]
            .iter()
            .any(|k| kind.contains(k));
        let closed = CLOSED_STATES
            .iter()
            .any(|s| self.state.eq_ignore_ascii_case(s));
        story && !closed
    }
    pub fn from_item(item: &Value, url: String, organization: &str) -> Option<Self> {
        Some(Self {
            id: item["id"].as_u64()?,
            title: super::field(item, "System.Title")
                .chars()
                .take(160)
                .collect(),
            url,
            organization: organization.to_owned(),
            project: super::field(item, "System.TeamProject").to_owned(),
            kind: super::field(item, "System.WorkItemType").to_owned(),
            state: super::field(item, "System.State").to_owned(),
            parent: super::relation_ids(item, "System.LinkTypes.Hierarchy-Reverse")
                .first()
                .copied(),
            area: super::field(item, "System.AreaPath").to_owned(),
            iteration: super::field(item, "System.IterationPath").to_owned(),
        })
    }
}
/// Work item types a new task is created as, by the user's own usage; `Task` by default.
pub const TASK_KINDS: &[&str] = &["Task", "Tarea"];
const STORY_KINDS: &[&str] = &["hu", "user story", "product backlog item", "pbi"];
const CLOSED_STATES: &[&str] = &[
    "Done",
    "Closed",
    "Removed",
    "Resolved",
    "Cut",
    "Rejected",
    "Completed",
    "Cerrado",
    "Cerrada",
    "Terminado",
    "Terminada",
    "Hecho",
    "Removido",
    "Removida",
    "Eliminado",
    "Abortado",
    "Abortada",
    "Cancelado",
    "Cancelada",
    "Rechazado",
    "Rechazada",
];
/// What a review leaves ready to link in the personal chat: the answer the user saw, the
/// unlinked activity and the user's work items.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LinkOffer {
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub activities: Vec<Linkable>,
    #[serde(default)]
    pub work_items: Vec<Target>,
    /// Type of the tasks the user registers work in, for new ones (`Task` when unknown).
    #[serde(default)]
    pub task_kind: String,
}
impl LinkOffer {
    /// Join offers from several sources, giving each activity its closed key in order.
    pub fn merge(offers: impl IntoIterator<Item = LinkOffer>) -> Self {
        let mut merged = LinkOffer::default();
        for offer in offers {
            if merged.task_kind.is_empty() {
                merged.task_kind = offer.task_kind;
            }
            merged.activities.extend(offer.activities);
            for item in offer.work_items {
                if !merged.work_items.iter().any(|w| w.id == item.id) {
                    merged.work_items.push(item);
                }
            }
        }
        merged.activities.truncate(40);
        for (index, activity) in merged.activities.iter_mut().enumerate() {
            activity.key = format!("a{}", index + 1);
        }
        merged
    }
}
/// One link the user asked for, in closed keys.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedLink {
    pub activity: String,
    pub work_item: u64,
}
/// Where the model proposes to register one activity: link it to an existing work item, or
/// create a task with `title` under `parent`. Code validates every key and ID.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub activity: String,
    pub action: ProposalAction,
    pub work_item: Option<u64>,
    pub parent: Option<u64>,
    pub title: Option<String>,
    pub reason: String,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalAction {
    Link,
    Create,
}
/// A task the plan creates: its title and type; the step's target is its parent.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NewTask {
    pub title: String,
    pub kind: String,
}
/// One resolved link: the source whose write credential adds it, the activity and the work
/// item read from Azure DevOps.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PlanStep {
    pub source: String,
    pub activity: Linkable,
    pub target: Target,
    /// With a new task, `target` is its parent and the link goes on the new task.
    #[serde(default)]
    pub create: Option<NewTask>,
    #[serde(default)]
    pub reason: String,
}
/// A plan the user must confirm: every activity and target fully resolved.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LinkPlan {
    pub links: Vec<PlanStep>,
}
/// The result of one write; recorded before the request as `sending` and never retried.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    Sending,
    Linked,
    AlreadyLinked,
    Failed,
    /// The write may or may not have happened (interrupted): check the work item by hand.
    Uncertain,
}

/// One link of a confirmed plan as recorded in the audit, before and after its request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LinkRecord {
    pub activity: String,
    pub activity_url: String,
    pub work_item: u64,
    pub title: String,
    pub work_item_url: String,
    pub status: LinkStatus,
    /// A task created for the link, under `work_item` (its parent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_title: Option<String>,
}

pub fn pull_request_artifact(project_id: &str, repo_id: &str, id: u64) -> String {
    format!("vstfs:///Git/PullRequestId/{project_id}%2f{repo_id}%2f{id}")
}
pub fn commit_artifact(project_id: &str, repo_id: &str, sha: &str) -> String {
    format!("vstfs:///Git/Commit/{project_id}%2f{repo_id}%2f{sha}")
}
pub fn build_artifact(id: u64) -> String {
    format!("vstfs:///Build/Build/{id}")
}

/// The relation that links an activity from a work item.
fn relation(activity: &Linkable) -> Value {
    match (&activity.artifact, &activity.link_name) {
        (Some(artifact), Some(name)) => {
            json!({"rel":"ArtifactLink","url":artifact,"attributes":{"name":name}})
        }
        _ => {
            json!({"rel":"Hyperlink","url":activity.url,"attributes":{"comment":"Linked from Personal Teams Assistant"}})
        }
    }
}
/// The JSON Patch that adds the link to a work item.
fn patch(activity: &Linkable) -> Value {
    json!([{"op":"add","path":"/relations/-","value":relation(activity)}])
}
/// The JSON Patch that creates a task under `parent`, assigned to the user, in the parent's
/// area and iteration, already linking `activities`.
fn new_task(
    organization: &str,
    parent: &Target,
    title: &str,
    assignee: &str,
    activities: &[&Linkable],
) -> Value {
    let parent_url = format!(
        "{}/_apis/wit/workItems/{}",
        organization.trim_end_matches('/'),
        parent.id
    );
    let mut operations = vec![
        json!({"op":"add","path":"/fields/System.Title","value":title}),
        json!({"op":"add","path":"/fields/System.AssignedTo","value":assignee}),
        json!({"op":"add","path":"/relations/-","value":{"rel":"System.LinkTypes.Hierarchy-Reverse","url":parent_url}}),
    ];
    if !parent.area.is_empty() {
        operations.push(json!({"op":"add","path":"/fields/System.AreaPath","value":parent.area}));
    }
    if !parent.iteration.is_empty() {
        operations.push(
            json!({"op":"add","path":"/fields/System.IterationPath","value":parent.iteration}),
        );
    }
    for activity in activities {
        operations.push(json!({"op":"add","path":"/relations/-","value":relation(activity)}));
    }
    Value::Array(operations)
}

/// Read one work item and accept it as a target only inside the catalog's scope.
pub async fn verify_work_item(
    client: &Client,
    key: &str,
    catalog_text: &str,
    id: u64,
) -> Result<Option<Target>> {
    let catalog = Catalog::parse(catalog_text)?;
    for source in &catalog.sources {
        let mut url = url::Url::parse(&source.organization)?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid ADO URL"))?
            .pop_if_empty()
            .extend(["_apis", "wit", "workitems", &id.to_string()]);
        url.query_pairs_mut()
            .append_pair("api-version", "7.1")
            .append_pair("$expand", "Relations");
        let Ok(item) = request(client, key, Method::GET, url, None).await else {
            continue;
        };
        if item["id"].as_u64() != Some(id) {
            continue;
        }
        let project = super::field(&item, "System.TeamProject").to_owned();
        let Ok(reference) = item_reference(source, &project, &item) else {
            continue;
        };
        return Ok(Target::from_item(
            &item,
            reference.url,
            &source.organization,
        ));
    }
    Ok(None)
}

/// Add one link. Called once per confirmed link; the caller records `sending` first and never
/// calls it again for the same link.
pub async fn add_link(
    client: &Client,
    key: &str,
    catalog_text: &str,
    target: &Target,
    activity: &Linkable,
) -> Result<LinkStatus> {
    let catalog = Catalog::parse(catalog_text)?;
    let source = catalog
        .sources
        .iter()
        .find(|s| s.organization == target.organization && s.organization == activity.organization)
        .context("link outside the catalog's organization")?;
    let url = project_url(
        source,
        &target.project,
        "dev.azure.com",
        &["_apis", "wit", "workitems", &target.id.to_string()],
    )?;
    // A transport error after sending may or may not have written the link.
    let Ok(response) = client
        .request(Method::PATCH, url)
        .basic_auth("", Some(key))
        .header("Content-Type", "application/json-patch+json")
        .body(patch(activity).to_string())
        .send()
        .await
    else {
        return Ok(LinkStatus::Uncertain);
    };
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(LinkStatus::Linked);
    }
    // Azure DevOps rejects a relation that already exists; only that class leaves here.
    let body = crate::adapters::bounded_bytes(response, 64_000)
        .await
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&body).to_lowercase();
    ensure!(
        status == 400 && text.contains("already exists"),
        "Azure DevOps link write returned HTTP {status}"
    );
    Ok(LinkStatus::AlreadyLinked)
}

/// Create one task under `parent` linking `activities`. Called once per confirmed task; the
/// caller records `sending` first and never calls it again. Returns the new task's ID.
pub async fn create_task(
    client: &Client,
    key: &str,
    catalog_text: &str,
    parent: &Target,
    task: &NewTask,
    activities: &[&Linkable],
) -> Result<(LinkStatus, Option<u64>)> {
    let catalog = Catalog::parse(catalog_text)?;
    let source = catalog
        .sources
        .iter()
        .find(|s| {
            s.organization == parent.organization
                && activities.iter().all(|a| a.organization == s.organization)
        })
        .context("task outside the catalog's organization")?;
    let kind = format!(
        "${}",
        if task.kind.is_empty() {
            "Task"
        } else {
            &task.kind
        }
    );
    let url = project_url(
        source,
        &parent.project,
        "dev.azure.com",
        &["_apis", "wit", "workitems", &kind],
    )?;
    let body = new_task(
        &source.organization,
        parent,
        &task.title,
        &source.author_email,
        activities,
    );
    // A transport error after sending may or may not have created the task.
    let Ok(response) = client
        .request(Method::POST, url)
        .basic_auth("", Some(key))
        .header("Content-Type", "application/json-patch+json")
        .body(body.to_string())
        .send()
        .await
    else {
        return Ok((LinkStatus::Uncertain, None));
    };
    let status = response.status().as_u16();
    ensure!(
        (200..300).contains(&status),
        "Azure DevOps task creation returned HTTP {status}"
    );
    // Created: an unreadable body only loses the new ID.
    let id = crate::adapters::bounded_bytes(response, 1_000_000)
        .await
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["id"].as_u64());
    Ok((LinkStatus::Linked, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(artifact: Option<&str>) -> Linkable {
        Linkable {
            label: "PR #12 Fix".into(),
            url: "https://dev.azure.com/example/Payments/_git/api/pullrequest/12".into(),
            organization: "https://dev.azure.com/example".into(),
            date: "2026-09-30".into(),
            artifact: artifact.map(str::to_owned),
            link_name: artifact.map(|_| "Pull Request".into()),
            ..Default::default()
        }
    }

    #[test]
    fn links_use_the_artifact_formats_azure_devops_stores() {
        assert_eq!(
            pull_request_artifact("p-id", "r-id", 12),
            "vstfs:///Git/PullRequestId/p-id%2fr-id%2f12"
        );
        assert_eq!(
            commit_artifact("p-id", "r-id", "abc"),
            "vstfs:///Git/Commit/p-id%2fr-id%2fabc"
        );
        assert_eq!(build_artifact(7), "vstfs:///Build/Build/7");
        let artifact = patch(&activity(Some("vstfs:///Git/PullRequestId/p%2fr%2f12")));
        assert_eq!(artifact[0]["op"], "add");
        assert_eq!(artifact[0]["path"], "/relations/-");
        assert_eq!(artifact[0]["value"]["rel"], "ArtifactLink");
        assert_eq!(artifact[0]["value"]["attributes"]["name"], "Pull Request");
        let hyperlink = patch(&activity(None));
        assert_eq!(hyperlink[0]["value"]["rel"], "Hyperlink");
        assert_eq!(
            hyperlink[0]["value"]["url"],
            "https://dev.azure.com/example/Payments/_git/api/pullrequest/12"
        );
    }

    #[test]
    fn a_new_task_goes_under_its_parent_assigned_to_the_user_with_its_links() {
        let parent = Target {
            id: 40,
            title: "HU".into(),
            url: "https://dev.azure.com/example/P/_workitems/edit/40".into(),
            organization: "https://dev.azure.com/example".into(),
            project: "P".into(),
            kind: "User Story".into(),
            area: "P\\Team".into(),
            iteration: "P\\Sprint 9".into(),
            ..Default::default()
        };
        assert!(parent.holds_tasks());
        // Not a task, a feature or an HU that is closed or aborted.
        for (kind, state) in [
            ("task", "Active"),
            ("Feature", "New"),
            ("Epic", "New"),
            ("Product Backlog Item", "Abortada"),
            ("User Story", "Done"),
            ("Product Backlog Item", "Removed"),
        ] {
            let item = Target {
                kind: kind.into(),
                state: state.into(),
                ..Default::default()
            };
            assert!(!item.holds_tasks(), "{kind} {state}");
        }
        assert!(
            Target {
                kind: "Product Backlog Item".into(),
                state: "Committed".into(),
                ..Default::default()
            }
            .holds_tasks()
        );
        let pr = activity(Some("vstfs:///Git/PullRequestId/p%2fr%2f12"));
        let wiki = activity(None);
        let body = new_task(
            "https://dev.azure.com/example/",
            &parent,
            "Fix the receipt",
            "me@example.com",
            &[&pr, &wiki],
        );
        let ops = body.as_array().unwrap();
        let value = |path: &str| {
            ops.iter()
                .filter(|o| o["path"] == path)
                .map(|o| o["value"].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            value("/fields/System.Title"),
            vec![json!("Fix the receipt")]
        );
        assert_eq!(
            value("/fields/System.AssignedTo"),
            vec![json!("me@example.com")]
        );
        assert_eq!(value("/fields/System.AreaPath"), vec![json!("P\\Team")]);
        assert_eq!(
            value("/fields/System.IterationPath"),
            vec![json!("P\\Sprint 9")]
        );
        let relations = value("/relations/-");
        assert_eq!(relations.len(), 3);
        assert_eq!(relations[0]["rel"], "System.LinkTypes.Hierarchy-Reverse");
        assert_eq!(
            relations[0]["url"],
            "https://dev.azure.com/example/_apis/wit/workItems/40"
        );
        assert_eq!(relations[1]["rel"], "ArtifactLink");
        assert_eq!(relations[2]["rel"], "Hyperlink");
        assert!(ops.iter().all(|o| o["op"] == "add"));
    }

    #[test]
    fn merged_offers_get_closed_keys_and_unique_work_items() {
        let target = Target {
            id: 5,
            title: "Task".into(),
            url: "https://dev.azure.com/example/P/_workitems/edit/5".into(),
            organization: "https://dev.azure.com/example".into(),
            project: "P".into(),
            ..Default::default()
        };
        let one = LinkOffer {
            activities: vec![activity(None)],
            work_items: vec![target.clone()],
            ..Default::default()
        };
        let merged = LinkOffer::merge([one.clone(), one]);
        let keys: Vec<_> = merged.activities.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["a1", "a2"]);
        assert_eq!(merged.work_items, vec![target]);
    }
}
