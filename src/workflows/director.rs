//! The workflow director's settings and what code decides around it. `[workflow_settings]` in
//! the map holds the ready states of each project, the watcher's interval, how many workflow
//! runs advance at once, how long a run waits for its pull requests and the director (the
//! installed CLI, model and effort on one machine of the account). The section is left out
//! while every value is its default, so a map without it loads with the defaults and is written
//! without it until a value changes. (The map's `[[workflows]]` key already names the
//! definitions, so the settings cannot be a `[workflows]` table.)
//!
//! Which director classifies and summarizes the items of a project is resolved by `resolve`: the
//! project's own director (`project_directors`) when it has one; otherwise the machine and agent
//! of the project's workflows when they all share them (the enabled ones that start by
//! themselves, or every enabled one when none does), so the director runs where the workflow
//! runs without being configured apart; otherwise the account's director; otherwise none.
//!
//! The director chooses a workflow for a work item and decides nothing else: code sends it the
//! enabled workflows of the item's project (`candidates`) and checks its answer (`judge`): the
//! ID must be one it sent, and still exist, be enabled and belong to the item's organization
//! and project. Anything else is no workflow, with the reason.

use super::{Agent, PROVIDERS, Workflow, organization_name};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const DEFAULT_WATCH_INTERVAL_MINUTES: u32 = 5;
pub const WATCH_INTERVAL_MINUTES: std::ops::RangeInclusive<u32> = 1..=60;
pub const DEFAULT_CONCURRENCY: u32 = 2;
pub const CONCURRENCY: std::ops::RangeInclusive<u32> = 1..=4;
pub const DEFAULT_PULL_REQUEST_WAIT_DAYS: u32 = 7;
pub const PULL_REQUEST_WAIT_DAYS: std::ops::RangeInclusive<u32> = 1..=60;
/// At most this many projects have ready states, with at most this many states each.
pub const MAX_READY_PROJECTS: usize = 50;
pub const MAX_READY_STATES: usize = 20;
/// The director's reason is one line of at most this many characters.
pub const MAX_REASON: usize = 300;

/// `[workflow_settings]`: what the watcher, the run engine and the director read.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Minutes between two looks for items in a ready state (1 to 60).
    pub watch_interval_minutes: u32,
    /// Workflow runs that advance at once (1 to 4).
    pub concurrency: u32,
    /// Days a run waits for its pull requests to be completed or abandoned (1 to 60).
    pub pull_request_wait_days: u32,
    /// The model-backed selector and summarizer; none until the user chooses it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub director: Option<Director>,
    /// The states that mean «ready to develop», per organization and project.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ready_states: Vec<ReadyStates>,
    /// The director of one project, before its workflows' agent and the account's director.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub project_directors: Vec<ProjectDirector>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            watch_interval_minutes: DEFAULT_WATCH_INTERVAL_MINUTES,
            concurrency: DEFAULT_CONCURRENCY,
            pull_request_wait_days: DEFAULT_PULL_REQUEST_WAIT_DAYS,
            director: None,
            ready_states: Vec::new(),
            project_directors: Vec::new(),
        }
    }
}
impl Settings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The workflow states of one project that mean «ready to develop», chosen from the states its
/// process has for the requirement, bug and listed task types.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReadyStates {
    /// `https://dev.azure.com/NAME`.
    pub organization: String,
    pub project: String,
    pub states: Vec<String>,
}

/// The director of one project: it classifies and summarizes that project's items.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectDirector {
    /// `https://dev.azure.com/NAME`.
    pub organization: String,
    pub project: String,
    pub director: Director,
}

/// Where the director of a project comes from.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The project's own director (`project_directors`).
    Project,
    /// The machine and agent its workflows share.
    Workflow,
    /// The account's director (`director`).
    Global,
}
impl Source {
    pub fn word(self) -> &'static str {
        match self {
            Source::Project => "project",
            Source::Workflow => "workflow",
            Source::Global => "global",
        }
    }
}

/// The director that classifies and summarizes a project's items, and where it comes from.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub director: Director,
    pub source: Source,
}

