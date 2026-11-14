// Settings › Workflows: the ready states and own director of each project, the watcher's
// interval, how many runs advance at once, the pull-request wait and the account's workflow
// director (`workflow_settings`, `workflow_settings_update`), and which director each project
// gets and where it comes from (`directors`). Its edits are unsaved Settings edits like any other category's:
// the save bar saves them (`saveWorkflowSettings`) after the profile, and Discard drops them.
import { t, tn, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, el } from './dom.js';
import { live } from './state.js';
import { busy } from './notices.js';
import { markDirty, showTab } from './nav.js';
import { providerNames, renderLive } from './home.js';
import { effortNames, fillAgentChoices } from './settings-models.js';
import { showAgentPrompt } from './guides.js';

/**
 * What the host last answered (`settings`, `defaults`, `bounds`, `editable`), and the form's copy
 * of the settings with whether it holds edits not saved yet. A pending form survives the
 * category being shown again; only saving or discarding replaces it.
 */
let workflowSettings = null;
let form = null;
let pending = false;
/** The director is sent only when the person changed it, never because a catalog filled it in. */
let directorTouched = false;
/** The projects whose own director the person changed (`projectKey`). */
const projectDirectorsTouched = new Set();
let machines = [];
/** Each project's process states (`workflow_settings {project}`), read when the category opens. */
const processStates = new Map();
const agentCatalogs = new Map();
let loadGeneration = 0;
/** The numbers of the form and their host field; their bounds come with the settings. */
const FIELDS = [['#workflow-interval', 'watch_interval_minutes'], ['#workflow-concurrency', 'concurrency'], ['#workflow-pr-wait', 'pull_request_wait_days']];
const FALLBACK = { watch_interval_minutes: [1, 60], concurrency: [1, 4], pull_request_wait_days: [1, 60] };
const range = field => workflowSettings?.bounds?.[field] ?? FALLBACK[field];
const RANGE_TEXT = {
  watch_interval_minutes: bounds => t('Check every {min} to {max} minutes.', bounds),
  concurrency: bounds => t('Let {min} to {max} runs advance at once.', bounds),
  pull_request_wait_days: bounds => t('Wait {min} to {max} days for pull requests.', bounds),
};

const message = error => tx(String(error?.message ?? error));
const projectKey = (organization, project) => `${(organization ?? '').toLowerCase()}|${(project ?? '').toLowerCase()}`;

/** A change in the category: the form keeps it and the save bar offers to save it. */
function edited() {
  pending = true;
  markDirty({ target: $('#settings-workflows') });
}

/** The saved settings only (Home's check reads nothing else). */
async function readWorkflowSettings() {
  workflowSettings = await invoke('workflow_settings');
  if (!pending) resetForm();
  return workflowSettings;
}
function resetForm() {
  form = structuredClone(workflowSettings.settings);
  form.ready_states ??= [];
  form.project_directors ??= [];
  directorTouched = false;
  projectDirectorsTouched.clear();
}

/** The category: the settings, the machines, then each project's process states. */
async function loadWorkflowSettings() {
  const generation = ++loadGeneration;
  $('#workflow-settings-progress').textContent = t('Loading…');
  $('#workflow-settings-problem').hidden = true;
  try {
    const [, fleet] = await Promise.all([readWorkflowSettings(), invoke('machine_list').catch(() => ({ machines: [] }))]);
    if (generation !== loadGeneration) return;
    machines = fleet.machines ?? [];
    $('#workflow-settings-progress').textContent = '';
    renderWorkflowSettings();
    renderLive();
    // Each project's states come from its process, read (and cached) by the host.
    await Promise.all(settingsProjects().filter(p => !processStates.has(projectKey(p.organization, p.project))).map(async project => {
      const key = projectKey(project.organization, project.project);
      try {
        const reply = await invoke('workflow_settings', { project: project.project, ...(project.organization ? { organization: project.organization } : {}) });
        processStates.set(key, { states: reply.process_states?.states ?? [] });
      } catch (error) {
        processStates.set(key, { states: [], error: message(error) });
      }
    }));
    if (generation === loadGeneration) renderWorkflowSettings({ director: false });
  } catch (error) {
    if (generation !== loadGeneration) return;
    $('#workflow-settings-progress').textContent = '';
    $('#workflow-settings-problem').hidden = false;
    $('#workflow-settings-problem').textContent = message(error);
  }
}

