// Settings › Workflows: the ready states of each project, the watcher's interval, how many runs
// advance at once, the pull-request wait and the workflow director (`workflow_settings`,
// `workflow_settings_update`). Saved with its own button, on the account's coordinator.
import { t, tx } from './i18n.js';
import { invoke } from './control.js';
import { $, el } from './dom.js';
import { live } from './state.js';
import { busy, done } from './notices.js';
import { applying } from './profile.js';
import { providerNames, renderLive } from './home.js';
import { effortNames } from './settings-models.js';
import { showAgentPrompt } from './guides.js';

/** What the host last answered, the form's copy of it, and the account's machines. */
let workflowSettings = null;
let form = null;
let machines = [];
const agentCatalogs = new Map();
let loadGeneration = 0;

const message = error => tx(String(error?.message ?? error));
const projectKey = (organization, project) => `${(organization ?? '').toLowerCase()}|${(project ?? '').toLowerCase()}`;

async function loadWorkflowSettings() {
  const generation = ++loadGeneration;
  $('#workflow-settings-progress').textContent = t('Loading…');
  $('#workflow-settings-problem').hidden = true;
  try {
    const [reply, fleet] = await Promise.all([invoke('workflow_settings'), invoke('machine_list').catch(() => ({ machines: [] }))]);
    if (generation !== loadGeneration) return;
    workflowSettings = reply;
    machines = fleet.machines ?? [];
    form = structuredClone(reply.settings);
    $('#workflow-settings-progress').textContent = '';
    renderWorkflowSettings();
    renderLive();
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
  for (const project of workflowSettings?.projects ?? []) projects.set(projectKey(project.organization, project.project), { ...project });
  for (const workflow of live?.map?.workflows ?? []) {
    const key = projectKey(workflow.organization, workflow.project);
    if (!projects.has(key)) projects.set(key, { organization: workflow.organization, project: workflow.project, states: [] });
  }
  for (const entry of form?.ready_states ?? []) {
    const key = projectKey(entry.organization, entry.project);
    if (!projects.has(key)) projects.set(key, { organization: entry.organization, project: entry.project, states: [] });
  }
  return [...projects.values()];
}
function readyFor(organization, project) {
  return (form?.ready_states ?? []).find(r => projectKey(r.organization, r.project) === projectKey(organization, project));
}

function renderWorkflowSettings() {
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
        const ready = form.ready_states ??= [];
        let item = readyFor(project.organization, project.project);
        if (!item) { item = { organization: project.organization, project: project.project, states: [] }; ready.push(item); }
        item.states = box.checked ? [...new Set([...item.states, state])] : item.states.filter(s => s !== state);
        if (!item.states.length) form.ready_states = ready.filter(r => r !== item);
      };
      const known = (project.states ?? []).includes(state);
      return el('label', { class: 'check' }, box, el('span', {}, state, known ? null : el('small', { textContent: ` · ${t('no longer in the process')}` })));
    }));
    return el('fieldset', { class: 'workflow-ready', 'data-project': project.project },
      el('legend', {}, el('strong', { textContent: project.project }), project.organization ? el('small', { textContent: ` · ${project.organization.replace(/^https:\/\/dev\.azure\.com\//, '')}` }) : null),
      states.length ? choices : el('p', { class: 'hint', textContent: project.error ? tx(project.error) : t('The states of this project\'s process could not be read; refresh once the Azure DevOps source can read it.') }),
      chosen.size ? null : el('p', { class: 'hint', textContent: t('No ready state: nothing in this project starts by itself.') }));
  }));
  if (!projects.length) list.append(el('p', { class: 'hint', textContent: t('No workflows yet. Each project a workflow names gets its ready states here.') }));

  for (const [id, field, min, max] of [['#workflow-interval', 'watch_interval_minutes', 1, 60], ['#workflow-concurrency', 'concurrency', 1, 4], ['#workflow-pr-wait', 'pull_request_wait_days', 1, 60]]) {
    const input = $(id);
    input.value = String(form[field] ?? '');
    input.min = String(min); input.max = String(max);
    input.disabled = !editable;
    input.oninput = () => { form[field] = Number(input.value); };
  }
  renderDirector(editable);
  $('#workflow-settings-save').hidden = !editable;
}

