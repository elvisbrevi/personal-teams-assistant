// Workflows: the list of workflows and their editor. Definitions are saved on the account's
// coordinator (`workflow_save`, `workflow_remove`); the host checks the project, repositories,
// checkouts and agent, and its warnings say what it could not verify.
import { t, tn, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el } from './dom.js';
import { live } from './state.js';
import { busy, done } from './notices.js';
import { selectTab, showTab } from './nav.js';
import { chooseDirectories, chooseFromList } from './directory-sheet.js';
import { applying } from './profile.js';
import { providerNames } from './home.js';
import { effortNames } from './settings-models.js';
import { blockSummary, KIND_NAMES, loadLaunchers, renderBlocks, STAGE_NAMES } from './workflow-blocks.js';

/** The host's limits (src/workflows.rs), checked here before anything is sent. */
const LIMITS = { name: 80, description: 2000, instructions: 8000, repositories: 20, blocks: 10 };

/**
 * What the Workflows section shows: the definitions, the block library with the workflows that
 * use each block, whether this computer saves them, and the account's machines.
 */
const workflowData = { workflows: [], blocks: [], usedBy: {}, editable: true, machines: [], thisMachine: '', loaded: false };
/** Editors' working copies by workflow ID (`new` for one not saved yet), kept across re-renders. */
const drafts = new Map();
/** Workflows whose editor fold is open. */
const openWorkflows = new Set();
/** The last save's checkout verification per workflow and repository: `{ state, text }`. */
const verification = new Map();
/** What each editor last heard from the host: `{ error, warnings }`. */
const outcomes = new Map();
/** Read once per page: the source's projects, a project's repositories, a machine's agents. */
const projectCatalogs = new Map();
const repositoryCatalogs = new Map();
const agentCatalogs = new Map();

const normalize = text => String(text ?? '').normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLocaleLowerCase();
/** A workflow or block ID from its name: lowercase letters, digits, - and _, starting with a letter. */
function slug(name, taken, prefix = 'w') {
  let base = normalize(name).replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 56);
  if (!/^[a-z]/.test(base)) base = `${prefix}-${base}`.replace(/-+$/, '');
  let id = base;
  for (let n = 2; taken.includes(id); n++) id = `${base}-${n}`;
  return id;
}
function changedAt(seconds) {
  return seconds ? new Date(seconds * 1000).toLocaleString(uiLocale(), { dateStyle: 'medium', timeStyle: 'short' }) : t('Not recorded');
}
const message = error => tx(String(error?.message ?? error));
const machineName = id => workflowData.machines.find(m => m.id === id)?.name || t('Unknown machine');

/** The enabled Azure DevOps sources: workflows read projects and repositories with the one enabled. */
function azureSources() {
  return (live?.map?.resources ?? []).filter(r => r.kind === 'tool' && r.tool?.type === 'azure_devops_status' && r.enabled);
}

let loadGeneration = 0;
async function loadWorkflows() {
  const generation = ++loadGeneration;
  $('#workflows-progress').textContent = t('Loading…');
  $('#workflows-problem').hidden = true;
  $('#blocks-problem').hidden = true;
  try {
    const [list, library, fleet] = await Promise.all([invoke('workflows_list'), invoke('workflow_blocks_list'),
      invoke('machine_list').catch(() => ({ machines: [], this_machine: '' })), loadLaunchers()]);
    if (generation !== loadGeneration) return;
    Object.assign(workflowData, { workflows: list.workflows ?? [], blocks: library.blocks ?? [], usedBy: library.used_by ?? {},
      editable: list.editable !== false && library.editable !== false, machines: fleet.machines ?? [], thisMachine: fleet.this_machine ?? '', loaded: true });
    // A working copy nobody changed follows what is saved now.
    for (const [id, draft] of drafts) {
      if (id === 'new' || draft.changed) continue;
      const saved = workflowData.workflows.find(w => w.id === id);
      if (saved) drafts.set(id, structuredClone(saved)); else drafts.delete(id);
    }
    $('#workflows-progress').textContent = '';
    renderWorkflows();
    renderBlocks();
  } catch (error) {
    if (generation !== loadGeneration) return;
    $('#workflows-progress').textContent = '';
    for (const box of [$('#workflows-problem'), $('#blocks-problem')]) { box.hidden = false; box.textContent = message(error); }
  }
}