/// The workflows whose machine and agent the director takes by default: the enabled ones of the
/// project that start by themselves, or every enabled one when none does.
pub fn defaults<'a>(
    workflows: &'a [Workflow],
    organization: &str,
    project: &str,
) -> Vec<&'a Workflow> {
    let enabled = candidates(workflows, organization, project);
    let automatic: Vec<&Workflow> = enabled.iter().copied().filter(|w| w.auto_start).collect();
    if automatic.is_empty() {
        enabled
    } else {
        automatic
    }
}

/// The director of a project (its organization's lowercased name): its own; else the machine
/// and agent all its default workflows share; else the account's; else none.
pub fn resolve(
    settings: &Settings,
    workflows: &[Workflow],
    organization: &str,
    project: &str,
) -> Option<Resolved> {
    if let Some(own) = settings.project_directors.iter().find(|p| {
        organization_name(&p.organization).as_deref() == Some(organization)
            && p.project.eq_ignore_ascii_case(project)
    }) {
        return Some(Resolved {
            director: own.director.clone(),
            source: Source::Project,
        });
    }
    let shared = defaults(workflows, organization, project);
    if let Some(first) = shared.first()
        && shared
            .iter()
            .all(|w| w.machine == first.machine && w.agent == first.agent)
    {
        return Some(Resolved {
            director: Director {
                machine: first.machine.clone(),
                provider: first.agent.provider.clone(),
                model: first.agent.model.clone(),
                effort: first.agent.effort.clone(),
            },
            source: Source::Workflow,
        });
    }
    settings.director.clone().map(|director| Resolved {
        director,
        source: Source::Global,
    })
}

/// The workflow director: an installed CLI, model and effort on one machine of the account.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Director {
    /// The machine's ID (the coordinator's own ID for the coordinator).
    pub machine: String,
    /// `codex`, `claude` or `opencode`.
    pub provider: String,
    pub model: String,
    pub effort: String,
}
impl Director {
    pub fn agent(&self) -> Agent {
        Agent {
            provider: self.provider.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
        }
    }
}

fn line(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// The settings alone: bounds, the ready states' shape and the director's fields.
pub fn validate(settings: &Settings) -> Result<()> {
    ensure!(
        WATCH_INTERVAL_MINUTES.contains(&settings.watch_interval_minutes),
        "watch_interval_minutes is between {} and {} minutes.",
        WATCH_INTERVAL_MINUTES.start(),
        WATCH_INTERVAL_MINUTES.end()
    );
    ensure!(
        CONCURRENCY.contains(&settings.concurrency),
        "concurrency is between {} and {} workflow runs.",
        CONCURRENCY.start(),
        CONCURRENCY.end()
    );
    ensure!(
        PULL_REQUEST_WAIT_DAYS.contains(&settings.pull_request_wait_days),
        "pull_request_wait_days is between {} and {} days.",
        PULL_REQUEST_WAIT_DAYS.start(),
        PULL_REQUEST_WAIT_DAYS.end()
    );
    ensure!(
        settings.ready_states.len() <= MAX_READY_PROJECTS,
        "At most {MAX_READY_PROJECTS} projects have ready states."
    );
    let mut projects = BTreeSet::new();
    for ready in &settings.ready_states {
        let organization = organization_name(&ready.organization).ok_or_else(|| {
            anyhow::anyhow!(
                "The ready states of {} need its organization URL (https://dev.azure.com/NAME).",
                ready.project
            )
        })?;
        ensure!(
            line(&ready.project, 120),
            "Ready states name their Azure DevOps project (at most 120 characters)."
        );
        ensure!(
            projects.insert((organization, ready.project.to_lowercase())),
            "Project {} has its ready states once.",
            ready.project
        );
        ensure!(
            (1..=MAX_READY_STATES).contains(&ready.states.len())
                && ready.states.iter().all(|s| line(s, 120))
                && ready.states.iter().collect::<BTreeSet<_>>().len() == ready.states.len(),
            "Project {} has between 1 and {MAX_READY_STATES} different ready states (at most 120 characters each).",
            ready.project
        );
    }
    if let Some(director) = &settings.director {
        validate_director(director, "The workflow director")?;
    }
    ensure!(
        settings.project_directors.len() <= MAX_READY_PROJECTS,
        "At most {MAX_READY_PROJECTS} projects have their own director."
    );
    let mut directed = BTreeSet::new();
    for own in &settings.project_directors {
        let organization = organization_name(&own.organization).ok_or_else(|| {
            anyhow::anyhow!(
                "The director of {} needs its organization URL (https://dev.azure.com/NAME).",
                own.project
            )
        })?;
        ensure!(
            line(&own.project, 120),
            "A project's director names its Azure DevOps project (at most 120 characters)."
        );
        ensure!(
            directed.insert((organization, own.project.to_lowercase())),
            "Project {} has its own director once.",
            own.project
        );
        validate_director(&own.director, &format!("The director of {}", own.project))?;
    }
    Ok(())
}

/// A director's fields: its machine's ID and an agent of a known provider.
fn validate_director(director: &Director, whose: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(&director.machine).is_ok() && director.machine.len() == 36,
        "{whose} needs the ID of its machine (pta tools call machine_list)."
    );
    ensure!(
        PROVIDERS.contains(&director.provider.as_str())
            && crate::llm::valid_model(&director.model)
            && !director.effort.is_empty()
            && director.effort.len() <= 40
            && director
                .effort
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "{whose} needs its agent: provider (codex, claude or opencode), model and effort."
    );
    Ok(())
}

