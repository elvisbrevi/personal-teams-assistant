//! The workflow director's operations, shared by the GUI, the CLI and MCP: its settings
//! (`workflow_settings`, `workflow_settings_update`) and the dry-run classification of one
//! work item (`workflow_classify`).
//!
//! An update is checked before it is saved: the bounds, every ready state against the states
//! the project's process has for its requirement, bug and listed task types (read with the
//! enabled source's read credential), and the director's agent against the CLIs installed on
//! its machine (`implementation_catalog`); what cannot be read there (an offline or older
//! machine) is a warning. A classification reads the item fresh, sends the director the item
//! and the enabled workflows of its project, and checks the answer in code
//! (`workflows::director::judge`): nothing starts and nothing is written but the session's
//! record, which Agent history lists.
use super::*;
use crate::workflows::director::{self as core, Director, ReadyStates, Settings};

pub(in crate::app) const METHODS: [&str; 3] = [
    "workflow_settings",
    "workflow_settings_update",
    "workflow_classify",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowArgs {
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    organization: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

/// The ready states of one project to set; no states clears them.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyChange {
    #[serde(default)]
    organization: Option<String>,
    project: String,
    #[serde(default)]
    states: Vec<String>,
}
/// The director's fields to change; a new director needs all four.
#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectorChange {
    #[serde(default)]
    machine: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    #[serde(default)]
    watch_interval_minutes: Option<u32>,
    #[serde(default)]
    concurrency: Option<u32>,
    #[serde(default)]
    pull_request_wait_days: Option<u32>,
    /// The director's fields to change, merged with the stored director; `null` clears it.
    #[serde(default)]
    director: Option<DirectorChange>,
    #[serde(default)]
    ready_states: Vec<ReadyChange>,
    /// The Azure source the ready states are read from, when several are enabled.
    #[serde(default)]
    source: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassifyArgs {
    provider: String,
    id: u64,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

/// The settings as the operations show them: every value, defaults included.
fn view(settings: &Settings) -> Value {
    json!({
        "watch_interval_minutes": settings.watch_interval_minutes,
        "concurrency": settings.concurrency,
        "pull_request_wait_days": settings.pull_request_wait_days,
        "director": settings.director,
        "ready_states": settings.ready_states,
    })
}
fn bounds() -> Value {
    let range = |r: std::ops::RangeInclusive<u32>| json!([r.start(), r.end()]);
    json!({
        "watch_interval_minutes": range(core::WATCH_INTERVAL_MINUTES),
        "concurrency": range(core::CONCURRENCY),
        "pull_request_wait_days": range(core::PULL_REQUEST_WAIT_DAYS),
        "ready_states_per_project": core::MAX_READY_STATES,
        "projects_with_ready_states": core::MAX_READY_PROJECTS,
    })
}

pub(super) async fn operation(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> Result<Value, String> {
    match method {
        "workflow_settings" => show(host, parse(args)?).await,
        "workflow_settings_update" => update(host, args, &Process::Azure).await,
        "workflow_classify" => classify(host, parse(args)?).await,
        _ => Err(invalid("Unknown workflow operation.")),
    }
}

async fn show(host: &Arc<Host>, args: ShowArgs) -> Result<Value, String> {
    let state = &host.state;
    let map = state.settings.map().map_err(fail)?;
    let mut reply = json!({
        "settings": view(&map.workflow_settings),
        "defaults": view(&Settings::default()),
        "bounds": bounds(),
        "editable": editable(state)?,
        "watcher": super::watcher::view(state),
    });
    if let Some(project) = &args.project {
        reply["process_states"] = tools::todo::process_states(
            state,
            args.source.as_deref(),
            args.organization.as_deref(),
            project,
        )
        .await?;
    } else if args.organization.is_some() || args.source.is_some() {
        return Err(invalid(
            "Give project with organization or source to read that project's states.",
        ));
    }
    Ok(reply)
}

/// What an update changes once it has been checked: the director (set or cleared), each
/// project's new ready states as the catalog spells them, and the ready states it clears
/// (organization name and project, lowercased).
#[derive(Default)]
struct Resolved {
    director: Option<Option<Director>>,
    ready: Vec<ReadyStates>,
    cleared: Vec<(String, String)>,
}

fn same_ready(stored: &ReadyStates, organization: &str, project: &str) -> bool {
    crate::workflows::organization_name(&stored.organization).as_deref() == Some(organization)
        && stored.project.eq_ignore_ascii_case(project)
}

/// The update applied to settings.
fn apply(settings: &mut Settings, args: &UpdateArgs, resolved: &Resolved) {
    if let Some(value) = args.watch_interval_minutes {
        settings.watch_interval_minutes = value;
    }
    if let Some(value) = args.concurrency {
        settings.concurrency = value;
    }
    if let Some(value) = args.pull_request_wait_days {
        settings.pull_request_wait_days = value;
    }
    if let Some(director) = &resolved.director {
        settings.director = director.clone();
    }
    for (organization, project) in &resolved.cleared {
        settings
            .ready_states
            .retain(|r| !same_ready(r, organization, project));
    }
    for entry in &resolved.ready {
        let organization =
            crate::workflows::organization_name(&entry.organization).unwrap_or_default();
        settings
            .ready_states
            .retain(|r| !same_ready(r, &organization, &entry.project));
        settings.ready_states.push(entry.clone());
    }
}

/// The director after a change: the fields given over the stored director; without one, every
/// field is needed.
fn merge_director(stored: Option<&Director>, change: &DirectorChange) -> Result<Director, String> {
    let fields = [
        ("machine", &change.machine),
        ("provider", &change.provider),
        ("model", &change.model),
        ("effort", &change.effort),
    ];
    let Some(stored) = stored else {
        let missing: Vec<&str> = fields
            .iter()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| *name)
            .collect();
        if !missing.is_empty() {
            return Err(invalid(format!(
                "There is no workflow director yet: give its machine, provider, model and effort (missing {}).",
                missing.join(", ")
            )));
        }
        return Ok(Director {
            machine: change.machine.clone().unwrap_or_default(),
            provider: change.provider.clone().unwrap_or_default(),
            model: change.model.clone().unwrap_or_default(),
            effort: change.effort.clone().unwrap_or_default(),
        });
    };
    let pick = |value: &Option<String>, stored: &String| value.clone().unwrap_or(stored.clone());
    Ok(Director {
        machine: pick(&change.machine, &stored.machine),
        provider: pick(&change.provider, &stored.provider),
        model: pick(&change.model, &stored.model),
        effort: pick(&change.effort, &stored.effort),
    })
}

/// The stored ready states a clearing change names: by organization and project, or by project
/// alone when only one organization has it. A project in several organizations needs its
/// organization, as setting its states does.
fn resolve_clear(
    stored: &Settings,
    change: &ReadyChange,
) -> Result<Option<(String, String)>, String> {
    let organization = change
        .organization
        .as_deref()
        .map(|o| {
            crate::workflows::organization_name(o)
                .ok_or_else(|| invalid("Give the organization as https://dev.azure.com/NAME."))
        })
        .transpose()?;
    let matching: Vec<&ReadyStates> = stored
        .ready_states
        .iter()
        .filter(|r| {
            r.project.eq_ignore_ascii_case(&change.project)
                && organization.as_ref().is_none_or(|o| {
                    crate::workflows::organization_name(&r.organization).as_ref() == Some(o)
                })
        })
        .collect();
    match matching.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some((
            crate::workflows::organization_name(&one.organization).unwrap_or_default(),
            one.project.to_lowercase(),
        ))),
        _ => Err(invalid(format!(
            "Project {} has ready states in several organizations; give its organization to clear them.",
            change.project
        ))),
    }
}

