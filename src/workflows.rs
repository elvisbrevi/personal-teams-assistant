//! Workflows: saved recipes for implementing the items of one Azure DevOps project, and the
//! reusable blocks they are made of. The map keeps them (`[[workflows]]`, `[[workflow_blocks]]`)
//! and they travel with the account; both sections are left out while empty, so an older host
//! keeps loading the map.
//!
//! A workflow names the project, the repositories it works on (by their ID and name in the
//! project's Git catalog), the execution machine, the checkout directory of each repository on
//! it, the coding agent, its instructions and an ordered list of blocks. A block is an
//! instruction the agents read, a Teams notification to one chat at one stage, or an approved
//! launcher of the coordinator run at one stage. Nothing here grants anything: when a
//! definition is saved the host checks its project, repositories, checkout directories and
//! agent on the machine (`check`), a notification's chat against the user's chats when a
//! Microsoft session can read them, and a launcher's approval; mentions are resolved against
//! the chat's members only when a notification is posted. Everything is checked again when it
//! is used, and instructions reach the agents as untrusted input. Directories are compared as
//! the execution machine spells them (`absolute_on`, `under`), whatever system the coordinator
//! runs.

pub mod director;

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

pub const MAX_WORKFLOWS: usize = 50;
pub const MAX_BLOCKS: usize = 100;
pub const MAX_REPOSITORIES: usize = 20;
pub const MAX_WORKFLOW_BLOCKS: usize = 10;
pub const MAX_INSTRUCTIONS: usize = 8000;
pub const MAX_MENTIONS: usize = 20;
/// The longest display name a mention can carry: a mention is written `@[Name]` and read back
/// by the Teams post, which takes up to 100 characters and no closing bracket.
pub const MAX_MENTION: usize = 100;
pub const MAX_TEMPLATE: usize = 4000;
/// The placeholders a notification template may hold; code renders them.
pub const PLACEHOLDERS: [&str; 5] = ["item", "link", "pull_requests", "summary", "state"];
/// The coding agents a workflow can run (the installed CLIs of the implementation flow).
pub const PROVIDERS: [&str; 3] = ["codex", "claude", "opencode"];

/// A saved recipe for implementing items of one Azure DevOps project.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    /// What the workflow director reads to choose it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub enabled: bool,
    /// Starts by itself when an assigned item reaches a ready state.
    #[serde(default)]
    pub auto_start: bool,
    /// The Azure DevOps organization URL (`https://dev.azure.com/NAME`).
    pub organization: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<Repository>,
    /// The execution machine's ID (the coordinator's own ID for the coordinator).
    pub machine: String,
    /// Repository ID to its canonical checkout directory on the machine.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checkouts: BTreeMap<String, PathBuf>,
    pub agent: Agent,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    /// Block IDs, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,
    /// When it last changed (Unix seconds); set by the host when it is saved.
    #[serde(default)]
    pub updated_at: i64,
}

/// A repository of the project's Git catalog, as the workflow stores it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    /// `codex`, `claude` or `opencode`.
    pub provider: String,
    pub model: String,
    pub effort: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Text the planning and coding agents read.
    #[default]
    Instruction,
    /// A message posted to one Teams chat at one stage.
    TeamsNotification,
    /// An approved launcher of the coordinator run at one stage.
    Launcher,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    BeforePlanning,
    PlanReady,
    ChildrenCreated,
    ChildDone,
    PullRequestsCompleted,
    Done,
    Failed,
}

/// A reusable part of a workflow. Only the fields of its kind are set.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// When an acting block runs; an instruction has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    /// An instruction's text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// A notification's chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<Chat>,
    /// The display names of the chat members a notification mentions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
    /// A notification's text, with `{item}`, `{link}`, `{pull_requests}`, `{summary}` and `{state}`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub template: String,
    /// A launcher block's approved launcher of the coordinator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<LauncherRef>,
    /// When it last changed (Unix seconds); set by the host when it is saved.
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Chat {
    /// The Graph chat ID, as `pta teams chats` lists it.
    pub id: String,
    /// Its display name, shown when the block is reviewed.
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LauncherRef {
    pub id: String,
    /// The SHA-256 of the approved program, as the launcher list shows it.
    pub digest: String,
}

fn plain(text: &str, max: usize) -> bool {
    text.chars().count() <= max
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}
/// A member's display name as a mention can name it.
fn mention_name(name: &str) -> bool {
    line(name, MAX_MENTION) && !name.contains(']')
}
fn line(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}
fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && id.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}
fn valid_uuid(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok() && id.len() == 36
}