/** The projects that get ready states: every workflow's, and any that already has states. */
function settingsProjects() {
  const projects = new Map();
  for (const entry of [...(live?.map?.workflows ?? []), ...(workflowSettings?.settings?.ready_states ?? []), ...(form?.ready_states ?? []),
    ...(workflowSettings?.settings?.project_directors ?? [])]) {
    const key = projectKey(entry.organization, entry.project);
    if (!projects.has(key)) projects.set(key, { organization: entry.organization, project: entry.project });
  }
  return [...projects.values()].map(project => ({ ...project, ...processStates.get(projectKey(project.organization, project.project)) }));
}
function ownDirectorFor(organization, project) {
  return (form?.project_directors ?? []).find(p => projectKey(p.organization, p.project) === projectKey(organization, project));
}
/** Where a project's director comes from, as the host resolved it for the saved settings. */
const SOURCE_TEXT = {
  project: () => t('its own director'),
  workflow: () => t('the machine and agent of its workflows'),
  global: () => t('the general director'),
};
function resolvedDirector(organization, project) {
  return (workflowSettings?.directors ?? []).find(d => projectKey(d.organization, d.project) === projectKey(organization, project));
}
function machineName(id) {
  return machines.find(m => m.id === id)?.name ?? id;
}
/** The line that says which director classifies and summarizes a project's items, and why. */
function directorLine(project) {
  const resolved = resolvedDirector(project.organization, project.project);
  if (!resolved) return '';
  if (!resolved.director) return t('No director: its workflows differ in machine or agent and there is no general director. Choose one for this project or below.');
  const d = resolved.director;
  return t('Director: {agent} on {machine}, from {source}.', { agent: [providerNames[d.provider] || d.provider, d.model, effortNames[d.effort] || d.effort].join(' · '),
    machine: machineName(d.machine), source: SOURCE_TEXT[resolved.source]?.() ?? resolved.source });
}

/**
 * A machine and its CLI, model and effort for one director (`director` is edited in place); an
 * empty machine means none. `touched` runs when the person changes it.
 * @param {{ machine: string, provider: string, model: string, effort: string }} director
 * @param {{ editable: boolean, empty: string, touched: () => void,
 *   ids?: { machine?: string, provider?: string, model?: string, effort?: string } }} options
 */
function directorPicker(director, { editable, empty, ids = {}, touched }) {
  const machine = el('select', { id: ids.machine, disabled: !editable }, el('option', { value: '', textContent: empty }),
    ...machines.map(m => el('option', { value: m.id, textContent: `${m.name} · ${m.os}${m.online ? '' : ` · ${t('Offline')}`}` })));
  machine.value = director.machine ?? '';
  const selects = {
    provider: el('select', { id: ids.provider, disabled: !editable }),
    model: el('select', { id: ids.model, disabled: !editable }),
    effort: el('select', { id: ids.effort, disabled: !editable }),
  };
  const note = el('p', { class: 'hint', role: 'status' });
  const fill = providers => fillAgentChoices(selects, providers, { ...director }, (agent, byUser) => {
    Object.assign(director, agent);
    if (byUser) touched();
  });
  const load = async () => {
    const id = director.machine;
    if (!id) { note.textContent = ''; fill([]); return; }
    note.textContent = t('Loading…');
    try {
      let providers = agentCatalogs.get(id);
      if (!providers) { providers = (await invoke('implementation_catalog', { machine: id })).providers ?? []; agentCatalogs.set(id, providers); }
      if (director.machine !== id) return;
      note.textContent = '';
      fill(providers);
    } catch (error) {
      if (director.machine !== id) return;
      note.textContent = t('The machine\'s coding agents could not be read ({reason}); the saved choice is checked again when you save.', { reason: message(error) });
      fill([]);
    }
  };
  machine.onchange = () => { director.machine = machine.value; touched(); load(); };
  load();
  return [el('label', {}, t('Machine'), ' ', el('small', { textContent: t('Where the director\'s CLI runs.') }), machine),
    el('label', {}, t('CLI'), selects.provider), el('label', {}, t('Model'), selects.model), el('label', {}, t('Effort'), selects.effort), note];
}