function renderDirector(editable) {
  const director = form.director ??= { machine: '', provider: '', model: '', effort: '' };
  const machine = el('select', { id: 'workflow-director-machine', disabled: !editable }, el('option', { value: '', textContent: t('Choose a machine') }),
    ...machines.map(m => el('option', { value: m.id, textContent: `${m.name} · ${m.os}${m.online ? '' : ` · ${t('Offline')}`}` })));
  machine.value = director.machine ?? '';
  const provider = el('select', { id: 'workflow-director-provider', disabled: !editable });
  const model = el('select', { id: 'workflow-director-model', disabled: !editable });
  const effort = el('select', { id: 'workflow-director-effort', disabled: !editable });
  const note = el('p', { class: 'hint', role: 'status' });
  const fill = providers => {
    const list = providers.length ? providers
      : director.provider ? [{ id: director.provider, models: [{ id: director.model, name: director.model, efforts: [director.effort] }] }] : [];
    provider.replaceChildren(...list.map(p => el('option', { value: p.id, textContent: providerNames[p.id] || p.id })));
    if (!list.length) provider.append(el('option', { value: '', textContent: t('No coding CLI installed on this machine') }));
    provider.value = list.some(p => p.id === director.provider) ? director.provider : list[0]?.id ?? '';
    const efforts = () => {
      const info = list.find(p => p.id === provider.value)?.models?.find(m => m.id === model.value);
      effort.replaceChildren(...(info?.efforts ?? []).map(value => el('option', { value, textContent: effortNames[value] || value })));
      effort.value = info?.efforts?.includes(director.effort) ? director.effort : info?.default_effort ?? info?.efforts?.[0] ?? '';
      Object.assign(director, { provider: provider.value, model: model.value, effort: effort.value });
    };
    const models = () => {
      const info = list.find(p => p.id === provider.value);
      model.replaceChildren(...(info?.models ?? []).map(m => el('option', { value: m.id, textContent: m.name || m.id })));
      model.value = info?.models?.some(m => m.id === director.model) ? director.model : info?.models?.[0]?.id ?? '';
      efforts();
    };
    provider.onchange = () => { director.provider = provider.value; models(); };
    model.onchange = () => { director.model = model.value; efforts(); };
    effort.onchange = () => { director.effort = effort.value; };
    models();
  };
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
  machine.onchange = () => { director.machine = machine.value; load(); };
  $('#workflow-director').replaceChildren(
    el('label', {}, t('Machine'), el('small', { textContent: t('Where the director\'s CLI runs.') }), machine),
    el('label', {}, t('CLI'), provider), el('label', {}, t('Model'), model), el('label', {}, t('Effort'), effort), note);
  load();
}

/** Reads the settings once for Home, which shows whether a project lacks its ready state. */
let requested = false;
function ensureWorkflowSettings() {
  if (requested || workflowSettings) return;
  requested = true;
  loadWorkflowSettings();
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
$('#workflow-settings-save').onclick = () => busy($('#workflow-settings-save'), async () => {
  $('#workflow-settings-problem').hidden = true;
  const ranges = [['watch_interval_minutes', 1, 60, t('Check every 1 to 60 minutes.')], ['concurrency', 1, 4, t('Let 1 to 4 runs advance at once.')],
    ['pull_request_wait_days', 1, 60, t('Wait 1 to 60 days for pull requests.')]];
  const wrong = ranges.filter(([field, min, max]) => !Number.isInteger(form[field]) || form[field] < min || form[field] > max).map(([, , , text]) => text);
  if (wrong.length) { $('#workflow-settings-problem').hidden = false; $('#workflow-settings-problem').textContent = wrong.join(' '); return; }
  try {
    await applying(() => invoke('workflow_settings_update', { settings: form }));
    done(t('Workflow settings saved.'));
    await loadWorkflowSettings();
  } catch (error) {
    $('#workflow-settings-problem').hidden = false;
    $('#workflow-settings-problem').textContent = message(error);
  }
});

export { workflowSettings, loadWorkflowSettings, ensureWorkflowSettings, projectsWithoutReadyState };