/// The ready states refused for a project: those its process does not have.
pub fn unknown_states<'a>(states: &'a [String], process: &BTreeSet<String>) -> Vec<&'a str> {
    states
        .iter()
        .filter(|s| !process.contains(*s))
        .map(String::as_str)
        .collect()
}

/// The organization's name (lowercased) of an Azure DevOps work item link.
pub fn item_organization(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let first = parsed.path_segments()?.find(|s| !s.is_empty())?;
    match parsed.host_str()? {
        "dev.azure.com" => organization_name(&format!("https://dev.azure.com/{first}")),
        host => host
            .strip_suffix(".visualstudio.com")
            .map(str::to_lowercase),
    }
}

fn same_project(workflow: &Workflow, organization: &str, project: &str) -> bool {
    organization_name(&workflow.organization).as_deref() == Some(organization)
        && workflow.project.eq_ignore_ascii_case(project)
}

/// The workflows the director may choose for an item of that organization (its lowercased
/// name) and project: the enabled ones of that project, in the map's order.
pub fn candidates<'a>(
    workflows: &'a [Workflow],
    organization: &str,
    project: &str,
) -> Vec<&'a Workflow> {
    workflows
        .iter()
        .filter(|w| w.enabled && same_project(w, organization, project))
        .collect()
}