/** A project's own director: a fold with its machine and agent; none falls back to its workflows'. */
function projectDirector(project, editable) {
  const key = projectKey(project.organization, project.project);
  let entry = ownDirectorFor(project.organization, project.project);
  const director = entry?.director ? { ...entry.director } : { machine: '', provider: '', model: '', effort: '' };
  const touched = () => {
    entry = ownDirectorFor(project.organization, project.project);
    form.project_directors = form.project_directors.filter(p => p !== entry);
    if (director.machine) form.project_directors.push({ organization: project.organization, project: project.project, director });
    projectDirectorsTouched.add(key);
    edited();
  };
  const fold = el('details', { class: 'fold', open: Boolean(entry) || projectDirectorsTouched.has(key) },
    el('summary', { textContent: t('Director for this project') }),
    el('p', { class: 'hint', textContent: t('Without one, the director runs on the machine and with the agent of this project\'s workflows when they share them, or else the general director below.') }),
    el('div', { class: 'grid' }, ...directorPicker(director, { editable, empty: t('Its workflows\' or the general one'), touched })));
  return [el('p', { class: 'hint workflow-director-line', textContent: directorLine(project) }), fold];
}

function readyFor(organization, project) {
  return (form?.ready_states ?? []).find(r => projectKey(r.organization, r.project) === projectKey(organization, project));
}

function renderWorkflowSettings({ director = true } = {}) {
  if (!form) return;
  const editable = workflowSettings.editable !== false;
  const list = $('#workflow-ready-states');
  const projects = settingsProjects();
  list.replaceChildren(...projects.map(project => {
    const entry = readyFor(project.organization, project.project);
    const chosen = new Set(entry?.states ?? []);
    const states = [...new Set([...(project.states ?? []), ...chosen])];
    const choices = el('div', { class: 'choice-list' }, ...states.map(state => {
      const box = el('input', { type: 'checkbox', checked: chosen.has(state), disabled: !editable });
      box.onchange = () => {
        let item = readyFor(project.organization, project.project);
        if (!item) { item = { organization: project.organization, project: project.project, states: [] }; form.ready_states.push(item); }
        item.states = box.checked ? [...new Set([...item.states, state])] : item.states.filter(s => s !== state);
        edited();
      };
      const known = !project.states || project.states.includes(state);
      return el('label', { class: 'check' }, box, el('span', {}, state, known ? null : el('small', { textContent: ` · ${t('no longer in the process')}` })));
    }));
    return el('fieldset', { class: 'workflow-ready', 'data-project': project.project },
      el('legend', {}, el('strong', { textContent: project.project }), project.organization ? el('small', { textContent: ` · ${project.organization.replace(/^https:\/\/dev\.azure\.com\//, '')}` }) : null),
      states.length ? choices : el('p', { class: 'hint', textContent: project.error ? tx(project.error)
        : project.states ? t('The states of this project\'s process could not be read; refresh once the Azure DevOps source can read it.') : t('Reading the states of this project\'s process…') }),
      chosen.size ? null : el('p', { class: 'hint', textContent: t('No ready state: nothing in this project starts by itself.') }),
      ...projectDirector(project, editable));
  }));
  if (!projects.length) list.append(el('p', { class: 'hint', textContent: t('No workflows yet. Each project a workflow names gets its ready states here.') }));

  for (const [id, field] of FIELDS) {
    const [min, max] = range(field);
    const input = $(id);
    input.value = String(form[field] ?? '');
    input.min = String(min); input.max = String(max);
    input.disabled = !editable;
    input.oninput = () => { form[field] = Number(input.value); edited(); };
  }
  renderWatcher(workflowSettings.watcher);
  if (director) renderDirector(editable);
}

