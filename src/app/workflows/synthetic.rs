//! A synthetic Azure DevOps catalog for the engine's tests: work items, who they are assigned
//! to and the pull requests of each repository branch. It has no way to write: every request
//! the engine makes is recorded, and each is a read.
// Only the Unix tests use the catalog (the fake provider CLIs are shell scripts), so on Windows its
// items are unused: allowed there rather than gated one by one.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
use crate::ado::workflow::PullRequest;
use crate::tools::todo::{Detail, DetailField, Item};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

#[derive(Default)]
pub(in crate::app) struct Catalog {
    items: Mutex<BTreeMap<u64, Detail>>,
    assigned: Mutex<BTreeSet<u64>>,
    pulls: Mutex<BTreeMap<(String, String), Vec<PullRequest>>>,
    /// The branches that exist, by repository (lowercased) and name.
    branches: Mutex<BTreeSet<(String, String)>>,
    /// Every request, as `GET what`.
    pub requests: Mutex<Vec<String>>,
    /// The items whose reads fail, as during an outage.
    pub failing_items: Mutex<BTreeSet<u64>>,
    /// Pull request reads fail.
    pub failing_pulls: std::sync::atomic::AtomicBool,
    /// The ready-state listings fail, as during an outage.
    pub failing_lists: std::sync::atomic::AtomicBool,
    /// The listing of this one state fails.
    pub failing_state: Mutex<Option<String>>,
    /// The most items a listing may return, whatever the limit asked (a catalog larger than
    /// the read's bound).
    pub list_cap: Mutex<Option<usize>>,
    /// The task-category types whose work item guide lists them with the bugs (a development
    /// bug): the To-Do list without a kind holds them, its `task` kind does not.
    pub listed_with_bugs: Mutex<BTreeSet<String>>,
}

#[cfg(unix)]
pub(in crate::app) const ORGANIZATION: &str = "https://dev.azure.com/example";

/// A work item of project Payments.
#[cfg(unix)]
pub(in crate::app) fn detail(
    id: u64,
    kind: &str,
    category: &str,
    title: &str,
    text: &str,
) -> Detail {
    Detail {
        item: Item {
            id,
            title: title.into(),
            url: format!("https://dev.azure.com/example/Payments/_workitems/edit/{id}"),
            project: "Payments".into(),
            state: "Ready".into(),
            kind: kind.into(),
            category: category.into(),
            created_at: None,
            changed_at: None,
            dates: Vec::new(),
        },
        fields: vec![DetailField {
            reference: "System.Description".into(),
            name: "Description".into(),
            kind: "html".into(),
            value: serde_json::Value::String(format!("<p>{text}</p>")),
            content: None,
        }],
        comments: Vec::new(),
        relations: Vec::new(),
        history: Vec::new(),
        partial: false,
        warnings: Vec::new(),
    }
}