/// Where the process states of a project come from: the enabled Azure source, or a test's
/// fixed answers by lowercased project name, each after a delay, under a bound.
pub(super) enum Process {
    Azure,
    #[cfg(test)]
    Fixed(BTreeMap<String, Value>),
    #[cfg(test)]
    Delayed {
        projects: BTreeMap<String, Value>,
        delay: std::time::Duration,
        bound: std::time::Duration,
    },
}
/// How long reading the process states of every project in one update may take, all of them
/// read at once. With the director's check after it (at most 40 seconds), an update answers
/// within the control channel's 120-second client deadline and never commits after the client
/// gave up.
pub(super) const READY_READ_SECONDS: u64 = 60;
impl Process {
    fn bound(&self) -> std::time::Duration {
        match self {
            #[cfg(test)]
            Self::Delayed { bound, .. } => *bound,
            _ => std::time::Duration::from_secs(READY_READ_SECONDS),
        }
    }
    async fn states(
        &self,
        state: &DesktopState,
        source: Option<&str>,
        organization: Option<&str>,
        project: &str,
    ) -> Result<Value, String> {
        #[cfg(test)]
        let fixed = |projects: &BTreeMap<String, Value>| {
            projects.get(&project.to_lowercase()).cloned().ok_or_else(|| {
                invalid(format!(
                    "invalid project: {project} is not in the configured organizations of the Azure source"
                ))
            })
        };
        match self {
            Self::Azure => tools::todo::process_states(state, source, organization, project).await,
            #[cfg(test)]
            Self::Fixed(projects) => fixed(projects),
            #[cfg(test)]
            Self::Delayed {
                projects, delay, ..
            } => {
                tokio::time::sleep(*delay).await;
                fixed(projects)
            }
        }
    }
}