/// The organization's name in a `https://dev.azure.com/NAME` URL, lowercased.
pub fn organization_name(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    (url.scheme() == "https"
        && url.host_str() == Some("dev.azure.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && segments.len() == 1)
        .then(|| decode(segments[0]).to_lowercase())
}

/// Every unknown `{placeholder}` of a template.
fn unknown_placeholders(template: &str) -> Vec<String> {
    let mut unknown = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('}') else { break };
        let name = &rest[..end];
        if !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !PLACEHOLDERS.contains(&name)
        {
            unknown.push(name.to_owned());
        }
        rest = &rest[end + 1..];
    }
    unknown
}

/// One block alone: its ID, name and the fields of its kind.
pub fn validate_block(block: &Block) -> Result<()> {
    ensure!(
        valid_id(&block.id),
        "A workflow block ID is lowercase letters, digits, - and _ (up to 64) and starts with a letter."
    );
    ensure!(
        line(&block.name, 80),
        "Workflow block {} needs a name of at most 80 characters.",
        block.id
    );
    ensure!(block.updated_at >= 0, "Invalid date in block {}.", block.id);
    let id = &block.id;
    match block.kind {
        Kind::Instruction => {
            ensure!(
                block.stage.is_none(),
                "Instruction block {id} has no stage: the agents read it while they plan and implement."
            );
            ensure!(
                !block.text.trim().is_empty() && plain(&block.text, MAX_INSTRUCTIONS),
                "Instruction block {id} needs a text of at most {MAX_INSTRUCTIONS} characters."
            );
            ensure!(
                block.chat.is_none()
                    && block.mentions.is_empty()
                    && block.template.is_empty()
                    && block.launcher.is_none(),
                "Instruction block {id} holds only its text."
            );
        }
        Kind::TeamsNotification => {
            ensure!(
                block.stage.is_some(),
                "Teams notification block {id} needs the stage at which it posts."
            );
            let chat = block.chat.as_ref().ok_or_else(|| {
                anyhow::anyhow!("Teams notification block {id} needs its chat (ID and name).")
            })?;
            ensure!(
                crate::adapters::teams::canonical_resource(&format!(
                    "chats/{}/messages/0",
                    chat.id
                ))
                .is_ok()
                    && line(&chat.name, 200),
                "Teams notification block {id} needs a chat ID as pta teams chats lists it and the chat's name (at most 200 characters)."
            );
            ensure!(
                block.mentions.len() <= MAX_MENTIONS
                    && block.mentions.iter().all(|m| mention_name(m))
                    && block.mentions.iter().collect::<BTreeSet<_>>().len() == block.mentions.len(),
                "Teams notification block {id} mentions at most {MAX_MENTIONS} different members by display name (at most {MAX_MENTION} characters each, without a closing bracket, as a Teams mention can be written)."
            );
            ensure!(
                !block.template.trim().is_empty() && plain(&block.template, MAX_TEMPLATE),
                "Teams notification block {id} needs a template of at most {MAX_TEMPLATE} characters."
            );
            let unknown = unknown_placeholders(&block.template);
            ensure!(
                unknown.is_empty(),
                "Teams notification block {id} uses unknown placeholders ({}); the template may use {{item}}, {{link}}, {{pull_requests}}, {{summary}} and {{state}}.",
                unknown.join(", ")
            );
            ensure!(
                block.text.is_empty() && block.launcher.is_none(),
                "Teams notification block {id} holds only its stage, chat, mentions and template."
            );
        }
        Kind::Launcher => {
            ensure!(
                block.stage.is_some(),
                "Launcher block {id} needs the stage at which it runs."
            );
            let launcher = block.launcher.as_ref().ok_or_else(|| {
                anyhow::anyhow!("Launcher block {id} needs its launcher (ID and digest).")
            })?;
            ensure!(
                crate::consumers::validate_launcher_id(&launcher.id).is_ok()
                    && launcher.digest.len() == 64
                    && launcher
                        .digest
                        .chars()
                        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "Launcher block {id} needs a launcher name and its program digest (64 lowercase hexadecimal characters)."
            );
            ensure!(
                block.text.is_empty()
                    && block.chat.is_none()
                    && block.mentions.is_empty()
                    && block.template.is_empty(),
                "Launcher block {id} holds only its stage and launcher."
            );
        }
    }
    Ok(())
}

/// One workflow alone: everything that does not need a machine, a catalog or other blocks.
pub fn validate_workflow(workflow: &Workflow) -> Result<()> {
    let id = &workflow.id;
    ensure!(
        valid_id(id),
        "A workflow ID is lowercase letters, digits, - and _ (up to 64) and starts with a letter."
    );
    ensure!(
        line(&workflow.name, 80) && plain(&workflow.description, 2000),
        "Workflow {id} needs a name of at most 80 characters; its description takes at most 2000."
    );
    ensure!(
        plain(&workflow.instructions, MAX_INSTRUCTIONS),
        "The instructions of workflow {id} take at most {MAX_INSTRUCTIONS} characters."
    );
    ensure!(
        organization_name(&workflow.organization).is_some(),
        "Workflow {id} needs its Azure DevOps organization URL (https://dev.azure.com/NAME)."
    );
    ensure!(
        line(&workflow.project, 120),
        "Workflow {id} needs its Azure DevOps project (at most 120 characters)."
    );
    ensure!(
        !workflow.repositories.is_empty() && workflow.repositories.len() <= MAX_REPOSITORIES,
        "Workflow {id} works on between 1 and {MAX_REPOSITORIES} repositories."
    );
    let mut repositories = BTreeSet::new();
    for repository in &workflow.repositories {
        ensure!(
            valid_uuid(&repository.id)
                && line(&repository.name, 200)
                && repositories.insert(repository.id.to_lowercase()),
            "Workflow {id}: each repository is listed once, by its catalog ID and name (pta workflows repositories)."
        );
    }
    ensure!(
        valid_uuid(&workflow.machine),
        "Workflow {id} needs the ID of its execution machine (pta tools call machine_list)."
    );
    let mut keys = BTreeSet::new();
    let mut directories: Vec<&str> = Vec::new();
    for (repository, directory) in &workflow.checkouts {
        ensure!(
            repositories.contains(&repository.to_lowercase()),
            "Workflow {id} has a checkout for repository {repository}, which it does not list."
        );
        ensure!(
            keys.insert(repository.to_lowercase()),
            "Workflow {id} has two checkouts for repository {repository}."
        );
        let directory = directory.to_str().unwrap_or_default();
        ensure!(
            absolute(directory) && !directories.iter().any(|d| same_directory(d, directory)),
            "Workflow {id}: each checkout is a different absolute directory on the machine."
        );
        directories.push(directory);
    }
    for repository in &workflow.repositories {
        ensure!(
            workflow
                .checkouts
                .keys()
                .any(|r| r.eq_ignore_ascii_case(&repository.id)),
            "Workflow {id} needs the checkout directory of repository {}.",
            repository.name
        );
    }
    let agent = &workflow.agent;
    ensure!(
        PROVIDERS.contains(&agent.provider.as_str())
            && crate::llm::valid_model(&agent.model)
            && !agent.effort.is_empty()
            && agent.effort.len() <= 40
            && agent
                .effort
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "Workflow {id} needs its coding agent: provider (codex, claude or opencode), model and effort."
    );
    ensure!(
        workflow.blocks.len() <= MAX_WORKFLOW_BLOCKS,
        "Workflow {id} has at most {MAX_WORKFLOW_BLOCKS} blocks."
    );
    ensure!(
        workflow.blocks.iter().collect::<BTreeSet<_>>().len() == workflow.blocks.len(),
        "Workflow {id} lists each block once."
    );
    ensure!(workflow.updated_at >= 0, "Invalid date in workflow {id}.");
    Ok(())
}

/// The map's sections: limits, unique IDs, each definition, and every block a workflow names.
pub fn validate(workflows: &[Workflow], blocks: &[Block]) -> Result<()> {
    ensure!(
        workflows.len() <= MAX_WORKFLOWS,
        "At most {MAX_WORKFLOWS} workflows are allowed."
    );
    ensure!(
        blocks.len() <= MAX_BLOCKS,
        "At most {MAX_BLOCKS} workflow blocks are allowed."
    );
    let mut ids = BTreeSet::new();
    for block in blocks {
        validate_block(block)?;
        ensure!(
            ids.insert(block.id.as_str()),
            "Duplicate workflow block ID {}.",
            block.id
        );
    }
    let mut workflow_ids = BTreeSet::new();
    for workflow in workflows {
        validate_workflow(workflow)?;
        ensure!(
            workflow_ids.insert(workflow.id.as_str()),
            "Duplicate workflow ID {}.",
            workflow.id
        );
        for block in &workflow.blocks {
            ensure!(
                ids.contains(block.as_str()),
                "Workflow {} names block {block}, which does not exist.",
                workflow.id
            );
        }
    }
    Ok(())
}

/// The workflows that name a block.
pub fn using<'a>(workflows: &'a [Workflow], block: &str) -> Vec<&'a str> {
    workflows
        .iter()
        .filter(|w| w.blocks.iter().any(|b| b == block))
        .map(|w| w.id.as_str())
        .collect()
}

