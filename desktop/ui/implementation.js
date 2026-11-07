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

let implementationForm = null;
let implementationRequest = 0;
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
    const form = implementationForm = { request, previous, selected, prepared, machine, context, provider, model, effort, directoryList, start, runs, directories: [], catalog: [], generation: 0, creations: prepared.creations || {} };
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
    start.onclick = () => busy(start, async () => {
      const result = await invoke('implementation_start', { machine: machine.value, item: selected, context: context.value, directories: form.directories, provider: provider.value, model: model.value, effort: effort.value });
      if (implementationForm !== form) return;
      done(t('Implementation run started.'));
      await implementationRuns(form);
    });
    $('#implementation-content').replaceChildren(el('div', { class: 'panel' },
      el('h2', { textContent: prepared.planning ? t('Plan child work items') : t('Implement selected work item') }),
      el('p', { class: 'hint', textContent: prepared.planning ? t('Review and edit the proposed children before creating them. Existing children are included as context.') : t('The selected coding agent will work in the directories you choose.') }),
      prepared.planning ? null : el('p', { class: 'callout warn', id: 'implementation-permissions', textContent: t('The agent runs with full permissions on that computer: it runs commands, commits, pushes and opens pull requests there without asking anyone.') }),
      el('label', {}, t('Implementation context'), context),
      el('label', {}, t('Execution machine'), machine),
      el('fieldset', {}, el('legend', { textContent: t('Working directories') }), directoryList, add),
      el('div', { class: 'grid' }, el('label', {}, t('Provider'), provider), el('label', {}, t('Model'), model), el('label', {}, t('Effort'), effort))),
      el('div', { class: 'actions implementation-footer' }, cancel, start),
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
  form.start.disabled = form.start.classList.contains('busy') || !(form.machine.value && form.directories.length && form.provider.value && form.model.value && form.effort.value);
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
    return el('div', { class: 'implementation-path' }, el('code', { textContent: path }), remove);
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
        const azure = form.selected.provider === 'azure';
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
          panel.append(el('fieldset', {}, el('legend', {}, el('label', { class: 'check' }, selected, t('Create this child'))), el('label', {}, t('Title'), title), el('label', {}, t('Description'), description),
            ...(azure ? [el('label', {}, t('Work item type'), kind), ...values.map(v => v.row)] : [])));
          return { selected, title, description, kind, values };
        });
        const create = el('button', { type: 'button', class: 'secondary', textContent: t('Create selected children') });
        let reviewed = false;
        panel.addEventListener('input', () => { reviewed = false; create.textContent = t('Create selected children'); });
        create.onclick = () => busy(create, async () => {
          if (!reviewed) { reviewed = true; create.textContent = t('Confirm child creation'); return; }
          const chosen = children.filter(c => c.selected.checked).map(c => ({ title: c.title.value, description: c.description.value,
            ...(azure ? { type: c.kind.value.trim(), fields: c.values.map(v => ({ reference: v.reference, value: v.value.value.trim() })).filter(v => v.value) } : {}) }));
          if (!chosen.length) throw t('Select at least one child.');
          form.creations[run.id] = await invoke('implementation_create', { machine, id: run.id, item: form.selected, children: chosen, confirmed: true });
          await implementationRuns(form);
        });
        panel.append(create);
      }
    }
    return panel;
  }));
  if (result.partial) form.runs.append(el('p', { class: 'hint', textContent: t('Showing recent executions. Older records can be requested by their run ID.') }));
  if (!runs.length) form.runs.append(el('p', { class: 'hint', textContent: t('No implementation runs for this item on this machine.') }));
}

export {
  implementationForm, openImplementation, implementationReady, implementationModels, implementationEfforts,
  implementationRuns,
};