/// A stand-in organization for a ready-state change that names none, so its own shape is
/// checked before anything is read; the count of projects is checked on the final list.
const UNREAD: &str = "https://dev.azure.com/unread";

/// `workflow_settings_update`, with the process states read from `process`.
pub(super) async fn update(
    host: &Arc<Host>,
    args: &Value,
    process: &Process,
) -> Result<Value, String> {
    let state = &host.state;
    require_coordinator(state)?;
    let clear_director = args.get("director").is_some_and(Value::is_null);
    let args: UpdateArgs = parse(args)?;
    if args.watch_interval_minutes.is_none()
        && args.concurrency.is_none()
        && args.pull_request_wait_days.is_none()
        && args.director.is_none()
        && !clear_director
        && args.ready_states.is_empty()
    {
        return Err(invalid(
            "Give at least one setting to change: watch_interval_minutes, concurrency, pull_request_wait_days, director or ready_states.",
        ));
    }
    let stored = state.settings.map().map_err(fail)?.workflow_settings;
    let (clearing, wanted): (Vec<ReadyChange>, Vec<ReadyChange>) = args
        .ready_states
        .iter()
        .cloned()
        .partition(|c| c.states.is_empty());
    let mut resolved = Resolved {
        director: if clear_director {
            Some(None)
        } else {
            args.director
                .as_ref()
                .map(|change| merge_director(stored.director.as_ref(), change))
                .transpose()?
                .map(Some)
        },
        ..Default::default()
    };
    for change in &clearing {
        resolved.cleared.extend(resolve_clear(&stored, change)?);
    }
    // Everything the settings alone can refuse, before the process or a machine is read: the
    // scalars, the director and the clearings on the stored list, and each new entry's shape.
    let mut candidate = stored.clone();
    apply(&mut candidate, &args, &resolved);
    core::validate(&candidate).map_err(invalid)?;
    for change in &wanted {
        core::validate(&Settings {
            ready_states: vec![ReadyStates {
                organization: change.organization.clone().unwrap_or_else(|| UNREAD.into()),
                project: change.project.clone(),
                states: change.states.clone(),
            }],
            ..Default::default()
        })
        .map_err(invalid)?;
    }
    // Every ready state must be one of its project's process states; the projects are read at
    // once, under one bound, and nothing is saved when it passes.
    let reads = futures_util::future::join_all(wanted.iter().map(|change| {
        process.states(
            state,
            args.source.as_deref(),
            change.organization.as_deref(),
            &change.project,
        )
    }));
    let read = tokio::time::timeout(process.bound(), reads)
        .await
        .map_err(|_| {
            "[network] The projects' process states could not be read in time; nothing was saved."
                .to_owned()
        })?;
    for (change, process) in wanted.iter().zip(read) {
        let process = process?;
        let states: std::collections::BTreeSet<String> =
            serde_json::from_value(process["states"].clone()).map_err(fail)?;
        let unknown = core::unknown_states(&change.states, &states);
        if !unknown.is_empty() {
            return Err(invalid(format!(
                "Project {} has no state {}; its requirement, bug and listed task types have {}.",
                process["project"].as_str().unwrap_or(&change.project),
                unknown
                    .iter()
                    .map(|s| format!("«{s}»"))
                    .collect::<Vec<_>>()
                    .join(", "),
                states
                    .iter()
                    .map(|s| format!("«{s}»"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        resolved.ready.push(ReadyStates {
            organization: process["organization"]
                .as_str()
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_owned(),
            project: process["project"]
                .as_str()
                .unwrap_or(&change.project)
                .to_owned(),
            states: change.states.clone(),
        });
    }
    // The final list, with the projects the catalog resolved: re-setting a project replaces it.
    let mut candidate = stored;
    apply(&mut candidate, &args, &resolved);
    core::validate(&candidate).map_err(invalid)?;
    let mut warnings = Vec::new();
    if let Some(Some(director)) = &resolved.director {
        warnings.extend(verify_director(host, director).await?);
    }
    let saved = change(host, |map| {
        apply(&mut map.workflow_settings, &args, &resolved);
        core::validate(&map.workflow_settings).map_err(invalid)?;
        Ok(map.workflow_settings.clone())
    })
    .await?;
    Ok(json!({"settings":view(&saved),"saved":true,"warnings":warnings}))
}

/// The director's machine must be the account's, and its agent installed there; what the
/// machine cannot answer now is a warning.
async fn verify_director(host: &Arc<Host>, director: &Director) -> Result<Vec<String>, String> {
    let machine = implementation::machine_facts(host, &director.machine)
        .await
        .map_err(fail)?
        .ok_or_else(|| {
            invalid(format!(
                "Machine {} is not an execution machine of this account (machine_list).",
                director.machine
            ))
        })?;
    if machine.own {
        // The shared server runs no agent for another account.
        authority::require_execution(&host.state)
            .map_err(|_| invalid(authority::EXECUTION_DENIED))?;
    }
    let mut warnings = Vec::new();
    if !(machine.own || machine.online) {
        warnings.push("The director's machine is offline, so its agent could not be verified; it is checked again when the director runs.".to_owned());
        return Ok(warnings);
    }
    if !machine.direct {
        warnings.push("The director's machine runs a version that cannot run the workflow director yet; update it before classifying.".to_owned());
    }
    match implementation::machine_call(
        host,
        &director.machine,
        "implementation_catalog",
        json!({}),
    )
    .await
    {
        Ok(catalog) => crate::workflows::agent_installed(
            &catalog,
            &director.agent(),
            "the workflow director's machine",
        )
        .map_err(invalid)?,
        Err(_) => warnings.push("The director's machine did not list its installed CLIs, so its agent could not be verified; it is checked again when the director runs.".to_owned()),
    }
    Ok(warnings)
}

/// How long the fresh item read may take before the director is asked: with the wait for the
/// director (`implementation::director::WAIT_SECONDS`), a classification answers within the
/// control channel's client deadline.
pub(super) const ITEM_SECONDS: u64 = 20;
const DESCRIPTION: usize = 8000;

/// The item fresh, in one detail read of any category (an HU, a bug or a task), with the
/// category its type was found in.
async fn read_item(
    state: &DesktopState,
    args: &ClassifyArgs,
) -> Result<crate::tools::todo::Detail, String> {
    let body = json!({"id":args.id,"project":args.project,"source":args.source});
    let read = tools::todo::read(state, "get_azure_item", &body);
    let value = tokio::time::timeout(std::time::Duration::from_secs(ITEM_SECONDS), read)
        .await
        .map_err(|_| "[network] The work item could not be read in time.".to_owned())??;
    serde_json::from_value(value).map_err(fail)
}

/// The item as the director reads it: type, category, project, title, description, tags, area
/// and iteration, bounded (untrusted input).
pub(super) fn item_input(detail: &crate::tools::todo::Detail) -> Value {
    let field = |reference: &str| {
        detail
            .fields
            .iter()
            .find(|f| f.reference == reference)
            .and_then(|f| f.value.as_str())
            .unwrap_or_default()
    };
    let bounded = |text: &str, max: usize| text.chars().take(max).collect::<String>();
    // Plain text with its spacing collapsed: the markup says nothing about the item.
    let description = crate::adapters::teams::plain_text(field("System.Description"))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let tags: Vec<String> = field("System.Tags")
        .split(';')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .take(50)
        .map(|t| bounded(t, 100))
        .collect();
    let item = &detail.item;
    json!({
        "id": item.id,
        "type": item.kind,
        "category": item.category,
        "project": item.project,
        "state": item.state,
        "title": bounded(&item.title, 500),
        "description": bounded(description.trim(), DESCRIPTION),
        "tags": tags,
        "area": bounded(field("System.AreaPath"), 400),
        "iteration": bounded(field("System.IterationPath"), 400),
    })
}

async fn classify(host: &Arc<Host>, args: ClassifyArgs) -> Result<Value, String> {
    let state = &host.state;
    require_coordinator(state)?;
    if args.provider != "azure" {
        return Err(invalid(
            "Workflows cover Azure DevOps work items: give provider azure.",
        ));
    }
    let director = state
        .settings
        .map()
        .map_err(fail)?
        .workflow_settings
        .director
        .ok_or_else(|| {
            "[not_ready] Choose the workflow director first (pta workflows settings --set director.machine ID --set director.provider P --set director.model M --set director.effort E).".to_owned()
        })?;
    let detail = read_item(state, &args).await?;
    classify_item(host, &director, &detail, false).await
}

/// The dry-run classification of an item (the Implement entry): the director's choice among the
/// enabled workflows of its project, and nothing else.
pub(super) async fn classify_for(
    host: &Arc<Host>,
    provider: &str,
    id: u64,
    project: Option<String>,
    source: Option<String>,
) -> Result<Value, String> {
    classify(
        host,
        ClassifyArgs {
            provider: provider.to_owned(),
            id,
            project,
            source,
        },
    )
    .await
}

/// Classify an item already read: ask the director on its machine and check its answer. With
/// `auto_only` (a run the watcher started) it chooses among the workflows that start by
/// themselves only.
pub(super) async fn classify_item(
    host: &Arc<Host>,
    director: &Director,
    detail: &crate::tools::todo::Detail,
    auto_only: bool,
) -> Result<Value, String> {
    let state = &host.state;
    let item = &detail.item;
    let organization = core::item_organization(&item.url)
        .ok_or_else(|| invalid("The work item has no Azure DevOps link."))?;
    let map = state.settings.map().map_err(fail)?;
    let mut candidates = core::candidates(&map.workflows, &organization, &item.project);
    candidates.retain(|w| !auto_only || w.auto_start);
    let listed: Vec<Value> = candidates
        .iter()
        .map(|w| json!({"id":w.id,"name":w.name}))
        .collect();
    let sent: Vec<String> = candidates.iter().map(|w| w.id.clone()).collect();
    let verified = json!({"id":item.id,"title":item.title,"url":item.url,"project":item.project,"type":item.kind,"category":item.category,"state":item.state});
    if candidates.is_empty() {
        // Nothing to choose from: the director is not asked.
        return Ok(json!({
            "workflow": null,
            "outcome": core::Verdict::None.word(),
            "reason": if auto_only {
                format!("No enabled workflow of project {} starts by itself.", item.project)
            } else {
                format!("No enabled workflow covers project {}.", item.project)
            },
            "candidates": listed,
            "item": verified,
            "run": null,
            "dry_run": true,
        }));
    }
    let machine = implementation::machine_facts(host, &director.machine)
        .await
        .map_err(fail)?
        .ok_or_else(|| {
            invalid(format!(
                "The workflow director's machine {} is not an execution machine of this account; choose it again (workflow_settings_update).",
                director.machine
            ))
        })?;
    if !(machine.own || machine.online) {
        return Err("[not_ready] The workflow director's machine is offline.".into());
    }
    let input = json!({
        "language": state.settings.config().map_err(fail)?.llm.language,
        "item": item_input(detail),
        "workflows": candidates.iter().map(|w| json!({
            "id": w.id,
            "name": w.name,
            "description": w.description,
            "repositories": w.repositories.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
    .to_string();
    let instructions =
        crate::agent_prompts::resolve(&map.agent_prompts, crate::agent_prompts::DIRECT);
    let answer = Box::pin(implementation::director::ask(
        host,
        implementation::director::Ask {
            machine: &director.machine,
            director,
            instructions: &instructions,
            input,
            item,
        },
    ))
    .await
    .map_err(|error| {
        let redactor =
            security::Redactor::new(&[], Vec::new()).expect("built-in redaction patterns");
        format!("[not_ready] {}", redactor.redact(&error.to_string()))
    })?;
    // Checked against the workflows as they are now, not as they were sent.
    let now = state.settings.map().map_err(fail)?.workflows;
    let judgment = core::judge(
        answer.direction.answer.as_ref(),
        &sent,
        &now,
        &organization,
        &item.project,
    );
    let verdict = json!({
        "workflow": judgment.workflow,
        "outcome": judgment.verdict.word(),
        "reason": judgment.reason,
        "director": {"workflow": judgment.said, "reason": judgment.said_reason},
        "candidates": sent,
    });
    implementation::director::record(host, &answer.run, verdict).map_err(fail)?;
    Ok(json!({
        "workflow": judgment.workflow,
        "outcome": judgment.verdict.word(),
        "reason": judgment.reason,
        "director": {"workflow": judgment.said, "reason": judgment.said_reason},
        "problem": answer.direction.problem,
        "candidates": listed,
        "item": verified,
        "run": answer.run,
        "session_id": answer.session_id,
        "dry_run": true,
    }))
}
