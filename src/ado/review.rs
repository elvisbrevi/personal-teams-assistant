//! Activity review: the user's own Azure DevOps work in a window, split into work that a work
//! item records and work that none does. Read-only and bounded; reads are separate from the
//! classification so the rules can be tested without the network.
use super::{
    COLD_REPO_SECONDS, Catalog, Source, artifact, bounded_lines, cached_read, field,
    item_reference, label, project_url, projects, request, same_user, wiql, work_items,
};
use crate::{
    evidence::{Evidence, Reference},
    state::Store,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use reqwest::{Client, Method};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// The user's commits per repository change often while they review their own work.
const COMMITS_SECONDS: i64 = 10 * 60;
const MAX_PULL_REQUESTS: usize = 15;
const MAX_RUNS: usize = 10;
const MAX_RELEASES: usize = 10;
const MAX_LINES: usize = 25;
const MAX_REGISTERED: usize = 12;

#[derive(Clone, Debug, Default)]
pub(super) struct Commit {
    pub repo_id: String,
    pub repo: String,
    pub sha: String,
    pub date: String,
    pub message: String,
}
#[derive(Clone, Debug, Default)]
pub(super) struct PullRequest {
    pub repo_id: String,
    pub repo: String,
    pub id: u64,
    pub title: String,
    pub status: String,
    pub date: String,
    pub branch: String,
    pub commits: Vec<String>,
    pub work_items: Vec<u64>,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Run {
    pub id: u64,
    pub definition: String,
    pub result: String,
    pub date: String,
    pub sha: String,
    pub work_items: Vec<u64>,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Release {
    pub id: u64,
    pub name: String,
    pub definition: String,
    pub status: String,
    pub date: String,
    pub builds: Vec<u64>,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Approval {
    pub release_id: u64,
    pub release: String,
    pub stage: String,
    pub date: String,
}
/// Everything read for one project. `failed` names the reads that did not complete.
#[derive(Debug, Default)]
pub(super) struct ProjectActivity {
    pub project: String,
    pub items: Vec<Value>,
    /// Other work items the activity links or mentions, as read from Azure DevOps: only
    /// these are named by number.
    pub other_items: Vec<Value>,
    pub commits: Vec<Commit>,
    pub pull_requests: Vec<PullRequest>,
    pub runs: Vec<Run>,
    pub releases: Vec<Release>,
    pub approvals: Vec<Approval>,
    pub failed: Vec<&'static str>,
}

/// What a work item link points at (`vstfs:///…` artifact links on the work item).
#[derive(Debug, PartialEq, Eq)]
enum Linked {
    Commit(String),
    PullRequest(String, u64),
    Build(u64),
}
fn linked(url: &str) -> Option<Linked> {
    let decoded = url
        .strip_prefix("vstfs:///")?
        .replace("%2F", "/")
        .replace("%2f", "/");
    match decoded.split('/').collect::<Vec<_>>().as_slice() {
        ["Git", "Commit", _, _, sha] => Some(Linked::Commit(sha.to_ascii_lowercase())),
        ["Git", "PullRequestId", _, repo, id] => Some(Linked::PullRequest(
            repo.to_ascii_lowercase(),
            id.parse().ok()?,
        )),
        ["Build", "Build", id] => Some(Linked::Build(id.parse().ok()?)),
        _ => None,
    }
}
/// Work item IDs a text names: `#123`, `AB#123` (Azure Boards' linking convention) or a
/// type and number such as «HU 8765» or «Bug 4321».
pub(crate) fn mentions(text: &str) -> Vec<u64> {
    let mut ids: Vec<u64> = Vec::new();
    for pattern in [
        r"(?:^|[^\w])(?:AB)?#(\d{1,9})\b",
        r"(?i)\b(?:HU|US|PBI|bug|task|tarea|work item)\s*#?(\d{3,9})\b",
    ] {
        if let Ok(re) = regex::Regex::new(pattern) {
            for c in re.captures_iter(text) {
                if let Ok(id) = c[1].parse::<u64>()
                    && !ids.contains(&id)
                {
                    ids.push(id);
                }
            }
        }
    }
    ids
}
/// The pull request a merge commit closes: Azure Repos' «Merged PR 12: …» or a
/// «Merge pull request 12 from …» message.
fn merged_pull_request(message: &str) -> Option<u64> {
    let rest = message
        .strip_prefix("Merged PR ")
        .or_else(|| message.strip_prefix("Merge pull request "))?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}
fn day(date: &str) -> &str {
    date.get(..10).unwrap_or(date)
}
fn short(text: &str, limit: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(limit).collect();
    if line.chars().count() > limit {
        out.push('…');
    }
    out
}
fn after(date: &str, since: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(date).is_ok_and(|d| d.with_timezone(&Utc) >= since)
}
fn ids(value: &Value) -> Vec<u64> {
    value["value"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| {
            w["id"]
                .as_u64()
                .or_else(|| w["id"].as_str().and_then(|s| s.parse().ok()))
        })
        .collect()
}

/// One activity line, sorted by date, with the references it names.
struct Line {
    date: String,
    text: String,
}

/// Classify one project's activity. Pure: no network, so the rules are testable.
pub(super) fn summarize(
    source: &Source,
    activity: &ProjectActivity,
) -> Result<(String, Vec<Reference>, usize)> {
    let project = activity.project.as_str();
    let mut references = Vec::new();
    // What the user's work items already link.
    let mut items: BTreeMap<u64, &Value> = BTreeMap::new();
    let mut commits_linked: BTreeMap<String, u64> = BTreeMap::new();
    let mut pulls_linked: BTreeMap<(String, u64), u64> = BTreeMap::new();
    let mut builds_linked: BTreeMap<u64, u64> = BTreeMap::new();
    for item in &activity.items {
        let Some(id) = item["id"].as_u64() else {
            continue;
        };
        items.insert(id, item);
        for relation in item["relations"].as_array().into_iter().flatten() {
            if relation["rel"].as_str() != Some("ArtifactLink") {
                continue;
            }
            match relation["url"].as_str().and_then(linked) {
                Some(Linked::Commit(sha)) => {
                    commits_linked.insert(sha, id);
                }
                Some(Linked::PullRequest(repo, pr)) => {
                    pulls_linked.insert((repo, pr), id);
                }
                Some(Linked::Build(build)) => {
                    builds_linked.insert(build, id);
                }
                None => {}
            }
        }
    }
    // Activity attached to each of the user's work items, for the registered summary, and
    // activity linked only to other work items.
    let mut attached: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    let mut elsewhere: Vec<(String, Vec<u64>)> = Vec::new();
    let mut attach = |ids: &[u64], what: String| {
        let (own, other): (Vec<u64>, Vec<u64>) = ids.iter().partition(|id| items.contains_key(id));
        for id in &own {
            attached.entry(*id).or_default().push(what.clone());
        }
        if own.is_empty() && !other.is_empty() {
            elsewhere.push((what, other));
        }
    };
    let mut unregistered: Vec<Line> = Vec::new();
    let mut registered_commits = BTreeSet::new();
    // Commits of the user's pull requests are reported with their pull request.
    let mut in_pull_request = BTreeSet::new();
    let branch_numbers = regex::Regex::new(r"\d{2,9}")?;
    for pr in &activity.pull_requests {
        let reference = artifact(
            source,
            project,
            "pull_request",
            &format!("{}:{}", pr.repo_id, pr.id),
            format!("PR #{} {} ({})", pr.id, short(&pr.title, 100), pr.repo),
            vec![
                format!("PR #{}", pr.id),
                format!("PR {}", pr.id),
                format!("pull request {}", pr.id),
                format!("!{}", pr.id),
            ],
            &["_git", &pr.repo, "pullrequest", &pr.id.to_string()],
            &[],
            None,
            None,
        )?;
        references.push(reference);
        let mut linked_items: Vec<u64> = pr.work_items.clone();
        if let Some(item) = pulls_linked.get(&(pr.repo_id.to_ascii_lowercase(), pr.id)) {
            linked_items.push(*item);
        }
        linked_items.extend(mentions(&pr.title));
        linked_items.extend(mentions(&pr.branch));
        // A branch named after a work item (`feature/1234-…`) links it when that item is
        // one of the user's own.
        linked_items.extend(
            branch_numbers
                .find_iter(&pr.branch)
                .filter_map(|m| m.as_str().parse::<u64>().ok())
                .filter(|id| items.contains_key(id)),
        );
        linked_items.sort_unstable();
        linked_items.dedup();
        let own_commits = pr
            .commits
            .iter()
            .filter(|sha| activity.commits.iter().any(|c| &c.sha == *sha))
            .count();
        in_pull_request.extend(pr.commits.iter().cloned());
        let what = format!(
            "PR #{} \"{}\" in {} ({}){}",
            pr.id,
            short(&pr.title, 100),
            pr.repo,
            pr.status,
            if own_commits > 0 {
                format!(", with {own_commits} of the user's commits")
            } else {
                String::new()
            }
        );
        if linked_items.is_empty() {
            unregistered.push(Line {
                date: pr.date.clone(),
                text: format!("{} · {what}.", day(&pr.date)),
            });
        } else {
            registered_commits.extend(pr.commits.iter().cloned());
            attach(&linked_items, what);
        }
    }
    for commit in &activity.commits {
        // Merge commits and commits of the user's pull requests are counted there.
        let merged = merged_pull_request(&commit.message)
            .is_some_and(|id| activity.pull_requests.iter().any(|pr| pr.id == id));
        if merged || in_pull_request.contains(&commit.sha) {
            continue;
        }
        let short_sha: String = commit.sha.chars().take(8).collect();
        references.push(artifact(
            source,
            project,
            "commit",
            &format!("{}:{}", commit.repo_id, commit.sha),
            format!(
                "Commit {short_sha} in {}: {}",
                commit.repo,
                short(&commit.message, 100)
            ),
            vec![short_sha.clone()],
            &["_git", &commit.repo, "commit", &commit.sha],
            &[],
            None,
            None,
        )?);
        let mut linked_items = mentions(&commit.message);
        if let Some(item) = commits_linked.get(&commit.sha) {
            linked_items.push(*item);
        }
        let what = format!(
            "commit {short_sha} in {}: \"{}\"",
            commit.repo,
            short(&commit.message, 120)
        );
        if linked_items.is_empty() {
            unregistered.push(Line {
                date: commit.date.clone(),
                text: format!("{} · {what}.", day(&commit.date)),
            });
        } else {
            registered_commits.insert(commit.sha.clone());
            attach(&linked_items, what);
        }
    }
    let mut registered_runs = BTreeSet::new();
    for run in &activity.runs {
        references.push(artifact(
            source,
            project,
            "pipeline_run",
            &run.id.to_string(),
            format!("Pipeline {}, run #{}", short(&run.definition, 100), run.id),
            vec![format!("run #{}", run.id), format!("build #{}", run.id)],
            &["_build", "results"],
            &[("buildId", run.id.to_string())],
            None,
            None,
        )?);
        let mut linked_items = run.work_items.clone();
        if let Some(item) = builds_linked.get(&run.id) {
            linked_items.push(*item);
        }
        let what = format!(
            "pipeline run #{} of \"{}\", queued by the user: {}",
            run.id,
            short(&run.definition, 100),
            run.result
        );
        if linked_items.is_empty() && !registered_commits.contains(&run.sha) {
            unregistered.push(Line {
                date: run.date.clone(),
                text: format!("{} · {what}.", day(&run.date)),
            });
        } else {
            registered_runs.insert(run.id);
            attach(&linked_items, what);
        }
    }
    let mut registered_releases = BTreeSet::new();
    for release in &activity.releases {
        references.push(artifact(
            source,
            project,
            "release",
            &release.id.to_string(),
            format!(
                "Release {} of {}",
                short(&release.name, 80),
                short(&release.definition, 80)
            ),
            vec![release.name.clone()],
            &["_releaseProgress"],
            &[
                ("_a", "release-pipeline-progress".into()),
                ("releaseId", release.id.to_string()),
            ],
            None,
            None,
        )?);
        let items_of_builds: Vec<u64> = release
            .builds
            .iter()
            .filter_map(|b| builds_linked.get(b).copied())
            .collect();
        let what = format!(
            "release {} of \"{}\", created by the user: {}",
            short(&release.name, 80),
            short(&release.definition, 80),
            release.status
        );
        if items_of_builds.is_empty() && !release.builds.iter().any(|b| registered_runs.contains(b))
        {
            unregistered.push(Line {
                date: release.date.clone(),
                text: format!("{} · {what}.", day(&release.date)),
            });
        } else {
            registered_releases.insert(release.id);
            attach(&items_of_builds, what);
        }
    }
    for approval in &activity.approvals {
        if registered_releases.contains(&approval.release_id) {
            continue;
        }
        if !activity
            .releases
            .iter()
            .any(|r| r.id == approval.release_id)
        {
            references.push(artifact(
                source,
                project,
                "release",
                &approval.release_id.to_string(),
                format!("Release {}", short(&approval.release, 80)),
                vec![approval.release.clone()],
                &["_releaseProgress"],
                &[
                    ("_a", "release-pipeline-progress".into()),
                    ("releaseId", approval.release_id.to_string()),
                ],
                None,
                None,
            )?);
        }
        unregistered.push(Line {
            date: approval.date.clone(),
            text: format!(
                "{} · approved stage \"{}\" of release {}.",
                day(&approval.date),
                short(&approval.stage, 80),
                short(&approval.release, 80)
            ),
        });
    }
    unregistered.sort_by(|a, b| b.date.cmp(&a.date));
    let candidates = unregistered.len();
    let mut text = format!("Project: {project}.\n");
    if unregistered.is_empty() {
        text.push_str("Without a linked work item: nothing found.\n");
    } else {
        text.push_str("Without a linked work item (candidates to register):\n");
        for line in unregistered.iter().take(MAX_LINES) {
            text.push_str(&format!("- {}\n", line.text));
        }
        if unregistered.len() > MAX_LINES {
            text.push_str(&format!(
                "- … and {} more without a work item.\n",
                unregistered.len() - MAX_LINES
            ));
        }
    }
    // Registered work: the user's work items in the window, with what links them.
    let mut registered: Vec<&Value> = items.values().copied().collect();
    registered.sort_by(|a, b| field(b, "System.ChangedDate").cmp(field(a, "System.ChangedDate")));
    let mut listed = 0;
    let mut lines = String::new();
    for item in registered.iter().take(MAX_REGISTERED) {
        let id = item["id"].as_u64().unwrap_or_default();
        references.push(item_reference(source, project, item)?);
        listed += 1;
        let activity = attached.get(&id).map(Vec::as_slice).unwrap_or_default();
        lines.push_str(&format!(
            "- work item {} (changed {}){}\n",
            label(item),
            day(field(item, "System.ChangedDate")),
            if activity.is_empty() {
                String::new()
            } else {
                format!(": {}", activity.join("; "))
            }
        ));
    }
    // Work linked only to work items that are not among the user's own in the window. Only
    // work items read from Azure DevOps are named, so every number shown has a link.
    let others: BTreeMap<u64, &Value> = activity
        .other_items
        .iter()
        .filter_map(|item| Some((item["id"].as_u64()?, item)))
        .collect();
    let mut referenced = BTreeSet::new();
    for (activity, ids) in &elsewhere {
        let verified: Vec<u64> = ids
            .iter()
            .copied()
            .filter(|id| others.contains_key(id))
            .collect();
        for id in &verified {
            if referenced.insert(*id)
                && let Ok(reference) = item_reference(source, project, others[id])
            {
                references.push(reference);
            }
        }
        let shown: Vec<String> = verified.iter().take(5).map(|id| format!("#{id}")).collect();
        let unverified = ids.len() - verified.len();
        lines.push_str(&format!(
            "- {activity}: linked to {}{}{} (not among the user's work items in the window)\n",
            shown.join(", "),
            if verified.len() > shown.len() {
                ", …"
            } else {
                ""
            },
            match (shown.is_empty(), unverified) {
                (_, 0) => String::new(),
                (true, n) => format!("{n} work item(s) that could not be verified"),
                (false, n) => format!(" and {n} that could not be verified"),
            }
        ));
    }
    if lines.is_empty() {
        text.push_str("Registered in work items: nothing changed by the user in the window.\n");
    } else {
        text.push_str("Registered in work items:\n");
        text.push_str(&lines);
        if registered.len() > listed {
            text.push_str(&format!(
                "- … and {} more work items.\n",
                registered.len() - listed
            ));
        }
    }
    Ok((text, references, candidates))
}

async fn commits(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    since: DateTime<Utc>,
    store: &Store,
) -> Result<(Vec<Commit>, bool)> {
    let scope = format!("{}|{}|{project}", source.organization, source.author_email);
    let url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "git", "repositories"],
    )?;
    let repos = cached_read(
        store,
        &format!("ado-repos|{scope}"),
        COLD_REPO_SECONDS,
        request(client, key, Method::GET, url, None),
    )
    .await?;
    let mut found = Vec::new();
    let mut incomplete = false;
    for repo in repos["value"]
        .as_array()
        .context("invalid ADO repositories")?
    {
        // An empty repository has no commits to read (its commits query fails).
        if repo["isDisabled"].as_bool() == Some(true) || repo["size"].as_u64() == Some(0) {
            continue;
        }
        let (Some(id), Some(name)) = (repo["id"].as_str(), repo["name"].as_str()) else {
            continue;
        };
        let mut url = project_url(
            source,
            project,
            "dev.azure.com",
            &["_apis", "git", "repositories", id, "commits"],
        )?;
        url.query_pairs_mut()
            .append_pair("searchCriteria.author", &source.author_email)
            .append_pair("searchCriteria.fromDate", &since.to_rfc3339())
            .append_pair("searchCriteria.$top", "50");
        let read = cached_read(
            store,
            &format!(
                "ado-review-commits|{scope}|{id}|{}",
                since.format("%Y-%m-%d")
            ),
            COMMITS_SECONDS,
            request(client, key, Method::GET, url, None),
        )
        .await;
        let value = match read {
            Ok(value) => value,
            Err(error) => {
                incomplete |= !super::unreadable(&error);
                continue;
            }
        };
        for commit in value["value"].as_array().into_iter().flatten() {
            let date = commit["author"]["date"].as_str().unwrap_or("");
            let (Some(sha), true, true) = (
                commit["commitId"].as_str(),
                commit["author"]["email"]
                    .as_str()
                    .is_some_and(|e| e.eq_ignore_ascii_case(&source.author_email)),
                after(date, since),
            ) else {
                continue;
            };
            found.push(Commit {
                repo_id: id.to_ascii_lowercase(),
                repo: name.into(),
                sha: sha.to_ascii_lowercase(),
                date: date.into(),
                message: commit["comment"].as_str().unwrap_or("").into(),
            });
        }
    }
    found.sort_by(|a, b| b.date.cmp(&a.date));
    found.truncate(60);
    Ok((found, incomplete))
}

async fn pull_requests(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    since: DateTime<Utc>,
) -> Result<Vec<PullRequest>> {
    let mut url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "git", "pullrequests"],
    )?;
    url.query_pairs_mut()
        .append_pair("searchCriteria.status", "all")
        .append_pair("$top", "100");
    let list = request(client, key, Method::GET, url, None).await?;
    let mut found = Vec::new();
    for pr in list["value"].as_array().context("invalid pull requests")? {
        let date = pr["creationDate"].as_str().unwrap_or("");
        let (Some(id), Some(repo_id), true, true) = (
            pr["pullRequestId"].as_u64(),
            pr["repository"]["id"].as_str(),
            same_user(&pr["createdBy"], &source.author_email),
            after(date, since),
        ) else {
            continue;
        };
        let repo = pr["repository"]["name"].as_str().unwrap_or(repo_id);
        let id_text = id.to_string();
        let segments = |last| {
            [
                "_apis",
                "git",
                "repositories",
                repo_id,
                "pullRequests",
                &id_text,
                last,
            ]
        };
        let commits_url = project_url(source, project, "dev.azure.com", &segments("commits"))?;
        let items_url = project_url(source, project, "dev.azure.com", &segments("workitems"))?;
        let (commits, items) = tokio::join!(
            request(client, key, Method::GET, commits_url, None),
            request(client, key, Method::GET, items_url, None)
        );
        found.push(PullRequest {
            repo_id: repo_id.to_ascii_lowercase(),
            repo: repo.into(),
            id,
            title: pr["title"].as_str().unwrap_or("").into(),
            status: pr["status"].as_str().unwrap_or("unknown").into(),
            date: date.into(),
            branch: pr["sourceRefName"].as_str().unwrap_or("").into(),
            commits: commits
                .map(|c| {
                    c["value"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c["commitId"].as_str().map(str::to_ascii_lowercase))
                        .collect()
                })
                .unwrap_or_default(),
            work_items: items.map(|v| ids(&v)).unwrap_or_default(),
        });
        if found.len() >= MAX_PULL_REQUESTS {
            break;
        }
    }
    Ok(found)
}