/** What the watcher on this computer is doing and its last poll since the host started. */
function renderWatcher(watcher) {
  const box = $('#workflow-watcher-state');
  if (!watcher) { box.hidden = true; return; }
  const when = seconds => new Date(seconds * 1000).toLocaleTimeString(uiLocale(), { timeStyle: 'short' });
  const lines = [];
  if (watcher.state === 'unavailable') lines.push(t('The watcher runs only on your coordinator: this computer, a computer signed in to it and a shared account never start runs by themselves.'));
  else if (watcher.state === 'idle') lines.push(t('The watcher is idle: choose a ready state for a project to start it.'));
  else if (watcher.state === 'waiting_for_director') lines.push(t('The watcher waits for a director: several workflows of {projects} start by themselves with different machines or agents, and it needs one to choose among them.', { projects: (watcher.waiting_projects ?? []).join(', ') }));
  else lines.push(t('The watcher checks every {n} minutes.', { n: watcher.interval_minutes }));
  const poll = watcher.last_poll;
  if (watcher.state === 'watching' || watcher.state === 'waiting_for_director') {
    if (!poll) lines.push(t('No check yet since the host started.'));
    else {
      lines.push(poll.started
        ? tn(poll.started, 'Last check at {time}: 1 run started.', 'Last check at {time}: {n} runs started.', { time: when(poll.at) })
        : t('Last check at {time}: nothing new.', { time: when(poll.at) }));
      const seen = poll.seen_by_category;
      if (seen && (seen.requirement || seen.bug || seen.task)) {
        lines.push(t('Items seen in that check: HUs {hu}, bugs {bug}, tasks {task}.', { hu: seen.requirement ?? 0, bug: seen.bug ?? 0, task: seen.task ?? 0 }));
      }
      if (poll.partial) lines.push(t('Part of what it reads was not covered in that check; it continues at the next one.'));
      for (const project of poll.projects ?? []) {
        if (project.skipped && !project.waiting_for_director) lines.push(`${project.project}: ${tx(project.skipped)}`);
        for (const error of project.errors ?? []) lines.push(`${project.project}: ${tx(error)}`);
      }
    }
  }
  box.hidden = false;
  box.replaceChildren(...lines.map(text => el('p', { class: 'hint', textContent: text })));
}

function renderDirector(editable) {
  const director = form.director ??= { machine: '', provider: '', model: '', effort: '' };
  $('#workflow-director').replaceChildren(...directorPicker(director, { editable, empty: t('No director yet'),
    ids: { machine: 'workflow-director-machine', provider: 'workflow-director-provider', model: 'workflow-director-model', effort: 'workflow-director-effort' },
    touched: () => { directorTouched = true; edited(); } }));
}

/** What would be refused, checked before the save bar writes anything; it opens the category. */
function checkWorkflowSettings() {
  if (!pending || !form) return;
  const wrong = FIELDS.map(([, field]) => field).filter(field => {
    const [min, max] = range(field);
    return !Number.isInteger(form[field]) || form[field] < min || form[field] > max;
  }).map(field => { const [min, max] = range(field); return RANGE_TEXT[field]({ min, max }); });
  if (!wrong.length) return;
  showTab('settings', 'panel-workflow-watcher');
  $('#workflow-settings-problem').hidden = false;
  $('#workflow-settings-problem').textContent = wrong.join(' ');
  throw wrong.join(' ');
}

/**
 * Saves the category's pending edits (the save bar, after the profile): only what changed, each
 * changed project's ready states replacing that project's alone, `director: null` to clear it.
 */
