// To-Do › Implement: the implementation form, its machine and directory choices, and its runs.
import { t, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el } from './dom.js';
import { current, setTab, tab } from './state.js';
import { busy, done, fail } from './notices.js';
import { renderSavebar, showTab } from './nav.js';
import { link } from './markdown.js';
import { loadWorkItemSessions, openAgentSession } from './sessions.js';
import { chooseDirectories } from './directory-sheet.js';
import { providerNames } from './home.js';
import { effortNames } from './settings-models.js';
import { runStateLabel as runState } from './run-states.js';
import { openWorkflowRun } from './workflow-runs.js';

let implementationForm = null;
let implementationRequest = 0;

/** The organization name of an Azure DevOps address (work item link or `https://dev.azure.com/NAME`). */
function organizationOf(address) {
  try {
    const url = new URL(address);
    if (url.hostname === 'dev.azure.com') return url.pathname.split('/').filter(Boolean)[0]?.toLowerCase();
    return url.hostname.endsWith('.visualstudio.com') ? url.hostname.split('.')[0].toLowerCase() : undefined;
  } catch { return undefined; }
}
/** The enabled workflows of the item's project (Azure items only): what the Workflow select offers. */
function projectWorkflows(prepared, selected) {
  if (selected.provider !== 'azure') return [];
  const item = prepared.detail.item;
  return (current?.map?.workflows ?? []).filter(w => w.enabled && organizationOf(w.organization) === organizationOf(item.url) && w.project.toLowerCase() === item.project.toLowerCase());
}
function implementationSelection(item, selection) {
  const provider = item.url.startsWith('https://github.com/') ? 'github' : 'azure';
  return { provider, id: item.id, project: item.project, source: provider === 'azure' ? selection?.source || $('#todo-source').value || undefined : undefined,
    task: provider === 'azure' && (selection?.task ?? item.category === 'task') };
}
async function openImplementation(item, selection, trigger) {
  const request = ++implementationRequest;
  const previous = { tab, scroll: window.scrollY, trigger };
  implementationForm = null;
  showTab('implementation');
  $('#implementation-item').textContent = `#${item.id} · ${item.title}`;
  $('#implementation-problem').hidden = true;
  $('#implementation-content').replaceChildren(el('p', { class: 'hint', textContent: t('Loading…') }));
  $('#implementation h1').focus();
  const selected = implementationSelection(item, selection);
  try {
    const [prepared, fleet] = await Promise.all([invoke('implementation_prepare', { item: selected }), invoke('machine_list')]);
    if (request !== implementationRequest) return;
    const machine = el('select', { id: 'implementation-machine' }, ...fleet.machines.map(m => el('option', { value: m.id, disabled: !m.online, textContent: `${m.name} · ${m.os}${m.online ? '' : ` · ${t('Offline')}`}` })));
    machine.value = fleet.machines.find(m => m.id === fleet.this_machine && m.online)?.id || fleet.machines.find(m => m.online)?.id || '';
    const context = el('textarea', { id: 'implementation-context', rows: 5, maxLength: 32000 });
    const provider = el('select', { id: 'implementation-provider' });
    const model = el('select', { id: 'implementation-model' });
    const effort = el('select', { id: 'implementation-effort' });
    const directoryList = el('div', { class: 'implementation-paths' });
    const add = el('button', { type: 'button', class: 'secondary', textContent: t('+ Choose directories') });
    const cancel = el('button', { type: 'button', class: 'secondary', textContent: t('Cancel') });
    const start = el('button', { type: 'button', textContent: t('Start'), disabled: true });
    const runs = el('div', { id: 'implementation-runs' });
    const eligible = projectWorkflows(prepared, selected);
    const workflow = el('select', { id: 'implementation-workflow' }, el('option', { value: 'none', textContent: t('No workflow') }),
      el('option', { value: 'director', textContent: t('The director chooses') }), ...eligible.map(w => el('option', { value: w.id, textContent: w.name })));
    const workflowNote = el('p', { class: 'hint', id: 'implementation-workflow-note', role: 'status', hidden: true });
    const workflowRun = el('div', { id: 'implementation-workflow-run', 'aria-live': 'polite' });
    const form = implementationForm = { request, previous, selected, prepared, machine, context, provider, model, effort, directoryList, start, runs, directories: [], catalog: [], generation: 0, creations: prepared.creations || {},
      workflow, workflowNote, workflowRun, eligible, workflowMode: 'none', workflowId: null, workflowGeneration: 0, add };
    const leave = () => {
      ++implementationRequest; implementationForm = null;
      $('#implementation').hidden = true;
      setTab(previous.tab); $(`#${tab}`).hidden = false;
      for (const button of document.querySelectorAll('nav button[data-tab]')) button.setAttribute('aria-current', String(button.dataset.tab === tab ? 'page' : 'false'));
      previous.trigger?.focus({ preventScroll: true }); window.scrollTo(0, previous.scroll); renderSavebar(); loadWorkItemSessions().catch(fail);
    };
    cancel.onclick = leave;
    add.onclick = () => chooseDirectories({ trigger: add, title: t('Directories on {machine}', { machine: machine.selectedOptions[0].textContent }), selected: form.directories,
      browse: path => invoke('implementation_browse', { machine: machine.value, path }),
      apply: paths => { if (implementationForm === form) { form.directories = paths; implementationDirectories(form); } } });
    provider.onchange = () => implementationModels(form);
    model.onchange = () => implementationEfforts(form);
    effort.onchange = () => implementationReady(form);
    machine.onchange = () => busy(machine, () => implementationMachine(form));
    workflow.onchange = () => chooseWorkflow(form).catch(fail);
    start.onclick = () => busy(start, async () => {
      if (form.workflowId) return startWorkflowRun(form);
      const result = await invoke('implementation_start', { machine: machine.value, item: selected, context: context.value, directories: form.directories, provider: provider.value, model: model.value, effort: effort.value });
      if (implementationForm !== form) return;
      done(t('Implementation run started.'));
      await implementationRuns(form);
    });
    $('#implementation-content').replaceChildren(el('div', { class: 'panel' },
      el('h2', { textContent: prepared.planning ? t('Plan child work items') : t('Implement selected work item') }),
      el('p', { class: 'hint', textContent: prepared.planning ? t('Review and edit the proposed children before creating them. Existing children are included as context.') : t('The selected coding agent will work in the directories you choose.') }),
      prepared.planning ? null : el('p', { class: 'callout warn', id: 'implementation-permissions', textContent: t('The agent runs with full permissions on that computer: it runs commands, commits, pushes and opens pull requests there without asking anyone.') }),
      eligible.length ? el('label', {}, t('Workflow'), workflow) : null,
      workflowNote,
      el('label', {}, t('Implementation context'), context),
      el('label', {}, t('Execution machine'), machine),
      el('fieldset', {}, el('legend', { textContent: t('Working directories') }), directoryList, add),
      el('div', { class: 'grid' }, el('label', {}, t('Provider'), provider), el('label', {}, t('Model'), model), el('label', {}, t('Effort'), effort))),
      el('div', { class: 'actions implementation-footer' }, cancel, start),
      workflowRun,
      el('section', { class: 'panel' }, el('h2', { textContent: t('Implementation runs') }), runs));
    await implementationMachine(form);
  } catch (error) {
    if (request !== implementationRequest) return;
    $('#implementation-problem').hidden = false;
    $('#implementation-problem').textContent = tx(String(error));
    const back = el('button', { type: 'button', class: 'secondary', textContent: t('Back') });
    back.onclick = () => showTab(previous.tab);
    $('#implementation-content').replaceChildren(back);
  }
}
function implementationReady(form) {
  if (form.purpose === 'request') {
    form.start.disabled = form.writeReady === false || form.submitted || form.start.classList.contains('busy') || !(form.context.value.trim() && form.target.value && form.provider.value && form.model.value && form.effort.value);
    return;
  }
  // With a workflow the machine, directories and agent are the workflow's own.
  if (form.workflowMode && form.workflowMode !== 'none') {
    // A started run is not started twice: Start waits for another choice.
    form.start.disabled = form.start.classList.contains('busy') || !form.workflowId || form.workflowStarted;
    return;
  }
  form.start.disabled = form.start.classList.contains('busy') || !(form.machine.value && form.directories.length && form.provider.value && form.model.value && form.effort.value);
}
/** Locks or unlocks what a workflow supplies: the machine, directories, agent, model, effort and context. */
function lockFields(form, locked) {
  for (const field of [form.machine, form.provider, form.model, form.effort, form.context, form.add]) if (field) field.disabled = locked;
  form.directoryList.classList.toggle('locked', locked);
}
/** Fills the locked fields with the workflow's own machine, checkouts and agent. */
function fillFromWorkflow(form, workflow) {
  // A machine's catalog still on its way must not replace what the workflow supplies.
  ++form.generation;
  const known = [...form.machine.options].some(o => o.value === workflow.machine);
  if (!known) form.machine.append(el('option', { value: workflow.machine, textContent: t('Unknown machine') }));
  form.machine.value = workflow.machine;
  form.directories = (workflow.repositories ?? []).map(r => workflow.checkouts?.[r.id.toLowerCase()] ?? workflow.checkouts?.[r.id]).filter(Boolean);
  implementationDirectories(form);
  const agent = workflow.agent ?? {};
  form.provider.replaceChildren(el('option', { value: agent.provider, textContent: providerNames[agent.provider] || agent.provider }));
  form.model.replaceChildren(el('option', { value: agent.model, textContent: agent.model }));
  form.effort.replaceChildren(el('option', { value: agent.effort, textContent: effortNames[agent.effort] || agent.effort }));
  form.context.value = '';
  form.workflowNote.hidden = false;
  form.workflowNote.textContent = t('This run takes its machine, directories, agent, model, effort and instructions from the workflow {name}. To choose them yourself, start without a workflow.', { name: workflow.name });
}
/** What the person chose before a workflow took over, to give back with «No workflow». */
function rememberChoices(form) {
  form.remembered = { machine: form.machine.value, context: form.context.value, directories: [...form.directories],
    provider: form.provider.value, model: form.model.value, effort: form.effort.value };
}
async function restoreChoices(form) {
  const saved = form.remembered; form.remembered = null;
  if (saved && [...form.machine.options].some(o => o.value === saved.machine)) form.machine.value = saved.machine;
  await busy(form.machine, () => implementationMachine(form));
  if (!saved || implementationForm !== form || form.workflowMode !== 'none') return;
  form.context.value = saved.context;
  // The directories belong to the machine they were chosen on.
  if (form.machine.value === saved.machine) { form.directories = saved.directories; implementationDirectories(form); }
  if ([...form.provider.options].some(o => o.value === saved.provider)) {
    form.provider.value = saved.provider; implementationModels(form);
    if ([...form.model.options].some(o => o.value === saved.model)) {
      form.model.value = saved.model; implementationEfforts(form);
      if ([...form.effort.options].some(o => o.value === saved.effort)) form.effort.value = saved.effort;
    }
  }
  implementationReady(form);
}
/** The Workflow select changed: none, the director's classification, or a named workflow. */
async function chooseWorkflow(form) {
  const generation = ++form.workflowGeneration;
  const mode = form.workflow.value === 'none' || form.workflow.value === 'director' ? form.workflow.value : 'id';
  const was = form.workflowMode;
  form.workflowMode = mode; form.workflowId = null; form.workflowStarted = false;
  form.workflowRun.replaceChildren();
  form.workflowNote.hidden = true;
  if (mode === 'none') {
    lockFields(form, false);
    if (was !== 'none') await restoreChoices(form);
    implementationReady(form);
    return;
  }
  if (was === 'none') rememberChoices(form);
  // A machine's catalog still on its way must not replace the locked values.
  ++form.generation;
  lockFields(form, true);
  if (mode === 'id') {
    const workflow = form.eligible.find(w => w.id === form.workflow.value);
    form.workflowId = workflow.id;
    fillFromWorkflow(form, workflow);
    implementationReady(form);
    await implementationRuns(form);
    return;
  }
  // The director chooses: it classifies first and nothing starts until the person confirms.
  implementationReady(form);
  form.workflowNote.hidden = false;
  form.workflowNote.textContent = t('Asking the workflow director…');
  let choice;
  try {
    choice = await invoke('implementation_start', { item: form.selected, workflow: 'director' });
  } catch (error) {
    if (implementationForm === form && generation === form.workflowGeneration) form.workflowNote.textContent = tx(String(error?.message ?? error));
    return;
  }
  if (implementationForm !== form || generation !== form.workflowGeneration) return;
  const chosen = form.eligible.find(w => w.id === choice.workflow);
  const again = el('button', { type: 'button', class: 'secondary small', textContent: t('Ask again') });
  again.onclick = () => chooseWorkflow(form).catch(fail);
  if (chosen) {
    form.workflowId = chosen.id;
    fillFromWorkflow(form, chosen);
    form.workflowNote.hidden = true;
    form.workflowRun.replaceChildren(el('div', { class: 'callout info', id: 'implementation-workflow-choice' },
      el('p', {}, el('strong', { textContent: t('The director chose {name}', { name: chosen.name }) })), el('p', { textContent: choice.reason }), again));
    implementationReady(form);
    await implementationRuns(form);
  } else {
    form.workflowNote.hidden = true;
    form.workflowRun.replaceChildren(el('div', { class: 'callout warn', id: 'implementation-workflow-choice' },
      el('p', {}, el('strong', { textContent: t('The director chose no workflow') })), el('p', { textContent: choice.reason }),
      el('p', { class: 'hint', textContent: t('Choose a workflow yourself, or start without one.') }), again));
    implementationReady(form);
  }
}
/** Starts the chosen workflow's run and shows its state here; the Workflows section follows it. */
async function startWorkflowRun(form) {
  const id = form.workflowId;
  const result = await invoke('implementation_start', { item: form.selected, workflow: id });
  if (implementationForm !== form) return;
  done(t('Workflow run started.'));
  const workflow = form.eligible.find(w => w.id === id);
  const run = result.run;
  const state = el('strong', { textContent: runState(run.state) });
  const badgeBox = el('span', {}, badge(runState(run.state), 'info'));
  const refresh = el('button', { type: 'button', class: 'secondary small', textContent: t('Refresh') });
  const open = el('button', { type: 'button', class: 'secondary small', textContent: t('Open the run') });
  refresh.onclick = () => busy(refresh, async () => {
    const reply = await invoke('workflow_run_status', { run: run.id });
    const label = runState(reply.run.state);
    badgeBox.replaceChildren(badge(label, ['done'].includes(reply.run.state) ? 'ok' : ['failed', 'needs_attention', 'interrupted', 'unassigned'].includes(reply.run.state) ? 'warn' : 'info'));
    state.textContent = label;
  });
  open.onclick = () => openWorkflowRun(run.id, open);
  form.workflowRun.replaceChildren(el('div', { class: 'callout ok', id: 'implementation-workflow-started' },
    el('p', {}, badgeBox, ' ', el('strong', { textContent: workflow?.name ?? id })),
    el('p', { class: 'hint', textContent: t('The run goes on by itself: it plans the item, stops for your review of the plan and then codes each child. Follow it in Workflows.') }),
    el('div', { class: 'actions' }, refresh, open)));
  form.workflowStarted = true;
  implementationReady(form);
  await implementationRuns(form);
}
function implementationModels(form) {
  const info = form.catalog.find(p => p.id === form.provider.value);
  form.model.replaceChildren(...(info?.models || []).map(m => el('option', { value: m.id, textContent: m.name })));
  implementationEfforts(form);
}
function implementationEfforts(form) {
  const model = form.catalog.find(p => p.id === form.provider.value)?.models.find(m => m.id === form.model.value);
  form.effort.replaceChildren(...(model?.efforts || []).map(value => el('option', { value, textContent: effortNames[value] || value })));
  if (model?.default_effort) form.effort.value = model.default_effort;
  implementationReady(form);
}
function implementationDirectories(form) {
  form.directoryList.replaceChildren(...form.directories.map(path => {
    const remove = el('button', { type: 'button', class: 'secondary small', textContent: t('Remove'), 'aria-label': t('Remove directory {path}', { path }) });
    remove.onclick = () => { form.directories = form.directories.filter(p => p !== path); implementationDirectories(form); };
    return el('div', { class: 'implementation-path' }, el('code', { textContent: path }), form.workflowMode && form.workflowMode !== 'none' ? null : remove);
  }));
  implementationReady(form);
}
async function implementationMachine(form) {
  const generation = ++form.generation;
  form.directories = []; form.catalog = [];
  form.provider.replaceChildren(); implementationModels(form); implementationDirectories(form);
  const catalog = await invoke('implementation_catalog', { machine: form.machine.value });
  if (implementationForm !== form || generation !== form.generation) return;
  form.catalog = catalog.providers;
  form.provider.replaceChildren(...catalog.providers.map(p => el('option', { value: p.id, textContent: providerNames[p.id] || p.id })));
  if (!catalog.providers.length) form.provider.append(el('option', { value: '', textContent: t('No coding CLI installed on this machine') }));
  implementationModels(form);
  await implementationRuns(form);
}
/** The permission mode a run was launched with, as the execution machine recorded it. */
function permissionMode(mode) {
  if (!mode) return t('Not recorded');
  if (mode === 'read-only') return t('Read-only, no tools');
  return t('Full permissions on that computer ({mode})', { mode });
}
/**
 * The review of a planning run's proposed children, shared by Implement and a workflow run's plan-review gate:
 * each child editable and selectable (an Azure child also its type and the guide's proposed field values),
 * and one button that creates the selected ones only on a second press, through `create(chosen)`.
 */