impl Catalog {
    #[cfg(unix)]
    pub(in crate::app) fn add(&self, detail: Detail, assigned: bool) {
        if assigned {
            self.assigned.lock().unwrap().insert(detail.item.id);
        }
        self.items.lock().unwrap().insert(detail.item.id, detail);
    }
    /// The pull requests of a repository's branch, as the next read returns them.
    #[cfg(unix)]
    pub(in crate::app) fn set_pulls(
        &self,
        repository: &str,
        branch: &str,
        pulls: Vec<PullRequest>,
    ) {
        self.pulls
            .lock()
            .unwrap()
            .insert((repository.to_lowercase(), branch.to_owned()), pulls);
    }
    /// A branch that exists in a repository, as the next read returns it.
    #[cfg(unix)]
    pub(in crate::app) fn set_branch(&self, repository: &str, branch: &str) {
        self.branches
            .lock()
            .unwrap()
            .insert((repository.to_lowercase(), branch.to_owned()));
    }
    pub(in crate::app) fn branch_exists(&self, repository: &str, branch: &str) -> bool {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET refs {repository} heads/{branch}"));
        self.branches
            .lock()
            .unwrap()
            .contains(&(repository.to_lowercase(), branch.to_owned()))
    }
    /// Add a comment to an item, as its next read returns it.
    #[cfg(unix)]
    pub(in crate::app) fn comment(&self, id: u64, author: &str, text: &str) {
        if let Some(detail) = self.items.lock().unwrap().get_mut(&id) {
            detail.comments.push(crate::tools::todo::Comment {
                author: author.into(),
                created_at: Some("2026-11-10T10:00:00Z".into()),
                modified_at: None,
                text: text.into(),
                format: "markdown".into(),
                content: None,
            });
        }
    }
    pub(in crate::app) fn item(&self, id: u64) -> Result<Detail, String> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET workitems/{id}"));
        if self.failing_items.lock().unwrap().contains(&id) {
            return Err("[network] Azure work items could not be read.".into());
        }
        self.items
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("[invalid_input] invalid work item: {id} not found"))
    }
    pub(in crate::app) fn assigned(&self, id: u64) -> bool {
        self.assigned.lock().unwrap().contains(&id)
    }
    pub(in crate::app) fn pull_requests(
        &self,
        repository: &str,
        branch: &str,
    ) -> Result<Vec<PullRequest>, String> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET pullrequests {repository} refs/heads/{branch}"));
        if self.failing_pulls.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("[network] The project's pull requests could not be read.".into());
        }
        Ok(self
            .pulls
            .lock()
            .unwrap()
            .get(&(repository.to_lowercase(), branch.to_owned()))
            .cloned()
            .unwrap_or_default())
    }
    /// Change the state an item is in.
    #[cfg(unix)]
    pub(in crate::app) fn set_state(&self, id: u64, state: &str) {
        if let Some(detail) = self.items.lock().unwrap().get_mut(&id) {
            detail.item.state = state.into();
        }
    }
    /// The items assigned to the user in a state of a project, newest ID first, at most
    /// `limit` (with `has_more` when there are others): the To-Do read the watcher makes.
    /// Without a kind it holds the HUs, the bugs and the task types listed with them; the
    /// `task` kind holds every other task-category type, as the To-Do list groups them.
    pub(in crate::app) fn ready_items(
        &self,
        source: Option<&str>,
        project: &str,
        state: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<crate::tools::todo::List, String> {
        let asked = kind.map(|k| format!(" kind {k}")).unwrap_or_default();
        self.requests.lock().unwrap().push(format!(
            "GET list {project} {state}{asked} limit {limit} source {}",
            source.unwrap_or("-")
        ));
        if self.failing_state.lock().unwrap().as_deref() == Some(state) {
            return Err("[network] Azure work items could not be read.".into());
        }
        if self.failing_lists.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("[network] Azure work items could not be read.".into());
        }
        let listed = self.listed_with_bugs.lock().unwrap().clone();
        let group = |item: &Item| match item.category.as_str() {
            "requirement" => "hu",
            "task" if !listed.contains(&item.kind) => "task",
            _ => "bug",
        };
        let assigned = self.assigned.lock().unwrap().clone();
        let mut items: Vec<Item> = self
            .items
            .lock()
            .unwrap()
            .values()
            .filter(|d| {
                assigned.contains(&d.item.id)
                    && kind.map_or(group(&d.item) != "task", |k| group(&d.item) == k)
                    && d.item.state == state
                    && d.item.project.eq_ignore_ascii_case(project)
            })
            .map(|d| d.item.clone())
            .collect();
        items.sort_by_key(|a| std::cmp::Reverse(a.id));
        let limit = self
            .list_cap
            .lock()
            .unwrap()
            .map_or(limit, |c| c.min(limit));
        let has_more = items.len() > limit;
        items.truncate(limit);
        Ok(crate::tools::todo::List {
            items,
            has_more,
            ..Default::default()
        })
    }
    /// Every request was a read.
    #[cfg(unix)]
    pub(in crate::app) fn only_reads(&self) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.starts_with("GET "))
    }
}