fn decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((high * 16 + low) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The organization, project and repository an Azure Repos remote URL names (lowercased), in
/// any of its spellings: `https://[user@]dev.azure.com/ORG/PROJECT/_git/REPO`,
/// `https://ORG.visualstudio.com/[DefaultCollection/]PROJECT/_git/REPO`,
/// `git@ssh.dev.azure.com:v3/ORG/PROJECT/REPO` and `ORG@vs-ssh.visualstudio.com:v3/ORG/PROJECT/REPO`
/// (also as `ssh://` URLs). A repository named like its project may leave the project out.
pub fn azure_remote(remote: &str) -> Option<(String, String, String)> {
    let remote = remote.trim();
    let lower = |s: &str| decode(s).to_lowercase();
    let strip = |s: &str| s.strip_suffix(".git").unwrap_or(s).to_owned();
    // The scp-like SSH form has no scheme.
    if !remote.contains("://")
        && let Some((host, path)) = remote.split_once(':')
    {
        let host = host.rsplit('@').next()?;
        if !matches!(host, "ssh.dev.azure.com" | "vs-ssh.visualstudio.com") {
            return None;
        }
        return ssh_path(path).map(|(o, p, r)| (lower(o), lower(p), lower(&strip(r))));
    }
    let url = url::Url::parse(remote).ok()?;
    let host = url.host_str()?.to_lowercase();
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    match url.scheme() {
        "ssh"
            if matches!(
                host.as_str(),
                "ssh.dev.azure.com" | "vs-ssh.visualstudio.com"
            ) =>
        {
            ssh_path(&segments.join("/")).map(|(o, p, r)| (lower(o), lower(p), lower(&strip(r))))
        }
        "https" | "http" => {
            let (organization, rest): (String, &[&str]) = if host == "dev.azure.com" {
                (lower(segments.first()?), &segments[1..])
            } else {
                let organization = host.strip_suffix(".visualstudio.com")?;
                let rest = match segments.first() {
                    Some(first) if first.eq_ignore_ascii_case("DefaultCollection") => {
                        &segments[1..]
                    }
                    _ => &segments[..],
                };
                (organization.to_owned(), rest)
            };
            match rest {
                [project, "_git", repository] => {
                    Some((organization, lower(project), lower(&strip(repository))))
                }
                ["_git", repository] => {
                    let repository = lower(&strip(repository));
                    Some((organization, repository.clone(), repository))
                }
                _ => None,
            }
        }
        _ => None,
    }
}
fn ssh_path(path: &str) -> Option<(&str, &str, &str)> {
    match path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["v3", organization, project, repository] => Some((organization, project, repository)),
        _ => None,
    }
}

