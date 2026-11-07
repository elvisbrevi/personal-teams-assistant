// Settings › Workflows: the ready states of each project, the watcher's interval, how many runs
// advance at once, the pull-request wait and the workflow director (`workflow_settings`,
// `workflow_settings_update`). Its edits are unsaved Settings edits like any other category's:
// the save bar saves them (`saveWorkflowSettings`) after the profile, and Discard drops them.
import { t, tx } from './i18n.js';
import { invoke } from './control.js';
import { $, el } from './dom.js';
import { live } from './state.js';
import { busy } from './notices.js';
import { markDirty, showTab } from './nav.js';
import { renderLive } from './home.js';
import { fillAgentChoices } from './settings-models.js';
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
  directorTouched = false;
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
  for (const entry of [...(live?.map?.workflows ?? []), ...(workflowSettings?.settings?.ready_states ?? []), ...(form?.ready_states ?? [])]) {
    const key = projectKey(entry.organization, entry.project);
    if (!projects.has(key)) projects.set(key, { organization: entry.organization, project: entry.project });
  }
  return [...projects.values()].map(project => ({ ...project, ...processStates.get(projectKey(project.organization, project.project)) }));
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
      chosen.size ? null : el('p', { class: 'hint', textContent: t('No ready state: nothing in this project starts by itself.') }));
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
  if (director) renderDirector(editable);
}

function renderDirector(editable) {
  const director = form.director ??= { machine: '', provider: '', model: '', effort: '' };
  const machine = el('select', { id: 'workflow-director-machine', disabled: !editable }, el('option', { value: '', textContent: t('No director yet') }),
    ...machines.map(m => el('option', { value: m.id, textContent: `${m.name} · ${m.os}${m.online ? '' : ` · ${t('Offline')}`}` })));
  machine.value = director.machine ?? '';
  const selects = {
    provider: el('select', { id: 'workflow-director-provider', disabled: !editable }),
    model: el('select', { id: 'workflow-director-model', disabled: !editable }),
    effort: el('select', { id: 'workflow-director-effort', disabled: !editable }),
  };
  const note = el('p', { class: 'hint', role: 'status' });
  const fill = providers => fillAgentChoices(selects, providers, { ...director }, (agent, byUser) => {
    Object.assign(director, agent);
    if (byUser) { directorTouched = true; edited(); }
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
  machine.onchange = () => { director.machine = machine.value; directorTouched = true; edited(); load(); };
  $('#workflow-director').replaceChildren(
    el('label', {}, t('Machine'), ' ', el('small', { textContent: t('Where the director\'s CLI runs.') }), machine),
    el('label', {}, t('CLI'), selects.provider), el('label', {}, t('Model'), selects.model), el('label', {}, t('Effort'), selects.effort), note);
  load();
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

$('#workflow-settings-refresh').onclick = () => busy($('#workflow-settings-refresh'), loadWorkflowSettings);
$('#workflow-director-prompt').onclick = () => showAgentPrompt('direct');

export {
  workflowSettings, loadWorkflowSettings, ensureWorkflowSettings, projectsWithoutReadyState, checkWorkflowSettings,
  saveWorkflowSettings, discardWorkflowSettings,
};