async fn runs(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    since: DateTime<Utc>,
) -> Result<Vec<Run>> {
    let mut url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "build", "builds"],
    )?;
    url.query_pairs_mut()
        .append_pair("minTime", &since.to_rfc3339())
        .append_pair("queryOrder", "queueTimeDescending")
        .append_pair("$top", "100");
    let list = request(client, key, Method::GET, url, None).await?;
    let mut found = Vec::new();
    for build in list["value"].as_array().context("invalid ADO builds")? {
        // Queued by the user themselves; CI runs started by a push follow their commits.
        let Some(id) = build["id"].as_u64() else {
            continue;
        };
        if !same_user(&build["requestedBy"], &source.author_email) {
            continue;
        }
        let items_url = project_url(
            source,
            project,
            "dev.azure.com",
            &["_apis", "build", "builds", &id.to_string(), "workitems"],
        )?;
        let items = request(client, key, Method::GET, items_url, None)
            .await
            .map(|v| ids(&v))
            .unwrap_or_default();
        found.push(Run {
            id,
            definition: build["definition"]["name"]
                .as_str()
                .unwrap_or("unnamed")
                .into(),
            result: build["result"]
                .as_str()
                .or_else(|| build["status"].as_str())
                .unwrap_or("unknown")
                .into(),
            date: build["queueTime"]
                .as_str()
                .or_else(|| build["startTime"].as_str())
                .unwrap_or("")
                .into(),
            sha: build["sourceVersion"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase(),
            work_items: items,
        });
        if found.len() >= MAX_RUNS {
            break;
        }
    }
    Ok(found)
}