function blankWorkflow() {
  const azure = azureSources()[0];
  const machine = workflowData.machines.find(m => m.id === workflowData.thisMachine) ?? workflowData.machines.find(m => m.online);
  return { id: '', name: '', description: '', enabled: true, auto_start: false, organization: azure?.tool?.organization ?? '', project: '',
    repositories: [], machine: machine?.id ?? '', checkouts: {}, agent: { provider: '', model: '', effort: '' }, instructions: '', blocks: [], updated_at: 0, changed: true };
}

function renderWorkflows() {
  const container = $('#workflow-rows');
  const { workflows, editable } = workflowData;
  $('#workflows-summary').textContent = workflows.length
    ? tn(workflows.filter(w => w.enabled).length, '{n} enabled of {total}', '{n} enabled of {total}', { total: workflows.length }) : '';
  $('#workflows-notice').hidden = editable;
  $('#workflows-notice').textContent = editable ? '' : t('This computer is signed in to your account\'s coordinator: workflows are changed and run there, and reach this computer with the account\'s configuration.');
  $('#workflow-new').hidden = !editable;
  $('#workflow-new').disabled = drafts.has('new');
  const rows = [];
  if (editable && drafts.has('new')) rows.push(workflowRow(drafts.get('new'), null));
  for (const workflow of workflows) rows.push(workflowRow(drafts.get(workflow.id) ?? workflow, workflow));
  if (!rows.length) rows.push(el('div', { class: 'empty' }, el('p', { textContent: t('No workflows yet. Create one for each project and set of repositories your HUs and bugs are implemented in.') })));
  container.replaceChildren(...rows);
}

/** The row's summary line: project, repositories, machine, agent, blocks and the last change. */
function workflowFacts(workflow) {
  const agent = workflow.agent?.provider ? `${providerNames[workflow.agent.provider] || workflow.agent.provider} · ${workflow.agent.model}` : t('No agent chosen');
  return [workflow.project || t('No project chosen'),
    `${t('Repositories')}: ${workflow.repositories.map(r => r.name).join(', ') || t('none')}`,
    `${t('Machine')}: ${workflow.machine ? machineName(workflow.machine) : t('none')}`, agent,
    tn(workflow.blocks.length, '1 block', '{n} blocks'),
    workflow.updated_at ? t('Changed {when}', { when: changedAt(workflow.updated_at) }) : t('Not saved yet')].join(' · ');
}

function workflowRow(draft, saved) {
  const key = saved ? saved.id : 'new';
  const facts = el('p', { class: 'resource-facts' });
  const renderFacts = () => facts.replaceChildren(el('span', { textContent: draft.description || t('No description') }), el('small', { textContent: workflowFacts(draft) }));
  renderFacts();
  const head = el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: (saved ?? draft).name || t('New workflow') }),
    saved ? badge(saved.enabled ? t('Enabled') : t('Disabled'), saved.enabled ? 'ok' : 'muted') : badge(t('Not saved yet'), 'info'),
    saved?.auto_start ? badge(t('Starts by itself'), 'active') : null, el('span', { class: 'grow' }));
  const problem = el('p', { class: 'callout error', role: 'alert', hidden: true });
  if (saved && workflowData.editable) {
    const box = el('input', { type: 'checkbox', class: 'switch', checked: saved.enabled, 'aria-label': `${t('Enabled')} · ${saved.name}` });
    box.onchange = () => busy(box, async () => {
      problem.hidden = true;
      try {
        await applying(() => invoke('workflow_save', { id: saved.id, enabled: box.checked }));
        const copy = drafts.get(saved.id);
        if (copy) copy.enabled = box.checked;
        done(box.checked ? t('Workflow {name} enabled.', { name: saved.name }) : t('Workflow {name} disabled.', { name: saved.name }));
        await loadWorkflows();
      } catch (error) {
        box.checked = saved.enabled;
        problem.hidden = false; problem.textContent = message(error);
      }
    });
    head.append(el('label', { class: 'workflow-switch' }, box, el('span', { textContent: t('Enabled') })));
  }
  const row = el('div', { class: 'resource workflow-row', 'data-workflow': key }, head, facts, problem);
  if (!workflowData.editable) return row;
  const edit = el('details', { class: 'fold resource-edit', open: openWorkflows.has(key) },
    el('summary', { textContent: saved ? t('Edit the workflow') : t('Write the new workflow') }));
  let built = false;
  const build = () => {
    if (built) return;
    built = true;
    if (!drafts.has(key)) drafts.set(key, structuredClone(saved));
    edit.append(workflowEditor(drafts.get(key), saved, renderFacts));
  };
  edit.ontoggle = () => { if (edit.open) { openWorkflows.add(key); build(); } else openWorkflows.delete(key); };
  if (edit.open) build();
  row.append(edit);
  return row;
}