async function saveWorkflowSettings() {
  if (!pending || !form) return;
  const problem = $('#workflow-settings-problem');
  const warnings = $('#workflow-settings-warnings');
  problem.hidden = true; warnings.hidden = true;
  checkWorkflowSettings();
  const saved = workflowSettings.settings;
  const args = {};
  for (const [, field] of FIELDS) if (form[field] !== saved[field]) args[field] = form[field];
  if (directorTouched) {
    const director = form.director?.machine ? { machine: form.director.machine, provider: form.director.provider, model: form.director.model, effort: form.director.effort } : null;
    if (JSON.stringify(director) !== JSON.stringify(saved.director ?? null)) args.director = director;
  }
  const sorted = list => JSON.stringify([...(list ?? [])].sort());
  const ready = settingsProjects().map(p => ({ organization: p.organization, project: p.project, states: readyFor(p.organization, p.project)?.states ?? [] }))
    .filter(change => sorted(change.states) !== sorted(saved.ready_states?.find(r => projectKey(r.organization, r.project) === projectKey(change.organization, change.project))?.states));
  if (ready.length) args.ready_states = ready;
  const projectDirectors = settingsProjects().filter(p => projectDirectorsTouched.has(projectKey(p.organization, p.project))).map(p => {
    const own = ownDirectorFor(p.organization, p.project)?.director;
    const director = own?.machine ? { machine: own.machine, provider: own.provider, model: own.model, effort: own.effort } : null;
    const stored = saved.project_directors?.find(s => projectKey(s.organization, s.project) === projectKey(p.organization, p.project))?.director ?? null;
    return JSON.stringify(director) === JSON.stringify(stored) ? null : { organization: p.organization, project: p.project, director };
  }).filter(Boolean);
  if (projectDirectors.length) args.project_directors = projectDirectors;
  if (Object.keys(args).length) {
    try {
      const reply = await invoke('workflow_settings_update', args);
      if (reply.warnings?.length) { warnings.hidden = false; warnings.replaceChildren(...reply.warnings.map(text => el('p', { textContent: tx(text) }))); }
    } catch (error) {
      problem.hidden = false;
      problem.textContent = message(error);
      showTab('settings', 'panel-workflow-ready');
      throw error;
    }
  }
  pending = false;
  await readWorkflowSettings();
}

/** Drops the category's pending edits (Discard, or a reload after saving). */
function discardWorkflowSettings() {
  pending = false;
  if (!workflowSettings) return;
  resetForm();
  if (machines.length || $('#workflow-ready-states').childElementCount) renderWorkflowSettings();
}

/** Reads the saved settings once for Home, which shows whether a project lacks its ready state. */
let requested = false;
function ensureWorkflowSettings() {
  if (requested || workflowSettings) return;
  requested = true;
  readWorkflowSettings().then(() => renderLive(), () => {});
}

/**
 * Projects of enabled workflows that start by themselves and have no ready state, or null while
 * the settings are unknown.
 */
function projectsWithoutReadyState() {
  if (!workflowSettings) return null;
  const saved = workflowSettings.settings?.ready_states ?? [];
  const missing = new Set();
  for (const workflow of live?.map?.workflows ?? []) {
    if (!workflow.enabled || !workflow.auto_start) continue;
    const entry = saved.find(r => projectKey(r.organization, r.project) === projectKey(workflow.organization, workflow.project));
    if (!entry?.states?.length) missing.add(workflow.project);
  }
  return [...missing];
}

/** Projects whose several workflows that start by themselves have no director to choose, or null while unknown. */
function projectsWaitingForDirector() {
  return workflowSettings ? (workflowSettings.watcher?.waiting_projects ?? []) : null;
}

$('#workflow-settings-refresh').onclick = () => busy($('#workflow-settings-refresh'), loadWorkflowSettings);
$('#workflow-director-prompt').onclick = () => showAgentPrompt('direct');

export {
  workflowSettings, loadWorkflowSettings, ensureWorkflowSettings, projectsWithoutReadyState, projectsWaitingForDirector, checkWorkflowSettings,
  saveWorkflowSettings, discardWorkflowSettings,
};