/// A repository of a project's Git catalog (`workflow_repositories`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct CatalogRepository {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub default_branch: String,
    #[serde(default)]
    pub remote_url: String,
    #[serde(default)]
    pub ssh_url: String,
    #[serde(default)]
    pub web_url: String,
}

/// Whether a checkout's `origin` names this repository of the workflow's project.
pub fn origin_names(
    origin: &str,
    organization: &str,
    project: &str,
    repository: &CatalogRepository,
) -> bool {
    let same = |a: &str, b: &str| !a.is_empty() && a.trim().eq_ignore_ascii_case(b.trim());
    if same(origin, &repository.remote_url) || same(origin, &repository.ssh_url) {
        return true;
    }
    let (Some(named), Some(organization)) = (azure_remote(origin), organization_name(organization))
    else {
        return false;
    };
    named
        == (
            organization,
            project.to_lowercase(),
            repository.name.to_lowercase(),
        )
}

/// What the execution machine said about one checkout directory (`checkout_inspect`).
#[derive(Clone, Debug, PartialEq)]
pub enum Inspection {
    /// The machine answered: the canonical directory, whether the path given goes through a
    /// symbolic link below its root, whether it is a Git checkout, and its `origin` when it has
    /// a readable one.
    Answered {
        directory: PathBuf,
        link: bool,
        checkout: bool,
        origin: Option<String>,
    },
    /// The directory does not exist on the machine or is not a directory.
    Missing,
    /// The directory is not under the machine's allowed roots, or resolves outside them.
    Outside,
    /// The machine could not answer (offline, or a version without `checkout_inspect`).
    Unknown(String),
}

/// What saving a workflow is checked against, gathered by the host.
#[derive(Clone, Debug)]
pub struct Facts {
    /// The project is one of the enabled Azure source's configured organizations.
    pub project_known: bool,
    pub catalog: Vec<CatalogRepository>,
    /// The operating system the machine reported (`linux`, `macos`, `windows`): how it spells
    /// directories, whatever the coordinator's own system is.
    pub os: String,
    /// The machine's allowed roots, as it last reported them (checked when it cannot answer).
    pub roots: Vec<PathBuf>,
    /// The machine's installed CLIs (`implementation_catalog`), or why they are unknown.
    pub agents: std::result::Result<serde_json::Value, String>,
    /// Each repository ID's (lowercase) inspection.
    pub checkouts: BTreeMap<String, Inspection>,
}

/// Check a workflow against what the host found; the warnings are what could not be verified
/// or what was adjusted. A checkout the machine spells differently (letter case on a
/// case-insensitive file system) is saved as the machine's canonical directory.
pub fn check(workflow: &mut Workflow, facts: &Facts) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    ensure!(
        facts.project_known,
        "Project {} is not in the configured organizations of the enabled Azure source.",
        workflow.project
    );
    let repositories = workflow.repositories.clone();
    for repository in &repositories {
        let Some(known) = facts
            .catalog
            .iter()
            .find(|r| r.id.eq_ignore_ascii_case(&repository.id))
        else {
            bail!(
                "Repository {} ({}) is not in the Git catalog of project {}.",
                repository.name,
                repository.id,
                workflow.project
            );
        };
        ensure!(
            known.name == repository.name,
            "Repository {} is named {} in the project's catalog; save it with that name.",
            repository.id,
            known.name
        );
        let key = repository.id.to_lowercase();
        let directory = workflow
            .checkouts
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(&repository.id))
            .map(|(_, d)| d.clone())
            .ok_or_else(|| {
                anyhow::anyhow!("Repository {} needs its checkout directory.", known.name)
            })?;
        let text = directory.to_string_lossy().into_owned();
        ensure!(
            absolute_on(&text, &facts.os),
            "{text} is not an absolute directory on this {} machine.",
            facts.os
        );
        match facts.checkouts.get(&key) {
            Some(Inspection::Answered {
                directory: canonical,
                link,
                checkout,
                origin,
            }) => {
                ensure!(
                    !link,
                    "{text} goes through a symbolic link (it names {}); save the canonical directory.",
                    canonical.display()
                );
                ensure!(*checkout, "{text} is not a Git checkout.");
                match origin {
                    Some(origin) => ensure!(
                        origin_names(origin, &workflow.organization, &workflow.project, known),
                        "The origin of {text} ({origin}) names another repository than {}.",
                        known.name
                    ),
                    None => warnings.push(format!(
                        "{text} has no readable origin remote, so it could not be verified as a checkout of {}.",
                        known.name
                    )),
                }
                if *canonical != directory {
                    warnings.push(format!(
                        "{text} is saved as the machine spells it: {}.",
                        canonical.display()
                    ));
                    workflow.checkouts.remove(&key);
                    workflow.checkouts.insert(key, canonical.clone());
                }
            }
            Some(Inspection::Missing) => {
                bail!("{text} does not exist on the machine or is not a directory.")
            }
            Some(Inspection::Outside) => bail!(
                "{text} is outside the machine's allowed roots (a symbolic link cannot leave them)."
            ),
            unknown => {
                let reason = match unknown {
                    Some(Inspection::Unknown(reason)) => reason.as_str(),
                    _ => "the machine did not inspect it",
                };
                ensure!(
                    facts
                        .roots
                        .iter()
                        .any(|root| under(&text, &root.to_string_lossy(), &facts.os)),
                    "{text} is outside the machine's allowed roots."
                );
                warnings.push(format!(
                    "{text} could not be verified as a checkout of {} ({reason}); the machine checks it again before a run.",
                    known.name
                ));
            }
        }
    }
    let agent = &workflow.agent;
    match &facts.agents {
        Ok(catalog) => agent_installed(catalog, agent, "the workflow's execution machine")?,
        Err(reason) => warnings.push(format!(
            "The coding agent could not be verified on the machine ({reason}); it is checked again before a run."
        )),
    }
    Ok(warnings)
}