/** A labelled text field whose value lives in `draft[key]`; a long one counts its characters. */
function textField(draft, key, label, { area = false, rows = 3, max, hint = '', changed }) {
  const input = el(area ? 'textarea' : 'input', { value: draft[key] ?? '', maxLength: max, ...(area ? { rows } : {}) });
  const counter = el('small', { class: 'counter' });
  const count = () => {
    const length = [...input.value].length;
    counter.textContent = t('{n} of {max} characters', { n: length, max });
    counter.classList.toggle('over', length > max);
  };
  input.oninput = () => { draft[key] = input.value; draft.changed = true; count(); changed?.(); };
  count();
  return el('label', {}, label, hint ? el('small', { textContent: hint }) : null, input, area ? counter : null);
}

function workflowEditor(draft, saved, renderFacts) {
  const key = saved ? saved.id : 'new';
  const outcome = outcomes.get(key) ?? {};
  if (!verification.has(key)) verification.set(key, {});
  const checks = verification.get(key);
  const changed = () => renderFacts();
  const touched = () => { draft.changed = true; changed(); };
  const problem = el('div', { class: 'callout error', role: 'alert', hidden: !outcome.error, textContent: outcome.error ?? '' });
  const warnings = el('div', { class: 'callout warn', role: 'status', hidden: !outcome.warnings?.length },
    ...(outcome.warnings ?? []).map(text => el('p', { textContent: tx(text) })));
  const report = ({ error = '', list = [] }) => {
    outcomes.set(key, { error, warnings: list });
    problem.hidden = !error; problem.textContent = error;
    warnings.replaceChildren(...list.map(text => el('p', { textContent: tx(text) })));
    warnings.hidden = !list.length;
  };

  // Name and, while the workflow is new, the ID it is saved under.
  const fields = el('div', { class: 'grid' });
  const name = textField(draft, 'name', t('Name'), { max: LIMITS.name, changed });
  fields.append(name);
  if (!saved) {
    const idInput = el('input', { value: draft.id, maxLength: 64, translate: 'no', spellcheck: false, autocapitalize: 'none' });
    idInput.oninput = () => { draft.id = idInput.value.trim(); draft.idEdited = true; };
    name.querySelector('input').addEventListener('input', () => {
      if (!draft.idEdited) { draft.id = slug(draft.name, workflowData.workflows.map(w => w.id)); idInput.value = draft.id; }
    });
    fields.append(el('label', {}, t('Workflow ID'), el('small', { textContent: t('Lowercase letters, digits, - and _; it cannot change once saved.') }), idInput));
  }
  const description = textField(draft, 'description', t('Description'), { area: true, rows: 3, max: LIMITS.description, changed,
    hint: t('What the director reads to choose this workflow: the kind of items, the system and the repositories it covers.') });

  // Project: the enabled Azure source's projects, in a searchable sheet.
  const sources = azureSources();
  const project = el('button', { type: 'button', class: 'secondary request-target', 'aria-haspopup': 'dialog', id: `workflow-project-${key}`,
    'aria-labelledby': `workflow-project-label-${key} workflow-project-${key}`, disabled: !sources.length, textContent: draft.project || t('Choose a project') });
  const projectProblem = el('p', { class: 'callout error', role: 'alert', hidden: true });
  project.onclick = () => busy(project, async () => {
    projectProblem.hidden = true;
    const source = sources[0].id;
    let catalog = projectCatalogs.get(source);
    if (!catalog) {
      try {
        catalog = await invoke('implementation_request_catalog', { provider: 'azure', source });
      } catch (error) { projectProblem.hidden = false; projectProblem.textContent = message(error); return; }
      projectCatalogs.set(source, catalog);
    }
    const targets = catalog.targets ?? [];
    const label = target => target.organization ? `${target.project} · ${target.organization.replace(/^https:\/\/dev\.azure\.com\//, '')}` : target.project;
    const index = target => String(targets.findIndex(x => x.project === target.project && x.organization === target.organization));
    chooseFromList({ trigger: project, title: t('Choose a project'), allLabel: t('All projects'), empty: t('No matching projects.'),
      countText: n => tn(n, '{n} project', '{n} projects'),
      items: targets.map((target, at) => ({ value: String(at), label: label(target) })),
      recent: (catalog.recent ?? []).map(index), selected: [index(draft)],
      apply: ([value]) => {
        const target = targets[Number(value)];
        if (!target || (target.project === draft.project && target.organization === draft.organization)) return;
        // Another project has other repositories: the choice and its checkouts start again.
        Object.assign(draft, { project: target.project, organization: target.organization, repositories: [], checkouts: {} });
        for (const id of Object.keys(checks)) delete checks[id];
        project.textContent = target.project;
        touched(); renderRepositories();
      } });
  });

  // Repositories of the project and the checkout of each on the machine.
  const repositories = el('div', { class: 'workflow-repos' });
  const repositoryProblem = el('p', { class: 'callout error', role: 'alert', hidden: true });
  const chooseRepos = el('button', { type: 'button', class: 'secondary', 'aria-haspopup': 'dialog', textContent: t('Choose repositories') });
  chooseRepos.onclick = () => busy(chooseRepos, async () => {
    repositoryProblem.hidden = true;
    const cacheKey = `${draft.organization}|${draft.project}`;
    let catalog = repositoryCatalogs.get(cacheKey);
    if (!catalog) {
      try {
        const reply = await invoke('workflow_repositories', { project: draft.project, ...(draft.organization ? { organization: draft.organization } : {}) });
        catalog = reply.repositories ?? [];
      } catch (error) { repositoryProblem.hidden = false; repositoryProblem.textContent = message(error); return; }
      repositoryCatalogs.set(cacheKey, catalog);
    }
    chooseFromList({ trigger: chooseRepos, title: t('Repositories of {project}', { project: draft.project }), multiple: true, max: LIMITS.repositories,
      allLabel: t('All repositories'), empty: t('No matching repositories.'), confirm: t('Use selected repositories'),
      countText: n => tn(n, '{n} repository selected', '{n} repositories selected'),
      items: catalog.map(r => ({ value: r.id, label: r.name, detail: (r.default_branch || '').replace(/^refs\/heads\//, '') })),
      selected: draft.repositories.map(r => r.id),
      apply: values => {
        const chosen = values.map(id => draft.repositories.find(r => r.id === id) ?? catalog.find(r => r.id === id)).filter(Boolean).map(r => ({ id: r.id, name: r.name }));
        const keep = new Set(chosen.map(r => r.id.toLowerCase()));
        draft.repositories = chosen;
        draft.checkouts = Object.fromEntries(Object.entries(draft.checkouts).filter(([id]) => keep.has(id.toLowerCase())));
        touched(); renderRepositories();
      } });
  });
  const renderRepositories = () => {
    chooseRepos.disabled = !draft.project;
    repositories.replaceChildren(...draft.repositories.map(repository => {
      const id = repository.id.toLowerCase();
      const path = draft.checkouts[id] ?? '';
      const check = checks[id];
      const choose = el('button', { type: 'button', class: 'secondary small', disabled: !draft.machine, 'aria-haspopup': 'dialog',
        textContent: path ? t('Change checkout') : t('Choose checkout'), 'aria-label': `${path ? t('Change checkout') : t('Choose checkout')} · ${repository.name}` });
      choose.onclick = () => {
        const machine = workflowData.machines.find(m => m.id === draft.machine);
        chooseDirectories({ trigger: choose, title: t('Checkout of {name} on {machine}', { name: repository.name, machine: machine?.name || draft.machine }),
          selected: path ? [path] : [], multiple: false,
          browse: dir => invoke('implementation_browse', { machine: draft.machine, path: dir }),
          apply: ([dir]) => {
            if (!dir) return;
            draft.checkouts[id] = dir;
            delete checks[id];
            touched(); renderRepositories();
          } });
      };
      const remove = el('button', { type: 'button', class: 'secondary small', textContent: t('Remove'), 'aria-label': `${t('Remove')} · ${repository.name}` });
      remove.onclick = () => {
        draft.repositories = draft.repositories.filter(r => r !== repository);
        delete draft.checkouts[id]; delete checks[id];
        touched(); renderRepositories(); chooseRepos.focus();
      };
      const state = check && { verified: badge(t('Verified'), 'ok'), unknown: badge(t('Not verified'), 'warn'), mismatch: badge(t('Mismatch'), 'error') }[check.state];
      return el('div', { class: 'workflow-repo', 'data-repository': repository.id },
        el('div', {}, el('strong', {}, repository.name, state ? ' ' : null, state),
          path ? el('code', { textContent: path, translate: 'no' }) : el('small', { textContent: t('No checkout chosen') }),
          check?.text ? el('small', { textContent: tx(check.text) }) : null),
        el('div', { class: 'actions' }, choose, remove));
    }));
    if (!draft.repositories.length) repositories.append(el('p', { class: 'hint', textContent: draft.project ? t('Choose the repositories this workflow works on.') : t('Choose the project first.') }));
  };
  renderRepositories();

  // Machine and coding agent, as in Implement.
  const machine = el('select', {}, el('option', { value: '', textContent: t('Choose a machine') }),
    ...workflowData.machines.map(m => el('option', { value: m.id, textContent: `${m.name} · ${m.os}${m.online ? '' : ` · ${t('Offline')}`}` })));
  machine.value = draft.machine;
  const provider = el('select', {});
  const model = el('select', {});
  const effort = el('select', {});
  const agentNote = el('p', { class: 'hint', role: 'status' });
  const fillAgent = providers => {
    const chosen = draft.agent;
    // A choice the machine did not list stays as it is saved: the host checks it again.
    const list = providers.length ? providers
      : chosen.provider ? [{ id: chosen.provider, models: [{ id: chosen.model, name: chosen.model, efforts: [chosen.effort] }] }] : [];
    provider.replaceChildren(...list.map(p => el('option', { value: p.id, textContent: providerNames[p.id] || p.id })));
    if (!list.length) provider.append(el('option', { value: '', textContent: t('No coding CLI installed on this machine') }));
    provider.value = list.some(p => p.id === chosen.provider) ? chosen.provider : list[0]?.id ?? '';
    const fillEfforts = () => {
      const info = list.find(p => p.id === provider.value)?.models?.find(m => m.id === model.value);
      effort.replaceChildren(...(info?.efforts ?? []).map(value => el('option', { value, textContent: effortNames[value] || value })));
      effort.value = info?.efforts?.includes(draft.agent.effort) ? draft.agent.effort : info?.default_effort ?? info?.efforts?.[0] ?? '';
      draft.agent = { provider: provider.value, model: model.value, effort: effort.value };
      changed();
    };
    const fillModels = () => {
      const info = list.find(p => p.id === provider.value);
      model.replaceChildren(...(info?.models ?? []).map(m => el('option', { value: m.id, textContent: m.name || m.id })));
      model.value = info?.models?.some(m => m.id === draft.agent.model) ? draft.agent.model : info?.models?.[0]?.id ?? '';
      fillEfforts();
    };
    provider.onchange = () => { draft.changed = true; draft.agent.provider = provider.value; fillModels(); };
    model.onchange = () => { draft.changed = true; draft.agent.model = model.value; fillEfforts(); };
    effort.onchange = () => { draft.changed = true; draft.agent.effort = effort.value; changed(); };
    fillModels();
  };
  const loadAgents = async () => {
    const id = draft.machine;
    if (!id) { agentNote.textContent = ''; fillAgent([]); return; }
    agentNote.textContent = t('Loading…');
    try {
      let providers = agentCatalogs.get(id);
      if (!providers) { providers = (await invoke('implementation_catalog', { machine: id })).providers ?? []; agentCatalogs.set(id, providers); }
      if (draft.machine !== id) return;
      agentNote.textContent = '';
      fillAgent(providers);
    } catch (error) {
      if (draft.machine !== id) return;
      agentNote.textContent = t('The machine\'s coding agents could not be read ({reason}); the saved choice is checked again when you save.', { reason: message(error) });
      fillAgent([]);
    }
  };
  machine.onchange = () => {
    // Checkouts are directories of one machine: another machine needs its own.
    draft.machine = machine.value; draft.checkouts = {};
    for (const id of Object.keys(checks)) delete checks[id];
    touched(); renderRepositories(); loadAgents();
  };
  loadAgents();

  const instructions = textField(draft, 'instructions', t('Instructions'), { area: true, rows: 6, max: LIMITS.instructions, changed,
    hint: t('What the planning and coding agents read for every item of this workflow. Code still adds the rules you cannot change here.') });

  // Blocks: an ordered list taken from the library.
  const blockList = el('div', { class: 'workflow-blocks' });
  const addChoice = el('select', {});
  const addBlock = el('button', { type: 'button', class: 'secondary', textContent: t('Add block') });
  const renderBlockList = () => {
    blockList.replaceChildren(...draft.blocks.map((id, index) => {
      const block = workflowData.blocks.find(b => b.id === id);
      const label = block?.name || id;
      const move = (delta, text) => {
        const button = el('button', { type: 'button', class: 'secondary small', textContent: text, 'aria-label': `${text} · ${label}`,
          disabled: index + delta < 0 || index + delta >= draft.blocks.length });
        button.onclick = () => {
          const list = draft.blocks;
          [list[index], list[index + delta]] = [list[index + delta], list[index]];
          touched(); renderBlockList();
          blockList.children[index + delta]?.querySelector(`button[data-move="${delta}"]:not(:disabled)`)?.focus();
        };
        button.dataset.move = String(delta);
        return button;
      };
      const remove = el('button', { type: 'button', class: 'secondary small', textContent: t('Remove'), 'aria-label': `${t('Remove')} · ${label}` });
      remove.onclick = () => { draft.blocks = draft.blocks.filter(b => b !== id); touched(); renderBlockList(); addChoice.focus(); };
      return el('div', { class: 'workflow-block', 'data-block': id },
        el('div', {}, el('strong', { textContent: `${index + 1}. ${label}` }),
          el('small', { textContent: block ? [KIND_NAMES[block.kind], block.stage ? STAGE_NAMES[block.stage] : null, blockSummary(block)].filter(Boolean).join(' · ') : t('This block no longer exists; remove it.') })),
        el('div', { class: 'actions' }, move(-1, t('Move up')), move(1, t('Move down')), remove));
    }));
    if (!draft.blocks.length) blockList.append(el('p', { class: 'hint', textContent: t('No blocks. Add reusable instructions, Teams notifications or launchers from the library.') }));
    const available = workflowData.blocks.filter(b => !draft.blocks.includes(b.id));
    addChoice.replaceChildren(...available.map(b => el('option', { value: b.id, textContent: `${b.name} · ${KIND_NAMES[b.kind]}` })));
    if (!available.length) addChoice.append(el('option', { value: '', textContent: workflowData.blocks.length ? t('Every block is already in this workflow') : t('The library has no blocks yet') }));
    addChoice.disabled = !available.length;
    addBlock.disabled = !available.length || draft.blocks.length >= LIMITS.blocks;
  };
  addBlock.onclick = () => {
    if (!addChoice.value || draft.blocks.length >= LIMITS.blocks) return;
    draft.blocks = [...draft.blocks, addChoice.value];
    touched(); renderBlockList(); addChoice.focus();
  };
  renderBlockList();
  const library = el('button', { type: 'button', class: 'link-button', textContent: t('Open the block library') });
  library.onclick = () => { selectTab($('#workflows-tabs'), 'blocks'); $('#workflows-tab-blocks').focus(); };

  const autoStart = el('input', { type: 'checkbox', checked: Boolean(draft.auto_start) });
  autoStart.onchange = () => { draft.auto_start = autoStart.checked; touched(); };
  const readyStates = el('button', { type: 'button', class: 'link-button', textContent: t('Choose the ready states') });
  readyStates.onclick = () => showTab('settings', 'panel-workflow-ready');

  // Save, enable or disable, remove.
  const save = el('button', { type: 'button', textContent: saved ? t('Save workflow') : t('Create workflow') });
  save.onclick = () => busy(save, async () => {
    const missing = missingFields(draft, saved);
    if (missing.length) { report({ error: `${t('Before saving:')} ${missing.join(' ')}` }); return; }
    report({});
    const workflow = {
      id: saved ? saved.id : draft.id, name: draft.name.trim(), description: draft.description, enabled: saved ? saved.enabled : true,
      auto_start: Boolean(draft.auto_start), organization: draft.organization, project: draft.project,
      repositories: draft.repositories.map(({ id, name }) => ({ id, name })), machine: draft.machine, checkouts: { ...draft.checkouts },
      agent: { ...draft.agent }, instructions: draft.instructions, blocks: [...draft.blocks], updated_at: saved?.updated_at ?? 0,
    };
    try {
      const reply = await applying(() => invoke('workflow_save', { workflow }));
      const result = reply.workflow ?? workflow;
      const list = reply.warnings ?? [];
      verification.set(result.id, checkoutStates(result, list));
      outcomes.set(result.id, { error: '', warnings: list });
      drafts.delete('new'); outcomes.delete('new'); verification.delete('new');
      drafts.set(result.id, structuredClone(result));
      openWorkflows.delete('new'); openWorkflows.add(result.id);
      done(saved ? t('Workflow {name} saved.', { name: result.name }) : t('Workflow {name} created.', { name: result.name }));
      await loadWorkflows();
      $(`[data-workflow="${result.id}"] summary`)?.focus({ preventScroll: true });
    } catch (error) {
      const text = message(error);
      // A refusal about one checkout is shown next to it, too.
      const named = workflow.repositories.find(r => {
        const dir = workflow.checkouts[r.id.toLowerCase()];
        return dir && text.includes(dir);
      });
      if (named) { checks[named.id.toLowerCase()] = { state: 'mismatch', text }; renderRepositories(); }
      report({ error: text });
    }
  });
  const actions = [];
  if (saved) {
    const toggle = el('button', { type: 'button', class: 'secondary', textContent: saved.enabled ? t('Disable') : t('Enable') });
    toggle.onclick = () => busy(toggle, async () => {
      try {
        await applying(() => invoke('workflow_save', { id: saved.id, enabled: !saved.enabled }));
        draft.enabled = !saved.enabled;
        done(saved.enabled ? t('Workflow {name} disabled.', { name: saved.name }) : t('Workflow {name} enabled.', { name: saved.name }));
        await loadWorkflows();
      } catch (error) { report({ error: message(error) }); }
    });
    const remove = el('button', { type: 'button', class: 'danger', textContent: t('Remove workflow') });
    let armed = null;
    remove.onclick = () => busy(remove, async () => {
      if (!armed) {
        remove.textContent = t('Tap again to remove');
        armed = setTimeout(() => { armed = null; remove.textContent = t('Remove workflow'); }, 4000);
        return;
      }
      clearTimeout(armed); armed = null;
      try {
        await applying(() => invoke('workflow_remove', { id: saved.id, confirmed: true }));
        drafts.delete(saved.id); openWorkflows.delete(saved.id); verification.delete(saved.id); outcomes.delete(saved.id);
        done(t('Workflow {name} removed.', { name: saved.name }));
        await loadWorkflows();
        $('#workflow-new').focus({ preventScroll: true });
      } catch (error) { remove.textContent = t('Remove workflow'); report({ error: message(error) }); }
    });
    actions.push(remove, el('span', { class: 'grow' }), toggle);
  } else {
    const discard = el('button', { type: 'button', class: 'secondary', textContent: t('Discard') });
    discard.onclick = () => { drafts.delete('new'); outcomes.delete('new'); openWorkflows.delete('new'); renderWorkflows(); $('#workflow-new').focus(); };
    actions.push(discard);
  }
  actions.push(save);

  return el('div', { class: 'workflow-editor' }, fields, description,
    el('div', { class: 'field-block' }, el('label', { id: `workflow-project-label-${key}`, textContent: t('Project') }), project,
      sources.length ? null : el('p', { class: 'hint', textContent: t('Enable an Azure DevOps source in Knowledge first: workflows read its projects and repositories.') }), projectProblem),
    el('label', {}, t('Execution machine'), el('small', { textContent: t('The computer the coding agent works on; each repository needs its checkout there.') }), machine),
    el('fieldset', {}, el('legend', { textContent: t('Repositories and checkouts') }), repositories, repositoryProblem, el('div', { class: 'actions' }, chooseRepos)),
    el('div', { class: 'grid' }, el('label', {}, t('CLI'), provider), el('label', {}, t('Model'), model), el('label', {}, t('Effort'), effort)), agentNote,
    instructions,
    el('fieldset', {}, el('legend', { textContent: t('Blocks, in order') }), blockList,
      el('div', { class: 'workflow-add' }, el('label', {}, t('Block from the library'), addChoice), addBlock), el('div', { class: 'actions' }, library)),
    el('label', { class: 'check' }, autoStart, el('span', {}, t('Start by itself when an assigned item reaches a ready state'),
      el('small', { textContent: t('The ready states are chosen per project in Settings. Children are still created only after you confirm their plan.') }))),
    el('div', { class: 'actions' }, readyStates),
    problem, warnings, el('div', { class: 'actions' }, ...actions));
}

/** What the host would refuse before reading anything, in the order of the form. */
function missingFields(draft, saved) {
  const missing = [];
  if (!draft.name.trim()) missing.push(t('Write its name.'));
  if (!saved && !/^[a-z][a-z0-9_-]{0,63}$/.test(draft.id)) missing.push(t('Give it an ID of lowercase letters, digits, - and _, starting with a letter.'));
  if (!saved && workflowData.workflows.some(w => w.id === draft.id)) missing.push(t('Another workflow has the ID {id}.', { id: draft.id }));
  if ([...draft.description].length > LIMITS.description) missing.push(t('Shorten the description.'));
  if (!draft.project) missing.push(t('Choose the project.'));
  if (!draft.repositories.length) missing.push(t('Choose at least one repository.'));
  if (!draft.machine) missing.push(t('Choose the execution machine.'));
  const without = draft.repositories.filter(r => !draft.checkouts[r.id.toLowerCase()]);
  if (without.length) missing.push(t('Choose the checkout of {names}.', { names: without.map(r => r.name).join(', ') }));
  if (!draft.agent.provider || !draft.agent.model || !draft.agent.effort) missing.push(t('Choose the coding agent, its model and effort.'));
  if ([...draft.instructions].length > LIMITS.instructions) missing.push(t('Shorten the instructions.'));
  if (draft.blocks.length > LIMITS.blocks) missing.push(t('Keep at most {n} blocks.', { n: LIMITS.blocks }));
  return missing;
}

/** Each repository's checkout as the save left it: verified, or not verified with the host's warning. */
function checkoutStates(workflow, warnings) {
  const states = {};
  for (const repository of workflow.repositories) {
    const id = repository.id.toLowerCase();
    const dir = Object.entries(workflow.checkouts).find(([k]) => k.toLowerCase() === id)?.[1];
    const about = warnings.filter(text => dir && text.includes(dir));
    const doubt = about.find(text => !text.includes(' is saved as the machine spells it: '));
    states[id] = doubt ? { state: 'unknown', text: doubt } : { state: 'verified', text: about[0] ?? '' };
  }
  return states;
}

function newWorkflow() {
  if (drafts.has('new')) return;
  drafts.set('new', blankWorkflow());
  openWorkflows.add('new');
  renderWorkflows();
  $('[data-workflow="new"] input')?.focus();
}

$('#workflow-new').onclick = newWorkflow;
$('#workflows-refresh').onclick = () => busy($('#workflows-refresh'), loadWorkflows);
$('#workflows-settings').onclick = () => showTab('settings', 'panel-workflow-ready');

export { workflowData, loadWorkflows, renderWorkflows, slug, changedAt };