async fn releases(
    client: &Client,
    key: &str,
    source: &Source,
    project: &str,
    since: DateTime<Utc>,
) -> Result<(Vec<Release>, Vec<Approval>)> {
    let mut url = project_url(
        source,
        project,
        "vsrm.dev.azure.com",
        &["_apis", "release", "releases"],
    )?;
    url.query_pairs_mut()
        .append_pair("minCreatedTime", &since.to_rfc3339())
        .append_pair("$expand", "artifacts")
        .append_pair("$top", "50");
    let list = request(client, key, Method::GET, url, None).await?;
    let mut created = Vec::new();
    for release in list["value"].as_array().context("invalid releases")? {
        let Some(id) = release["id"].as_u64() else {
            continue;
        };
        if !same_user(&release["createdBy"], &source.author_email) {
            continue;
        }
        created.push(Release {
            id,
            name: release["name"].as_str().unwrap_or("unnamed").into(),
            definition: release["releaseDefinition"]["name"]
                .as_str()
                .unwrap_or("unnamed")
                .into(),
            status: release["status"].as_str().unwrap_or("unknown").into(),
            date: release["createdOn"].as_str().unwrap_or("").into(),
            builds: release["artifacts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| {
                    a["definitionReference"]["version"]["id"]
                        .as_str()
                        .and_then(|v| v.parse().ok())
                })
                .collect(),
        });
        if created.len() >= MAX_RELEASES {
            break;
        }
    }
    let mut url = project_url(
        source,
        project,
        "vsrm.dev.azure.com",
        &["_apis", "release", "approvals"],
    )?;
    url.query_pairs_mut()
        .append_pair("statusFilter", "approved")
        .append_pair("queryOrder", "descending")
        .append_pair("top", "50");
    let mut approvals = Vec::new();
    // Approvals are optional evidence: a failure leaves releases intact.
    if let Ok(list) = request(client, key, Method::GET, url, None).await {
        for approval in list["value"].as_array().into_iter().flatten() {
            let date = approval["modifiedOn"].as_str().unwrap_or("");
            let Some(release_id) = approval["release"]["id"].as_u64() else {
                continue;
            };
            if !same_user(&approval["approvedBy"], &source.author_email) || !after(date, since) {
                continue;
            }
            approvals.push(Approval {
                release_id,
                release: approval["release"]["name"]
                    .as_str()
                    .unwrap_or("unnamed")
                    .into(),
                stage: approval["releaseEnvironment"]["name"]
                    .as_str()
                    .unwrap_or("unnamed")
                    .into(),
                date: date.into(),
            });
            if approvals.len() >= MAX_RELEASES {
                break;
            }
        }
    }
    Ok((created, approvals))
}