/// What code concluded from the director's answer.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// A workflow it sent, still enabled and of the item's project.
    Chosen,
    /// The director said none fits.
    None,
    /// No answer, an answer that is not the JSON asked for, or an ID it did not send.
    Unrecognized,
    /// A workflow that is disabled.
    Disabled,
    /// A workflow of another project.
    OtherProject,
}
impl Verdict {
    /// Its word, as it is serialized (`other_project`, …).
    pub fn word(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

/// The checked answer: the workflow (only when chosen), the verdict, the reason to show (the
/// director's line when it chose or said none; the verdict's word otherwise) and what the
/// director said, bounded.
#[derive(Clone, Debug, PartialEq)]
pub struct Judgment {
    pub workflow: Option<String>,
    pub verdict: Verdict,
    pub reason: String,
    pub said: Option<String>,
    pub said_reason: String,
}

/// The first line of a text, without control characters, at most `max` characters.
pub fn one_line(text: &str, max: usize) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

/// Check the director's answer (`{"workflow": ID or "", "reason": TEXT}`; none when it gave
/// no usable answer) against the IDs code sent and the workflows as they are now.
pub fn judge(
    answer: Option<&Value>,
    sent: &[String],
    now: &[Workflow],
    organization: &str,
    project: &str,
) -> Judgment {
    let refused = |verdict: Verdict, said: Option<String>, said_reason: String| Judgment {
        workflow: None,
        verdict,
        reason: verdict.word(),
        said,
        said_reason,
    };
    let shaped = answer.and_then(|a| {
        let object = a.as_object()?;
        (object.len() == 2).then_some(())?;
        Some((a["workflow"].as_str()?, a["reason"].as_str()?))
    });
    let Some((workflow, reason)) = shaped else {
        return refused(Verdict::Unrecognized, None, String::new());
    };
    let reason = one_line(reason, MAX_REASON);
    let id = workflow.trim();
    if id.is_empty() {
        return Judgment {
            workflow: None,
            verdict: Verdict::None,
            reason: if reason.is_empty() {
                Verdict::None.word()
            } else {
                reason.clone()
            },
            said: None,
            said_reason: reason,
        };
    }
    let said: String = id.chars().filter(|c| !c.is_control()).take(64).collect();
    let Some(found) = now.iter().find(|w| w.id == id) else {
        return refused(Verdict::Unrecognized, Some(said), reason);
    };
    if !same_project(found, organization, project) {
        return refused(Verdict::OtherProject, Some(said), reason);
    }
    if !found.enabled {
        return refused(Verdict::Disabled, Some(said), reason);
    }
    if !sent.iter().any(|s| s == id) {
        return refused(Verdict::Unrecognized, Some(said), reason);
    }
    Judgment {
        workflow: Some(found.id.clone()),
        verdict: Verdict::Chosen,
        reason: if reason.is_empty() {
            Verdict::Chosen.word()
        } else {
            reason.clone()
        },
        said: Some(said),
        said_reason: reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::tests::workflow;
    use serde_json::json;

    const MACHINE: &str = "5a6b7c8d-9e0f-4a1b-8c2d-3e4f5a6b7c8d";

    #[test]
    fn defaults_are_left_out_and_bounds_are_refused() {
        let settings = Settings::default();
        assert!(settings.is_default());
        assert_eq!(settings.watch_interval_minutes, 5);
        assert_eq!(settings.concurrency, 2);
        assert_eq!(settings.pull_request_wait_days, 7);
        assert!(validate(&settings).is_ok());
        for (field, value) in [
            ("watch_interval_minutes", 0),
            ("watch_interval_minutes", 61),
            ("concurrency", 0),
            ("concurrency", 5),
            ("pull_request_wait_days", 0),
            ("pull_request_wait_days", 61),
        ] {
            let mut changed = Settings::default();
            match field {
                "watch_interval_minutes" => changed.watch_interval_minutes = value,
                "concurrency" => changed.concurrency = value,
                _ => changed.pull_request_wait_days = value,
            }
            let error = validate(&changed).unwrap_err().to_string();
            assert!(error.contains(field), "{error}");
        }
        let ready = |project: &str, states: &[&str]| ReadyStates {
            organization: "https://dev.azure.com/example".into(),
            project: project.into(),
            states: states.iter().map(|s| s.to_string()).collect(),
        };
        let twice = Settings {
            ready_states: vec![ready("Payments", &["Ready"]), ready("payments", &["New"])],
            ..Default::default()
        };
        assert!(validate(&twice).is_err());
        for states in [&[][..], &["Ready", "Ready"], &[" "]] {
            let bad = Settings {
                ready_states: vec![ready("Payments", states)],
                ..Default::default()
            };
            assert!(validate(&bad).is_err(), "{states:?}");
        }
        let mut director = Settings {
            director: Some(Director {
                machine: MACHINE.into(),
                provider: "codex".into(),
                model: "gpt-6.1-sol".into(),
                effort: "low".into(),
            }),
            ..Default::default()
        };
        assert!(validate(&director).is_ok());
        director.director.as_mut().unwrap().provider = "deepseek".into();
        assert!(validate(&director).is_err());
        director.director.as_mut().unwrap().provider = "codex".into();
        director.director.as_mut().unwrap().machine = "this-computer".into();
        assert!(validate(&director).is_err());
        let process = BTreeSet::from(["New".to_owned(), "Ready".to_owned()]);
        assert_eq!(
            unknown_states(&["Ready".into(), "Readyy".into()], &process),
            ["Readyy"]
        );
    }

    fn director(machine: &str, model: &str) -> Director {
        Director {
            machine: machine.into(),
            provider: "codex".into(),
            model: model.into(),
            effort: "low".into(),
        }
    }

    #[test]
    fn a_project_director_comes_first_then_the_shared_workflow_agent_then_the_global_one() {
        const OTHER: &str = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
        let mut payments = workflow("payments");
        payments.auto_start = true;
        let mut refunds = workflow("refunds");
        refunds.auto_start = true;
        // A manual workflow on another machine is not among the defaults while one starts by
        // itself.
        let mut manual = workflow("manual");
        manual.machine = OTHER.into();
        let mut all = vec![payments, refunds, manual];
        let mut settings = Settings::default();
        let resolved = |settings: &Settings, all: &[Workflow]| {
            resolve(settings, all, "example", "payments").map(|r| (r.director, r.source))
        };
        // Every candidate shares machine and agent: the workflow's.
        let (shared, source) = resolved(&settings, &all).unwrap();
        assert_eq!(source, Source::Workflow);
        assert_eq!(shared.machine, MACHINE);
        assert_eq!(
            (
                shared.provider.as_str(),
                shared.model.as_str(),
                shared.effort.as_str()
            ),
            ("codex", "gpt-6.1-sol", "medium")
        );
        // They differ: the global director, or none without one.
        all[1].agent.effort = "high".into();
        assert_eq!(resolved(&settings, &all), None);
        settings.director = Some(director(MACHINE, "global-model"));
        assert_eq!(resolved(&settings, &all).unwrap().1, Source::Global);
        // Nothing starts by itself: every enabled workflow is a default.
        for w in &mut all {
            w.auto_start = false;
        }
        all[1].agent.effort = "medium".into();
        assert_eq!(resolved(&settings, &all).unwrap().1, Source::Global);
        all[2].enabled = false;
        assert_eq!(resolved(&settings, &all).unwrap().1, Source::Workflow);
        // The project's own director wins, by organization and project in any letter case.
        settings.project_directors = vec![ProjectDirector {
            organization: "https://dev.azure.com/Example".into(),
            project: "PAYMENTS".into(),
            director: director(OTHER, "project-model"),
        }];
        let (own, source) = resolved(&settings, &all).unwrap();
        assert_eq!(
            (own.model.as_str(), source),
            ("project-model", Source::Project)
        );
        assert!(
            resolve(&settings, &all, "other", "payments")
                .is_some_and(|r| r.source == Source::Global)
        );
        // A project without workflows has only the global director.
        assert_eq!(
            resolve(&settings, &all, "example", "billing")
                .unwrap()
                .source,
            Source::Global
        );
        assert_eq!(Source::Workflow.word(), "workflow");
    }

    #[test]
    fn a_project_has_its_own_director_once_with_valid_fields() {
        let own = |project: &str, director: Director| ProjectDirector {
            organization: "https://dev.azure.com/example".into(),
            project: project.into(),
            director,
        };
        let mut settings = Settings {
            project_directors: vec![own("Payments", director(MACHINE, "m"))],
            ..Default::default()
        };
        assert!(validate(&settings).is_ok());
        assert!(!settings.is_default());
        let text = toml::to_string(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), settings);
        settings
            .project_directors
            .push(own("payments", director(MACHINE, "m")));
        assert!(
            validate(&settings)
                .unwrap_err()
                .to_string()
                .contains("once")
        );
        settings.project_directors.pop();
        settings.project_directors[0].director.machine = "this-computer".into();
        assert!(
            validate(&settings)
                .unwrap_err()
                .to_string()
                .contains("The director of Payments")
        );
        settings.project_directors[0].director.machine = MACHINE.into();
        settings.project_directors[0].organization = "https://example.com/x".into();
        assert!(validate(&settings).is_err());
        // Left out while empty, so a map without it is unchanged.
        assert!(
            !toml::to_string(&Settings::default())
                .unwrap()
                .contains("project_directors")
        );
    }

    #[test]
    fn the_organization_of_an_item_link_is_read_from_its_url() {
        assert_eq!(
            item_organization("https://dev.azure.com/Example/Payments/_workitems/edit/41"),
            Some("example".into())
        );
        assert_eq!(
            item_organization("https://example.visualstudio.com/Payments/_workitems/edit/41"),
            Some("example".into())
        );
        assert_eq!(item_organization("https://evil.example/x/y"), None);
    }

    #[test]
    fn only_a_sent_enabled_workflow_of_the_items_project_is_chosen() {
        let payments = workflow("payments");
        let mut refunds = workflow("refunds");
        refunds.description = "Refunds".into();
        let mut paused = workflow("paused");
        paused.enabled = false;
        let mut billing = workflow("billing");
        billing.project = "Billing".into();
        let mut other_organization = workflow("elsewhere");
        other_organization.organization = "https://dev.azure.com/other".into();
        let all = vec![payments, refunds, paused, billing, other_organization];
        let sent: Vec<String> = candidates(&all, "example", "payments")
            .iter()
            .map(|w| w.id.clone())
            .collect();
        assert_eq!(sent, ["payments", "refunds"]);
        let judge = |answer: Value| judge(Some(&answer), &sent, &all, "example", "Payments");
        let chosen = judge(json!({"workflow":"refunds","reason":"Es un reembolso.\nMás texto"}));
        assert_eq!(chosen.workflow.as_deref(), Some("refunds"));
        assert_eq!(chosen.verdict, Verdict::Chosen);
        assert_eq!(chosen.reason, "Es un reembolso.");
        let none = judge(json!({"workflow":"","reason":"Ninguno aplica."}));
        assert_eq!((none.workflow, none.verdict), (None, Verdict::None));
        assert_eq!(none.reason, "Ninguno aplica.");
        for (answer, verdict) in [
            (
                json!({"workflow":"unknown","reason":"x"}),
                Verdict::Unrecognized,
            ),
            (json!({"workflow":"paused","reason":"x"}), Verdict::Disabled),
            (
                json!({"workflow":"billing","reason":"x"}),
                Verdict::OtherProject,
            ),
            (
                json!({"workflow":"elsewhere","reason":"x"}),
                Verdict::OtherProject,
            ),
            (json!({"workflow":"payments"}), Verdict::Unrecognized),
            (
                json!({"workflow":"payments","reason":"x","grant":"all"}),
                Verdict::Unrecognized,
            ),
            (json!(["payments"]), Verdict::Unrecognized),
            (json!({"workflow":7,"reason":"x"}), Verdict::Unrecognized),
        ] {
            let judged = judge(answer.clone());
            assert_eq!(judged.workflow, None, "{answer}");
            assert_eq!(judged.verdict, verdict, "{answer}");
            assert_eq!(judged.reason, verdict.word(), "{answer}");
        }
        // No answer at all (malformed or empty) is unrecognized.
        let empty = super::judge(None, &sent, &all, "example", "Payments");
        assert_eq!(empty.verdict, Verdict::Unrecognized);
        // A workflow disabled after the director was asked is refused as disabled.
        let mut now = all.clone();
        now[1].enabled = false;
        let late = super::judge(
            Some(&json!({"workflow":"refunds","reason":"x"})),
            &sent,
            &now,
            "example",
            "Payments",
        );
        assert_eq!(late.verdict, Verdict::Disabled);
        // One added during the session was not among the candidates.
        let mut added = workflow("added");
        added.enabled = true;
        now.push(added);
        let unsent = super::judge(
            Some(&json!({"workflow":"added","reason":"x"})),
            &sent,
            &now,
            "example",
            "Payments",
        );
        assert_eq!(unsent.verdict, Verdict::Unrecognized);
    }
}
