//! A synthetic Azure DevOps catalog for the engine's tests: work items, who they are assigned
//! to and the pull requests of each repository branch. It has no way to write: every request
//! the engine makes is recorded, and each is a read.
use crate::ado::workflow::PullRequest;
use crate::tools::todo::{Detail, DetailField, Item};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

#[derive(Default)]
pub(in crate::app) struct Catalog {
    items: Mutex<BTreeMap<u64, Detail>>,
    assigned: Mutex<BTreeSet<u64>>,
    pulls: Mutex<BTreeMap<(String, String), Vec<PullRequest>>>,
    /// Every request, as `GET what`.
    pub requests: Mutex<Vec<String>>,
}

pub(in crate::app) const ORGANIZATION: &str = "https://dev.azure.com/example";

/// A work item of project Payments.
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
    pub(in crate::app) fn add(&self, detail: Detail, assigned: bool) {
        if assigned {
            self.assigned.lock().unwrap().insert(detail.item.id);
        }
        self.items.lock().unwrap().insert(detail.item.id, detail);
    }
    /// The pull requests of a repository's branch, as the next read returns them.
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
    pub(in crate::app) fn item(&self, id: u64) -> Result<Detail, String> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET workitems/{id}"));
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
    pub(in crate::app) fn pull_requests(&self, repository: &str, branch: &str) -> Vec<PullRequest> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET pullrequests {repository} refs/heads/{branch}"));
        self.pulls
            .lock()
            .unwrap()
            .get(&(repository.to_lowercase(), branch.to_owned()))
            .cloned()
            .unwrap_or_default()
    }
    /// Every request was a read.
    pub(in crate::app) fn only_reads(&self) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.starts_with("GET "))
    }
}