async fn project_activity(
    client: &Client,
    key: &str,
    source: &Source,
    project: String,
    since: DateTime<Utc>,
    store: &Store,
) -> ProjectActivity {
    let email = &source.author_email;
    let query = format!(
        "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND [System.ChangedDate] >= '{}' AND ([System.AssignedTo] = '{email}' OR [System.ChangedBy] = '{email}' OR [System.CreatedBy] = '{email}') ORDER BY [System.ChangedDate] DESC",
        since.format("%Y-%m-%d")
    );
    let items = async {
        let ids: BTreeSet<u64> = wiql(client, key, source, &project, &query)
            .await?
            .into_iter()
            .take(80)
            .collect();
        work_items(client, key, source, &project, &ids).await
    };
    let (items, commits, pull_requests, runs, releases) = tokio::join!(
        items,
        commits(client, key, source, &project, since, store),
        pull_requests(client, key, source, &project, since),
        runs(client, key, source, &project, since),
        releases(client, key, source, &project, since),
    );
    let mut activity = ProjectActivity {
        project,
        ..Default::default()
    };
    // Work items the activity links or names that are not the user's own: read them so the
    // review can name and link them (bounded).
    let own: BTreeSet<u64> = items
        .as_ref()
        .map(|items| items.iter().filter_map(|i| i["id"].as_u64()).collect())
        .unwrap_or_default();
    let mut foreign: BTreeSet<u64> = BTreeSet::new();
    if let Ok((found, _)) = &commits {
        foreign.extend(found.iter().flat_map(|c| mentions(&c.message)));
    }
    if let Ok(found) = &pull_requests {
        for pr in found {
            foreign.extend(pr.work_items.iter().copied());
            foreign.extend(mentions(&pr.title));
            foreign.extend(mentions(&pr.branch));
        }
    }
    if let Ok(found) = &runs {
        foreign.extend(found.iter().flat_map(|r| r.work_items.iter().copied()));
    }
    let foreign: BTreeSet<u64> = foreign.difference(&own).copied().take(60).collect();
    if !foreign.is_empty() {
        activity.other_items = work_items(client, key, source, &activity.project, &foreign)
            .await
            .unwrap_or_default();
    }
    match items {
        Ok(items) => activity.items = items,
        Err(_) => activity.failed.push("work items"),
    }
    match commits {
        Ok((commits, incomplete)) => {
            activity.commits = commits;
            if incomplete {
                activity.failed.push("some repositories' commits");
            }
        }
        Err(_) => activity.failed.push("commits"),
    }
    match pull_requests {
        Ok(found) => activity.pull_requests = found,
        Err(_) => activity.failed.push("pull requests"),
    }
    match runs {
        Ok(found) => activity.runs = found,
        Err(_) => activity.failed.push("pipeline runs"),
    }
    match releases {
        Ok((found, approvals)) => {
            activity.releases = found;
            activity.approvals = approvals;
        }
        Err(_) => activity.failed.push("releases"),
    }
    activity
}