/// Whether a machine's installed CLIs (`implementation_catalog`) include the agent's provider
/// with that model and effort; `place` names the machine in the refusal.
pub fn agent_installed(catalog: &serde_json::Value, agent: &Agent, place: &str) -> Result<()> {
    let provider = catalog["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["id"] == agent.provider.as_str() && p["available"] != false);
    let Some(provider) = provider else {
        bail!("{} is not installed on {place}.", agent.provider);
    };
    ensure!(
        provider["models"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|m| {
                m["id"] == agent.model.as_str()
                    && m["efforts"]
                        .as_array()
                        .is_some_and(|e| e.iter().any(|e| e == agent.effort.as_str()))
            }),
        "{} on {place} has no model {} with effort {}.",
        agent.provider,
        agent.model,
        agent.effort
    );
    Ok(())
}

/// The components of an absolute directory as a machine of that spelling writes it, or none
/// when it is not absolute there or holds `.` or `..`. Text-based, so the coordinator reads a
/// Windows machine's directories the same on any system: `C:\…` or `C:/…`, `\\server\share\…`
/// and the verbatim `\\?\` forms on Windows; `/…` elsewhere.
fn components(path: &str, windows: bool) -> Option<Vec<String>> {
    let parts: Vec<String> = if windows {
        let path = path.replace('/', "\\");
        let path = match path.strip_prefix(r"\\?\UNC\") {
            Some(rest) => format!(r"\\{rest}"),
            None => path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned(),
        };
        let bytes = path.as_bytes();
        let mut parts = Vec::new();
        let rest = if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            parts.push(path[..2].to_owned());
            &path[3..]
        } else {
            let mut segments = path.strip_prefix(r"\\")?.splitn(3, '\\');
            let (server, share) = (segments.next()?, segments.next()?);
            if server.is_empty() || share.is_empty() {
                return None;
            }
            parts.push(format!(r"\\{server}\{share}"));
            segments.next().unwrap_or("")
        };
        parts.extend(
            rest.split('\\')
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        );
        parts
    } else {
        let rest = path.strip_prefix('/')?;
        rest.split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    (!parts.iter().any(|p| p == "." || p == "..") && !path.chars().any(char::is_control))
        .then_some(parts)
}
fn windows(os: &str) -> bool {
    os.eq_ignore_ascii_case("windows")
}
/// Whether the file systems of that system compare names without letter case by default.
fn case_insensitive(os: &str) -> bool {
    windows(os) || os.eq_ignore_ascii_case("macos")
}

/// Whether a directory is absolute in some machine's spelling (Unix or Windows), as the map
/// checks it without knowing the machine.
pub fn absolute(path: &str) -> bool {
    path.len() <= 4096 && (components(path, false).is_some() || components(path, true).is_some())
}
/// Whether a directory is absolute on a machine of that operating system.
pub fn absolute_on(path: &str, os: &str) -> bool {
    path.len() <= 4096 && components(path, windows(os)).is_some()
}
/// Whether a directory is a root or below it on a machine of that operating system, component
/// by component and without reading the file system (no link is resolved).
pub fn under(path: &str, root: &str, os: &str) -> bool {
    let (Some(path), Some(root)) = (components(path, windows(os)), components(root, windows(os)))
    else {
        return false;
    };
    let same = |a: &String, b: &String| {
        if case_insensitive(os) {
            a.to_lowercase() == b.to_lowercase()
        } else {
            a == b
        }
    };
    path.len() >= root.len() && root.iter().zip(&path).all(|(r, p)| same(r, p))
}
/// The same directory written twice, as a machine of any system could see it.
fn same_directory(a: &str, b: &str) -> bool {
    let key = |p: &str| {
        components(p, false)
            .or_else(|| components(p, true))
            .map(|c| c.join("/").to_lowercase())
    };
    key(a).is_some() && key(a) == key(b)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const REPO: &str = "0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f";
    const OTHER: &str = "7d9e1f20-3a4b-4c5d-8e6f-0a1b2c3d4e5f";
    const MACHINE: &str = "5a6b7c8d-9e0f-4a1b-8c2d-3e4f5a6b7c8d";

    pub(crate) fn workflow(id: &str) -> Workflow {
        Workflow {
            id: id.into(),
            name: "Payments".into(),
            description: "HUs of the payments service".into(),
            enabled: true,
            organization: "https://dev.azure.com/example".into(),
            project: "Payments".into(),
            repositories: vec![Repository {
                id: REPO.into(),
                name: "payments-api".into(),
            }],
            machine: MACHINE.into(),
            checkouts: BTreeMap::from([(REPO.into(), PathBuf::from("/work/payments-api"))]),
            agent: Agent {
                provider: "codex".into(),
                model: "gpt-6.1-sol".into(),
                effort: "medium".into(),
            },
            instructions: "Start from feature/222.".into(),
            ..Default::default()
        }
    }
    pub(crate) fn instruction(id: &str) -> Block {
        Block {
            id: id.into(),
            name: "Base branch".into(),
            kind: Kind::Instruction,
            text: "Branch from feature/222.".into(),
            ..Default::default()
        }
    }
    fn notification(id: &str) -> Block {
        Block {
            id: id.into(),
            name: "Tell Victoria".into(),
            kind: Kind::TeamsNotification,
            stage: Some(Stage::Done),
            chat: Some(Chat {
                id: "19:synthetic@thread.v2".into(),
                name: "Releases".into(),
            }),
            mentions: vec!["Victoria".into()],
            template: "{item} is released: {link} ({pull_requests})".into(),
            ..Default::default()
        }
    }
    fn launcher(id: &str) -> Block {
        Block {
            id: id.into(),
            name: "Release in desa".into(),
            kind: Kind::Launcher,
            stage: Some(Stage::PullRequestsCompleted),
            launcher: Some(LauncherRef {
                id: "release-desa".into(),
                digest: "a".repeat(64),
            }),
            ..Default::default()
        }
    }
    fn catalog() -> Vec<CatalogRepository> {
        vec![CatalogRepository {
            id: REPO.into(),
            name: "payments-api".into(),
            remote_url: "https://example@dev.azure.com/example/Payments/_git/payments-api".into(),
            ssh_url: "git@ssh.dev.azure.com:v3/example/Payments/payments-api".into(),
            ..Default::default()
        }]
    }
    fn agents() -> serde_json::Value {
        serde_json::json!({"providers":[{"id":"codex","available":true,"models":[{"id":"gpt-6.1-sol","efforts":["low","medium"]}]}]})
    }
    fn facts(inspection: Inspection) -> Facts {
        Facts {
            project_known: true,
            catalog: catalog(),
            os: "linux".into(),
            roots: vec![PathBuf::from("/work")],
            agents: Ok(agents()),
            checkouts: BTreeMap::from([(REPO.to_lowercase(), inspection)]),
        }
    }
    fn answered(origin: Option<&str>) -> Inspection {
        Inspection::Answered {
            directory: PathBuf::from("/work/payments-api"),
            link: false,
            checkout: true,
            origin: origin.map(str::to_owned),
        }
    }
    fn checked(workflow: &Workflow, facts: &Facts) -> Result<Vec<String>> {
        check(&mut workflow.clone(), facts)
    }

    #[test]
    fn the_sections_round_trip_and_are_left_out_while_empty() {
        let mut map = crate::knowledge::KnowledgeMap::parse("").unwrap();
        let empty = toml::to_string(&map).unwrap();
        assert!(!empty.contains("workflow"));
        map.workflow_blocks = vec![
            instruction("base"),
            notification("notify"),
            launcher("desa"),
        ];
        let mut w = workflow("payments");
        w.blocks = vec!["base".into(), "notify".into(), "desa".into()];
        map.workflows = vec![w];
        let text = toml::to_string(&map).unwrap();
        let back = crate::knowledge::KnowledgeMap::parse(&text).unwrap();
        assert_eq!(back.workflows, map.workflows);
        assert_eq!(back.workflow_blocks, map.workflow_blocks);
        assert!(text.contains("[[workflows]]") && text.contains("[[workflow_blocks]]"));
        // A map without them parses as before and writes nothing new.
        let legacy = crate::knowledge::KnowledgeMap::parse("[[resources]]\nid = \"wiki\"\ndescription = \"Wiki\"\ntopics = []\nenabled = false\nexternal_processing = false\nallowed_conversations = []\nkind = \"url\"\nurl = \"https://example.com\"\n").unwrap();
        assert!(legacy.workflows.is_empty() && legacy.workflow_blocks.is_empty());
        assert!(!toml::to_string(&legacy).unwrap().contains("workflow"));
    }

    #[test]
    fn limits_and_references_are_enforced() {
        let blocks = vec![instruction("base")];
        let mut w = workflow("payments");
        assert!(validate(std::slice::from_ref(&w), &blocks).is_ok());
        w.instructions = "x".repeat(MAX_INSTRUCTIONS + 1);
        assert!(validate(std::slice::from_ref(&w), &blocks).is_err());
        w.instructions = "x".repeat(MAX_INSTRUCTIONS);
        assert!(validate(std::slice::from_ref(&w), &blocks).is_ok());
        w.blocks = vec!["missing".into()];
        let error = validate(std::slice::from_ref(&w), &blocks)
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing"), "{error}");
        w.blocks = (0..=MAX_WORKFLOW_BLOCKS).map(|n| format!("b{n}")).collect();
        let many: Vec<Block> = w.blocks.iter().map(|b| instruction(b)).collect();
        assert!(validate(std::slice::from_ref(&w), &many).is_err());
        w.blocks.pop();
        assert!(validate(std::slice::from_ref(&w), &many).is_ok());
        w.blocks.clear();
        w.repositories = (0..=MAX_REPOSITORIES)
            .map(|_| Repository {
                id: uuid::Uuid::new_v4().to_string(),
                name: "repo".into(),
            })
            .collect();
        w.checkouts = w
            .repositories
            .iter()
            .enumerate()
            .map(|(n, r)| (r.id.clone(), PathBuf::from(format!("/work/{n}"))))
            .collect();
        assert!(validate_workflow(&w).is_err());
        w.repositories.pop();
        w.checkouts = w
            .repositories
            .iter()
            .enumerate()
            .map(|(n, r)| (r.id.clone(), PathBuf::from(format!("/work/{n}"))))
            .collect();
        assert!(validate_workflow(&w).is_ok());
        let too_many: Vec<Workflow> = (0..=MAX_WORKFLOWS)
            .map(|n| workflow(&format!("w{n}")))
            .collect();
        assert!(validate(&too_many, &[]).is_err());
        assert!(validate(&too_many[..MAX_WORKFLOWS], &[]).is_ok());
        let too_many: Vec<Block> = (0..=MAX_BLOCKS)
            .map(|n| instruction(&format!("b{n}")))
            .collect();
        assert!(validate(&[], &too_many).is_err());
        assert!(validate(&[], &too_many[..MAX_BLOCKS]).is_ok());
        // A checkout of a repository the workflow does not list, a missing one, a relative one.
        let mut w = workflow("payments");
        w.checkouts.insert(OTHER.into(), "/work/other".into());
        assert!(validate_workflow(&w).is_err());
        let mut w = workflow("payments");
        w.checkouts.clear();
        assert!(validate_workflow(&w).is_err());
        let mut w = workflow("payments");
        w.checkouts.insert(REPO.into(), "relative".into());
        assert!(validate_workflow(&w).is_err());
        let mut w = workflow("payments");
        w.agent.provider = "deepseek".into();
        assert!(validate_workflow(&w).is_err());
        let mut w = workflow("payments");
        w.organization = "https://evil.example/example".into();
        assert!(validate_workflow(&w).is_err());
        assert!(validate(&[workflow("same"), workflow("same")], &[]).is_err());
    }

    #[test]
    fn each_block_kind_holds_only_its_own_fields() {
        for block in [instruction("a"), notification("b"), launcher("c")] {
            assert!(validate_block(&block).is_ok(), "{block:?}");
        }
        let mut b = instruction("a");
        b.stage = Some(Stage::Done);
        assert!(validate_block(&b).is_err());
        let mut b = notification("b");
        b.stage = None;
        assert!(validate_block(&b).is_err());
        let mut b = notification("b");
        b.template = "Hello {someone}".into();
        assert!(
            validate_block(&b)
                .unwrap_err()
                .to_string()
                .contains("someone")
        );
        let mut b = notification("b");
        b.chat.as_mut().unwrap().id = "../../me/sendMail".into();
        assert!(validate_block(&b).is_err());
        let mut b = notification("b");
        b.launcher = launcher("c").launcher;
        assert!(validate_block(&b).is_err());
        let mut b = launcher("c");
        b.launcher.as_mut().unwrap().digest = "short".into();
        assert!(validate_block(&b).is_err());
        let mut b = launcher("c");
        b.text = "run".into();
        assert!(validate_block(&b).is_err());
        assert!(validate_block(&instruction("Bad ID")).is_err());
        // In use: a block named by a workflow.
        let mut w = workflow("payments");
        w.blocks = vec!["a".into()];
        assert_eq!(using(&[w], "a"), ["payments"]);
    }

    #[test]
    fn directories_are_read_as_the_machine_spells_them() {
        // A Windows machine's directories are absolute and under its roots on any coordinator.
        assert!(absolute(r"C:\Work\payments-api") && absolute("C:/Work/x") && absolute("/srv/x"));
        assert!(absolute(r"\\server\share\x") && !absolute("relative") && !absolute("C:"));
        assert!(absolute_on(r"C:\Work\x", "windows") && !absolute_on(r"C:\Work\x", "linux"));
        assert!(absolute_on("/srv/x", "macos") && !absolute_on("/srv/x", "windows"));
        assert!(!absolute(r"C:\Work\..\Windows") && !absolute("/work/../etc"));
        assert!(under(r"C:\Work\payments-api", r"C:\Work", "windows"));
        assert!(under(r"c:/work/Payments-API", r"C:\Work", "windows"));
        assert!(under(r"\\?\C:\Work\x", r"C:\Work", "windows"));
        assert!(!under(r"C:\Workshop\x", r"C:\Work", "windows"));
        assert!(under("/Users/Ana/Work/x", "/users/ana/work", "macos"));
        assert!(!under("/Users/Ana/Work/x", "/users/ana/work", "linux"));
        assert!(!under("/work2/x", "/work", "linux") && under("/work", "/work", "linux"));
        // The check on a Linux coordinator for a Windows machine that cannot answer.
        let mut w = workflow("payments");
        w.checkouts
            .insert(REPO.into(), PathBuf::from(r"C:\Work\payments-api"));
        assert!(validate_workflow(&w).is_ok());
        let mut f = facts(Inspection::Unknown("offline".into()));
        f.os = "windows".into();
        f.roots = vec![PathBuf::from(r"C:\Work")];
        assert_eq!(checked(&w, &f).unwrap().len(), 1);
        f.roots = vec![PathBuf::from(r"D:\Other")];
        assert!(checked(&w, &f).is_err());
        // A Unix directory is not absolute on a Windows machine, nor the reverse.
        f.roots = vec![PathBuf::from(r"C:\Work")];
        assert!(
            checked(&workflow("payments"), &f)
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        let linux = facts(Inspection::Unknown("offline".into()));
        assert!(checked(&w, &linux).is_err());
    }

    #[test]
    fn checkouts_are_keyed_once_per_repository_whatever_the_letter_case() {
        let mut w = workflow("payments");
        w.checkouts
            .insert(REPO.to_uppercase(), PathBuf::from("/work/other"));
        let error = validate_workflow(&w).unwrap_err().to_string();
        assert!(error.contains("two checkouts"), "{error}");
        // The same directory written twice is refused too, as a case-insensitive machine sees it.
        let mut w = workflow("payments");
        w.repositories.push(Repository {
            id: OTHER.into(),
            name: "payments-web".into(),
        });
        w.checkouts
            .insert(OTHER.into(), "/WORK/payments-api".into());
        assert!(validate_workflow(&w).is_err());
    }

    #[test]
    fn a_checkout_is_saved_as_the_machine_spells_it() {
        let origin = "git@ssh.dev.azure.com:v3/example/Payments/payments-api";
        let mut w = workflow("payments");
        w.checkouts
            .insert(REPO.into(), PathBuf::from("/Work/Payments-API"));
        let mut f = facts(answered(Some(origin)));
        f.os = "macos".into();
        let warnings = check(&mut w, &f).unwrap();
        assert_eq!(w.checkouts[REPO], PathBuf::from("/work/payments-api"));
        assert!(
            warnings[0].contains("as the machine spells it"),
            "{warnings:?}"
        );
    }

    #[test]
    fn azure_remotes_are_read_in_every_spelling() {
        let expected = Some(("example".into(), "payments".into(), "payments-api".into()));
        for remote in [
            "https://example@dev.azure.com/example/Payments/_git/payments-api",
            "https://dev.azure.com/example/Payments/_git/payments-api.git",
            "https://example.visualstudio.com/Payments/_git/payments-api",
            "https://example.visualstudio.com/DefaultCollection/Payments/_git/payments-api",
            "git@ssh.dev.azure.com:v3/example/Payments/payments-api",
            "example@vs-ssh.visualstudio.com:v3/example/Payments/payments-api",
            "ssh://git@ssh.dev.azure.com/v3/example/Payments/payments-api",
        ] {
            assert_eq!(azure_remote(remote), expected, "{remote}");
        }
        assert_eq!(
            azure_remote("https://dev.azure.com/example/My%20Project/_git/api"),
            Some(("example".into(), "my project".into(), "api".into()))
        );
        assert_eq!(
            azure_remote("https://dev.azure.com/example/_git/Payments"),
            Some(("example".into(), "payments".into(), "payments".into()))
        );
        assert_eq!(
            azure_remote("git@github.com:example/payments-api.git"),
            None
        );
        assert_eq!(azure_remote("/srv/mirror/payments-api"), None);
    }

    #[test]
    fn saving_refuses_what_is_wrong_and_warns_about_what_cannot_be_verified() {
        let w = workflow("payments");
        let origin = "git@ssh.dev.azure.com:v3/example/Payments/payments-api";
        assert_eq!(
            checked(&w, &facts(answered(Some(origin)))).unwrap(),
            Vec::<String>::new()
        );
        // The project outside the source's organizations.
        let mut f = facts(answered(Some(origin)));
        f.project_known = false;
        assert!(checked(&w, &f).unwrap_err().to_string().contains("Project"));
        // An unknown repository.
        let mut f = facts(answered(Some(origin)));
        f.catalog[0].id = OTHER.into();
        assert!(
            checked(&w, &f)
                .unwrap_err()
                .to_string()
                .contains("not in the Git catalog")
        );
        // A checkout outside the roots, or one that resolves outside them.
        let error = checked(&w, &facts(Inspection::Outside))
            .unwrap_err()
            .to_string();
        assert!(error.contains("outside"), "{error}");
        let error = checked(&w, &facts(Inspection::Missing))
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not exist"), "{error}");
        // A path through a symbolic link below the root.
        let alias = Inspection::Answered {
            directory: PathBuf::from("/work/real/payments-api"),
            link: true,
            checkout: true,
            origin: Some(origin.into()),
        };
        let error = checked(&w, &facts(alias)).unwrap_err().to_string();
        assert!(error.contains("symbolic link"), "{error}");
        // Not a checkout.
        let plain = Inspection::Answered {
            directory: PathBuf::from("/work/payments-api"),
            link: false,
            checkout: false,
            origin: None,
        };
        assert!(
            checked(&w, &facts(plain))
                .unwrap_err()
                .to_string()
                .contains("not a Git checkout")
        );
        // Another repository's origin is refused; no origin is a warning.
        let error = checked(
            &w,
            &facts(answered(Some(
                "https://dev.azure.com/example/Payments/_git/payments-web",
            ))),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("another repository"), "{error}");
        assert!(
            checked(
                &w,
                &facts(answered(Some("git@github.com:example/payments-api.git")))
            )
            .is_err()
        );
        let warnings = checked(&w, &facts(answered(None))).unwrap();
        assert_eq!(warnings.len(), 1);
        // An older or offline machine: inside its last roots warns, outside refuses.
        let warnings = checked(&w, &facts(Inspection::Unknown("an older version".into()))).unwrap();
        assert!(warnings[0].contains("an older version"), "{warnings:?}");
        let mut f = facts(Inspection::Unknown("offline".into()));
        f.roots = vec![PathBuf::from("/elsewhere")];
        assert!(checked(&w, &f).is_err());
        let mut outside = workflow("payments");
        outside.checkouts.insert(REPO.into(), "/work/../etc".into());
        assert!(checked(&outside, &facts(Inspection::Unknown("offline".into()))).is_err());
        // An agent the machine does not have, a model or effort it does not offer.
        let mut w = workflow("payments");
        w.agent.provider = "claude".into();
        assert!(
            checked(&w, &facts(answered(Some(origin))))
                .unwrap_err()
                .to_string()
                .contains("not installed")
        );
        let mut w = workflow("payments");
        w.agent.effort = "xhigh".into();
        assert!(checked(&w, &facts(answered(Some(origin)))).is_err());
        let mut f = facts(answered(Some(origin)));
        f.agents = Err("the machine is offline".into());
        assert_eq!(checked(&workflow("payments"), &f).unwrap().len(), 1);
        // A name that is not the catalog's.
        let mut w = workflow("payments");
        w.repositories[0].name = "renamed".into();
        assert!(checked(&w, &facts(answered(Some(origin)))).is_err());
    }
}