function planChildrenReview(run, azure, create) {
  const children = run.plan.children.map(child => {
    const selected = el('input', { type: 'checkbox', checked: true });
    const title = el('input', { value: child.title, maxLength: 200 });
    const description = el('textarea', { value: child.description, rows: 3, maxLength: 16000 });
    const kind = el('input', { value: child.type ?? '', maxLength: 120, placeholder: t('Default task type'), translate: 'no', spellcheck: false });
    // Values the agent proposed from the work item guide; an emptied value is not written.
    const guide = () => (current?.map?.work_item_guides ?? []).find(g => g.provider === 'azure' && g.type.toLowerCase() === kind.value.trim().toLowerCase());
    const values = (child.fields ?? []).map(proposed => {
      const known = guide()?.fields?.find(f => f.reference === proposed.reference);
      const value = known?.values?.length
        ? el('select', {}, el('option', { value: '', textContent: t('Do not fill') }), ...[...new Set([...known.values, proposed.value])].map(v => el('option', { value: v, textContent: v, selected: v === proposed.value })))
        : el('input', { value: proposed.value, spellcheck: false });
      return { reference: proposed.reference, value, row: el('label', {}, el('span', {}, known?.name || proposed.reference, ' ', el('code', { textContent: proposed.reference })), value) };
    });
    const fieldset = el('fieldset', {}, el('legend', {}, el('label', { class: 'check' }, selected, t('Create this child'))), el('label', {}, t('Title'), title), el('label', {}, t('Description'), description),
      ...(azure ? [el('label', {}, t('Work item type'), kind), ...values.map(v => v.row)] : []));
    return { selected, title, description, kind, values, fieldset };
  });
  const button = el('button', { type: 'button', class: 'secondary', textContent: t('Create selected children') });
  const review = el('div', { class: 'plan-review' }, ...children.map(c => c.fieldset), button);
  let reviewed = false;
  review.addEventListener('input', () => { reviewed = false; button.textContent = t('Create selected children'); });
  button.onclick = () => busy(button, async () => {
    if (!reviewed) { reviewed = true; button.textContent = t('Confirm child creation'); return; }
    const chosen = children.filter(c => c.selected.checked).map(c => ({ title: c.title.value, description: c.description.value,
      ...(azure ? { type: c.kind.value.trim(), fields: c.values.map(v => ({ reference: v.reference, value: v.value.value.trim() })).filter(v => v.value) } : {}) }));
    if (!chosen.length) throw t('Select at least one child.');
    await create(chosen);
  });
  return review;
}
async function implementationRuns(form) {
  const machine = form.machine.value; const generation = form.generation;
  const result = await invoke('implementation_runs', { machine, work_item_url: form.prepared.detail.item.url });
  if (implementationForm !== form || generation !== form.generation) return;
  const runs = result.runs.filter(run => run.item.url === form.prepared.detail.item.url);
  await Promise.all(runs.filter(run => run.state === 'plan_ready' && (!form.creations[run.id] || form.creations[run.id].state === 'sending')).map(async run => {
    const record = await invoke('implementation_creation', { id: run.id });
    if (record) form.creations[run.id] = record;
  }));
  if (implementationForm !== form || generation !== form.generation) return;
  form.runs.replaceChildren(...runs.map(run => {
    const panel = el('article', { class: 'implementation-run' }, badge(tx(run.state), ['done', 'completed', 'created'].includes(run.state) ? 'ok' : ['permission_prompt', 'needs_attention', 'failed'].includes(run.state) ? 'warn' : 'info'), el('strong', { textContent: `${providerNames[run.provider] || run.provider || ''} · ${run.model || ''}` }),
      el('dl', { class: 'todo-fields' }, ...[[t('Requested at'), run.created_at], [t('Started at'), run.started_at], [t('Finished at'), run.finished_at]].map(([label, value]) => el('div', {}, el('dt', { textContent: label }), el('dd', { textContent: value ? new Date(value).toLocaleString(uiLocale()) : t('Not recorded') }))),
        el('div', {}, el('dt', { textContent: t('Permission mode') }), el('dd', { class: 'implementation-permission-mode', textContent: permissionMode(run.permission_mode) }))));
    if (run.prompts?.length) panel.append(el('div', { class: 'callout warn implementation-prompts' }, el('div', {},
      el('p', { textContent: t('The coding CLI still stopped on a permission prompt, which nobody answered. Remove its cause on that computer:') }),
      el('pre', { class: 'implementation-output', textContent: run.prompts.join('\n') }))));
    if (run.session_id) {
      const open = el('button', { type: 'button', class: 'secondary small', textContent: t('Open agent session') });
      open.onclick = () => openAgentSession({ ...run, machine }, open);
      panel.append(el('div', { class: 'todo-context' }, t('Agent session'), el('code', { textContent: run.session_id }), open));
    }
    if (['running', 'planning'].includes(run.state)) {
      const cancel = el('button', { type: 'button', class: 'secondary small', textContent: t('Stop run') });
      cancel.onclick = () => busy(cancel, async () => { await invoke('implementation_cancel', { machine, id: run.id }); await implementationRuns(form); });
      panel.append(cancel);
    }
    if (run.output) panel.append(el('pre', { class: 'implementation-output', textContent: tx(run.output) }));
    if (run.plan?.children && run.state === 'plan_ready') {
      if (form.creations[run.id]) {
        const record = form.creations[run.id];
        panel.append(badge(tx(record.state), record.state === 'created' ? 'ok' : 'warn'), ...(record.created || []).map(item => link(`#${item.id} ${item.title}`, item.url)));
      } else {
        panel.append(planChildrenReview(run, form.selected.provider === 'azure', async chosen => {
          form.creations[run.id] = await invoke('implementation_create', { machine, id: run.id, item: form.selected, children: chosen, confirmed: true });
          await implementationRuns(form);
        }));
      }
    }
    return panel;
  }));
  if (result.partial) form.runs.append(el('p', { class: 'hint', textContent: t('Showing recent executions. Older records can be requested by their run ID.') }));
  if (!runs.length) form.runs.append(el('p', { class: 'hint', textContent: t('No implementation runs for this item on this machine.') }));
}

export {
  implementationForm, openImplementation, implementationReady, implementationModels, implementationEfforts,
  implementationRuns, planChildrenReview,
};