/// The user's own Azure DevOps activity in the last `days` days, compared with their work
/// items: commits, pull requests, pipeline runs they queued, releases they created and
/// approvals they gave, each with a verified link.
pub async fn review(
    client: &Client,
    key: &str,
    catalog_text: &str,
    days: i64,
    store: Arc<Store>,
) -> Result<Evidence> {
    let catalog = Catalog::parse(catalog_text)?;
    let since = Utc::now() - Duration::days(days);
    let mut jobs = tokio::task::JoinSet::new();
    let mut sections = Vec::new();
    for source in &catalog.sources {
        for project in projects(client, key, source).await? {
            let (client, key, source, store) = (
                client.clone(),
                key.to_owned(),
                source.clone(),
                store.clone(),
            );
            jobs.spawn(async move {
                let activity =
                    project_activity(&client, &key, &source, project, since, &store).await;
                (source, activity)
            });
            if jobs.len() >= 8
                && let Some(done) = jobs.join_next().await
            {
                sections.push(done?);
            }
        }
    }
    while let Some(done) = jobs.join_next().await {
        sections.push(done?);
    }
    let mut text = format!(
        "Azure DevOps activity review from {} to {} (last {days} days). It includes only the user's own work: commits they authored, pull requests they created, pipeline runs they queued, releases they created and approvals they gave, compared with the work items they were assigned, created or changed.\n",
        since.format("%Y-%m-%d"),
        Utc::now().format("%Y-%m-%d"),
    );
    let mut references = Vec::new();
    let mut partial = Vec::new();
    let mut found = Vec::new();
    for (source, activity) in &sections {
        if !activity.failed.is_empty() {
            partial.push(format!(
                "{} ({})",
                activity.project,
                activity.failed.join(", ")
            ));
        }
        let empty = activity.items.is_empty()
            && activity.commits.is_empty()
            && activity.pull_requests.is_empty()
            && activity.runs.is_empty()
            && activity.releases.is_empty()
            && activity.approvals.is_empty();
        if empty {
            continue;
        }
        let (section, refs, candidates) = summarize(source, activity)?;
        found.push((candidates, section));
        references.extend(refs);
    }
    // Projects with more unregistered work first.
    found.sort_by_key(|(candidates, _)| std::cmp::Reverse(*candidates));
    if found.is_empty() {
        text.push_str("No own activity was found in the window in the projects read.\n");
    }
    for (_, section) in found {
        text.push_str(&section);
    }
    if !partial.is_empty() {
        text.push_str(&format!(
            "Partial coverage: these reads failed: {}. Do not infer that there was no activity there.\n",
            partial.join("; ")
        ));
    }
    let mut seen = BTreeSet::new();
    references.retain(|r| seen.insert(r.id.clone()));
    Ok(Evidence {
        text: bounded_lines(&text, 14_000),
        references,
        partial: !partial.is_empty(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source() -> Source {
        Source {
            organization: "https://dev.azure.com/example".into(),
            projects: vec!["Payments".into()],
            author_email: "me@example.com".into(),
        }
    }
    fn item(id: u64, links: &[&str]) -> Value {
        json!({
            "id": id,
            "fields": {
                "System.TeamProject": "Payments",
                "System.Title": format!("Task {id}"),
                "System.State": "Active",
                "System.ChangedDate": "2026-09-30T10:00:00Z",
            },
            "relations": links.iter().map(|url| json!({"rel":"ArtifactLink","url":url})).collect::<Vec<_>>(),
        })
    }
    fn commit(sha: &str, message: &str) -> Commit {
        Commit {
            repo_id: "repo-1".into(),
            repo: "payments-api".into(),
            sha: sha.into(),
            date: "2026-09-29T09:00:00Z".into(),
            message: message.into(),
        }
    }

    #[test]
    fn artifact_links_and_mentions_are_parsed_strictly() {
        assert_eq!(
            linked("vstfs:///Git/Commit/proj%2Frepo-1%2FABCDEF12"),
            Some(Linked::Commit("abcdef12".into()))
        );
        assert_eq!(
            linked("vstfs:///Git/PullRequestId/proj%2FRepo-1%2F45"),
            Some(Linked::PullRequest("repo-1".into(), 45))
        );
        assert_eq!(linked("vstfs:///Build/Build/812"), Some(Linked::Build(812)));
        assert_eq!(linked("https://evil.example/Build/Build/1"), None);
        assert_eq!(linked("vstfs:///Git/Ref/proj%2Frepo%2FGBmain"), None);
        assert_eq!(mentions("Fix login AB#123 and #45; not x#9"), vec![123, 45]);
        assert_eq!(
            mentions("Bug 4321: corregir redondeo; HU 8765 - Paso a producción"),
            vec![4321, 8765]
        );
        assert!(mentions("Release 2 of version 1.10.0").is_empty());
        assert_eq!(
            merged_pull_request("Merged PR 46: unlinked change"),
            Some(46)
        );
        assert_eq!(
            merged_pull_request("Merge pull request 4935 from chore/bump into main"),
            Some(4935)
        );
        assert_eq!(merged_pull_request("Merge branch 'main'"), None);
    }

    #[test]
    fn work_without_a_linked_item_becomes_a_candidate() {
        let activity = ProjectActivity {
            project: "Payments".into(),
            items: vec![
                item(100, &["vstfs:///Git/Commit/proj%2Frepo-1%2Faaaaaaaa11"]),
                item(200, &[]),
            ],
            commits: vec![
                commit("aaaaaaaa11", "Linked from the work item"),
                commit("bbbbbbbb22", "Mentions AB#200 in the message"),
                commit("cccccccc33", "Hotfix without any task"),
                commit("dddddddd44", "Part of an unlinked PR"),
                commit("eeeeeeee55", "Merged PR 46: unlinked change"),
            ],
            pull_requests: vec![
                PullRequest {
                    repo_id: "repo-1".into(),
                    repo: "payments-api".into(),
                    id: 46,
                    title: "Unlinked change".into(),
                    status: "completed".into(),
                    date: "2026-09-28T12:00:00Z".into(),
                    branch: "refs/heads/fix/timeout".into(),
                    commits: vec!["dddddddd44".into()],
                    work_items: vec![],
                },
                PullRequest {
                    repo_id: "repo-1".into(),
                    repo: "payments-api".into(),
                    id: 47,
                    title: "Branch names the task".into(),
                    status: "active".into(),
                    date: "2026-09-27T12:00:00Z".into(),
                    branch: "refs/heads/feature/200-login".into(),
                    commits: vec![],
                    work_items: vec![],
                },
            ],
            runs: vec![Run {
                id: 812,
                definition: "Deploy QA".into(),
                result: "succeeded".into(),
                date: "2026-09-26T08:00:00Z".into(),
                ..Default::default()
            }],
            releases: vec![Release {
                id: 31,
                name: "Release-31".into(),
                definition: "Payments".into(),
                status: "active".into(),
                date: "2026-09-25T08:00:00Z".into(),
                builds: vec![812],
            }],
            other_items: vec![],
            approvals: vec![Approval {
                release_id: 30,
                release: "Release-30".into(),
                stage: "PROD".into(),
                date: "2026-09-24T08:00:00Z".into(),
            }],
            failed: vec![],
        };
        let (text, references, candidates) = summarize(&source(), &activity).unwrap();
        let (unregistered, registered) = text.split_once("Registered in work items:").expect(&text);
        // Unlinked PR (with its commit folded in), the hotfix, the manual run, the release
        // of that run and the approval.
        assert_eq!(candidates, 5, "{text}");
        for expected in [
            "PR #46 \"Unlinked change\" in payments-api (completed), with 1 of the user's commits",
            "commit cccccccc in payments-api: \"Hotfix without any task\"",
            "pipeline run #812 of \"Deploy QA\"",
            "release Release-31",
            "approved stage \"PROD\" of release Release-30",
        ] {
            assert!(unregistered.contains(expected), "{expected}\n{text}");
        }
        for folded in ["dddddddd", "eeeeeeee", "aaaaaaaa", "bbbbbbbb", "PR #47"] {
            assert!(!unregistered.contains(folded), "{folded}\n{text}");
        }
        assert!(registered.contains("#100 Task 100 — Active"));
        assert!(registered.contains("commit aaaaaaaa"));
        assert!(registered.contains("#200 Task 200 — Active"));
        assert!(registered.contains("commit bbbbbbbb"));
        assert!(registered.contains("PR #47"));
        // Every entity has a verified link inside the authorized project.
        for r in &references {
            r.validate().unwrap();
        }
        let urls: Vec<_> = references.iter().map(|r| r.url.as_str()).collect();
        assert!(
            urls.contains(
                &"https://dev.azure.com/example/Payments/_git/payments-api/pullrequest/46"
            )
        );
        assert!(urls.contains(
            &"https://dev.azure.com/example/Payments/_git/payments-api/commit/cccccccc33"
        ));
        assert!(urls.contains(
            &"https://dev.azure.com/example/Payments/_releaseProgress?_a=release-pipeline-progress&releaseId=31"
        ));
        assert!(
            urls.contains(&"https://dev.azure.com/example/Payments/_build/results?buildId=812")
        );
    }

    #[test]
    fn only_work_items_read_from_azure_devops_are_named() {
        let mut other = item(555, &[]);
        other["fields"]["System.Title"] = json!("Someone else's task");
        let activity = ProjectActivity {
            project: "Payments".into(),
            other_items: vec![other],
            pull_requests: vec![PullRequest {
                repo_id: "repo-1".into(),
                repo: "payments-api".into(),
                id: 48,
                title: "Shared fix".into(),
                status: "completed".into(),
                date: "2026-09-28T12:00:00Z".into(),
                work_items: vec![555, 8765],
                ..Default::default()
            }],
            runs: vec![Run {
                id: 901,
                definition: "Nightly".into(),
                result: "succeeded".into(),
                date: "2026-09-26T08:00:00Z".into(),
                work_items: vec![6, 8],
                ..Default::default()
            }],
            ..Default::default()
        };
        let (text, references, candidates) = summarize(&source(), &activity).unwrap();
        assert_eq!(candidates, 0, "{text}");
        assert!(text.contains("PR #48 \"Shared fix\" in payments-api (completed): linked to #555 and 1 that could not be verified"), "{text}");
        assert!(text.contains("pipeline run #901 of \"Nightly\", queued by the user: succeeded: linked to 2 work item(s) that could not be verified"), "{text}");
        assert!(!text.contains("#8765") && !text.contains("#6") && !text.contains("#8,"));
        assert!(
            references
                .iter()
                .any(|r| r.url.ends_with("/_workitems/edit/555"))
        );
    }

    #[test]
    fn a_run_of_a_registered_commit_and_its_release_are_registered() {
        let activity = ProjectActivity {
            project: "Payments".into(),
            items: vec![item(100, &["vstfs:///Build/Build/900"])],
            runs: vec![Run {
                id: 900,
                definition: "Deploy".into(),
                result: "succeeded".into(),
                date: "2026-09-26T08:00:00Z".into(),
                ..Default::default()
            }],
            releases: vec![Release {
                id: 32,
                name: "Release-32".into(),
                definition: "Payments".into(),
                status: "active".into(),
                date: "2026-09-26T09:00:00Z".into(),
                builds: vec![900],
            }],
            ..Default::default()
        };
        let (text, _, candidates) = summarize(&source(), &activity).unwrap();
        assert_eq!(candidates, 0, "{text}");
        assert!(text.contains("Without a linked work item: nothing found."));
        assert!(text.contains("pipeline run #900"));
        assert!(text.contains("release Release-32"));
    }
}
