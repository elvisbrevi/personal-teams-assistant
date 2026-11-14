// To-Do › Create request and Agent history.
import { transport } from './transport.js';
import { t, tn, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el, icon } from './dom.js';
import { tab } from './state.js';
import { busy, fail } from './notices.js';
import { showTab } from './nav.js';
import { todoTypes } from './todo.js';
import { copy, link } from './markdown.js';
import { openAgentSession, sessionDestination, sessionIsMobile, sessionLifeBadge } from './sessions.js';
import { implementationEfforts, implementationModels, implementationReady } from './implementation.js';
import { loadAzureCliStatus, loadGithubCliStatus, providerNames } from './home.js';
import { effortNames } from './settings-models.js';
import { failureText } from './run-states.js';

// ---------------------------------------------------------------- request creation and agent history
let requestForm = null;
let requestGeneration = 0;
async function openRequest() {
  const generation = ++requestGeneration;
  requestForm = null;
  const azure = $('#todo-provider').value === 'azure';
  const source = $('#todo-source').value;
  showTab('create-request');
  $('#create-request h1').focus();
  $('#request-destination').textContent = azure ? t('Azure DevOps · HU or bug') : t('GitHub · Epic');
  $('#request-problem').hidden = true;
  $('#request-content').replaceChildren(el('p', { class: 'hint', textContent: t('Loading…') }));
  const cancel = el('button', { type: 'button', class: 'secondary', textContent: t('Cancel') });
  cancel.onclick = () => { ++requestGeneration; requestForm = null; showTab('todo'); $('#todo-create').focus(); };
  try {
    const catalog = await invoke('implementation_request_catalog', { provider: azure ? 'azure' : 'github', ...(azure ? { source } : {}) });
    if (generation !== requestGeneration || tab !== 'create-request') return;
    const target = el('button', { id: 'request-target', type: 'button', class: 'secondary request-target', value: '', 'aria-haspopup': 'dialog', 'aria-labelledby': 'request-target-label request-target', textContent: azure ? t('Choose a project') : t('Choose a repository') });
    target.onclick = () => chooseRequestTarget(target, catalog, azure);
    const context = el('textarea', { id: 'request-context', rows: 7, maxLength: 32000, required: true });
    const provider = el('select', { id: 'request-provider' }, ...catalog.providers.map(p => el('option', { value: p.id, textContent: providerNames[p.id] || p.id })));
    const model = el('select', { id: 'request-model' });
    const effort = el('select', { id: 'request-effort' });
    const start = el('button', { id: 'request-create', type: 'button', textContent: t('Create'), disabled: true });
    const result = el('div', { id: 'request-result', 'aria-live': 'polite' });
    const kind = el('fieldset', {}, el('legend', { textContent: t('Request type') }));
    const filter = $('#todo-kind').value;
    const requested = filter === 'bug' || todoTypes.some(type => `type:${type.name}` === filter && type.group === 'bug') ? 'bug' : 'hu';
    for (const [value, label] of [['hu', t('HU')], ['bug', t('Bug')]]) {
      const radio = el('input', { type: 'radio', name: 'request-kind', value, checked: value === requested });
      kind.append(el('label', { class: 'check' }, radio, label));
    }
    kind.hidden = !azure;
    const form = requestForm = { purpose: 'request', generation, id: crypto.randomUUID(), target, context, provider, model, effort, start, catalog: catalog.providers, targets: catalog.targets, writeReady: catalog.write_ready, result, cancel, submitted: false };
    provider.onchange = () => implementationModels(form);
    model.onchange = () => implementationEfforts(form);
    effort.onchange = context.oninput = target.onchange = () => implementationReady(form);
    implementationModels(form);
    start.onclick = () => busy(start, async () => {
      if (form.submitted) return;
      form.submitted = true;
      for (const control of [target, context, provider, model, effort, ...kind.querySelectorAll('input')]) control.disabled = true;
      cancel.textContent = t('Back to To-Do');
      result.replaceChildren(el('p', { class: 'hint', textContent: t('Creating request…') }));
      try {
        await invoke('implementation_request_start', { id: form.id, target: form.targets[Number(target.value)], kind: azure ? kind.querySelector('input:checked').value : 'epic', context: context.value, provider: provider.value, model: model.value, effort: effort.value, confirmed: true });
        await loadRequestStatus(form);
      } catch (error) {
        // Creation may have started: retain the UUID and inspect status without replaying the send.
        result.replaceChildren(el('p', { class: 'callout error', role: 'alert', textContent: tx(String(error)) }));
        const inspect = el('button', { class: 'secondary', textContent: t('Check creation status') });
        inspect.onclick = () => busy(inspect, () => loadRequestStatus(form));
        result.append(inspect);
      }
    });
    const sessionStatus = el('button', { type: 'button', class: 'secondary', textContent: azure ? t('Check Azure CLI status') : t('Check GitHub CLI status') });
    sessionStatus.onclick = () => { showTab('overview', azure ? 'azure-cli-status' : 'github-cli-status'); azure ? loadAzureCliStatus() : loadGithubCliStatus(); };
    $('#request-content').replaceChildren(el('div', { class: 'panel' },
      el('div', { class: 'field-block' }, el('label', { id: 'request-target-label', textContent: azure ? t('Project') : t('Repository') }), target), kind,
      el('label', {}, t('Request context'), context),
      el('div', { class: 'grid' }, el('label', {}, t('CLI'), provider), el('label', {}, t('Model'), model), el('label', {}, t('Effort'), effort)),
      catalog.write_ready === false ? el('div', { class: 'callout warn' }, el('p', { textContent: azure ? t('Select Azure CLI on Home to sign in with Microsoft in your browser.') : t('Sign in with gh auth login on the machine running this app, then refresh GitHub CLI status.') }), sessionStatus) : null,
      !catalog.targets.length ? el('p', { class: 'callout warn', textContent: t('No destinations are available. Check the selected source or GitHub CLI session.') }) : null,
      !catalog.providers.length ? el('p', { class: 'callout warn', textContent: t('No coding CLI installed on this machine') }) : null),
      el('div', { class: 'actions request-footer' }, cancel, start), result);
  } catch (error) {
    if (generation !== requestGeneration) return;
    $('#request-problem').hidden = false;
    $('#request-problem').textContent = tx(String(error));
    $('#request-content').replaceChildren(cancel);
  }
}
function requestTargetLabel(target) {
  return target.organization ? `${target.project} · ${target.organization}` : target.project;
}
function chooseRequestTarget(trigger, catalog, azure) {
  const title = azure ? t('Choose a project') : t('Choose a repository');
  const dialog = el('dialog', { class: 'directory-picker destination-picker', 'aria-label': title });
  const close = el('button', { type: 'button', class: 'icon-button secondary', 'aria-label': t('Cancel') }, icon('close'));
  const search = el('input', { id: 'request-target-search', type: 'search', autocomplete: 'off' });
  const body = el('div', { class: 'directory-picker-body' });
  const count = el('p', { class: 'hint', role: 'status' });
  const normalize = text => text.normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLocaleLowerCase();
  const recent = (catalog.recent || []).map(target => catalog.targets.findIndex(t => t.project === target.project && t.organization === target.organization)).filter(index => index >= 0).slice(0, 3);
  const render = () => {
    const query = normalize(search.value.trim());
    const matches = catalog.targets.map((target, index) => ({ target, index })).filter(({ target }) => normalize(target.project).includes(query));
    const row = ({ target, index }) => {
      const button = el('button', { type: 'button', class: 'link-button', value: String(index), textContent: requestTargetLabel(target), 'aria-pressed': trigger.value === String(index) });
      button.onclick = () => {
        trigger.value = String(index); trigger.textContent = requestTargetLabel(target);
        trigger.onchange(); dialog.close();
      };
      return el('div', { class: 'directory-picker-row' }, button);
    };
    const recentMatches = matches.filter(({ index }) => recent.includes(index)).sort((a, b) => recent.indexOf(a.index) - recent.indexOf(b.index));
    const otherMatches = matches.filter(({ index }) => !recent.includes(index));
    body.replaceChildren(...[
      recentMatches.length ? el('h3', { textContent: t('Recent') }) : null,
      recentMatches.length ? el('div', { class: 'directory-picker-list', 'data-recent': '' }, ...recentMatches.map(row)) : null,
      otherMatches.length ? el('h3', { textContent: azure ? t('All projects') : t('All repositories') }) : null,
      otherMatches.length ? el('div', { class: 'directory-picker-list' }, ...otherMatches.map(row)) : null,
      !matches.length ? el('p', { class: 'hint', textContent: t('No matching destinations.') }) : null,
    ].filter(Boolean));
    count.textContent = tn(matches.length, '{n} destination', '{n} destinations');
  };
  search.oninput = render;
  dialog.onkeydown = event => { if (event.key === 'Escape') { event.preventDefault(); dialog.close(); } };
  close.onclick = () => dialog.close();
  dialog.onclose = () => { document.body.classList.remove('directory-picker-open'); dialog.remove(); trigger.focus({ preventScroll: true }); };
  dialog.append(el('div', { class: 'directory-picker-head' }, el('h2', { textContent: title }), close),
    el('label', { class: 'destination-search' }, t('Filter by name'), search), body,
    el('div', { class: 'directory-picker-footer' }, count));
  document.body.append(dialog); document.body.classList.add('directory-picker-open');
  render(); dialog.showModal(); search.focus();
}
async function loadRequestStatus(form) {
  const record = await invoke('implementation_request_status', { id: form.id });
  if (requestForm !== form || tab !== 'create-request') return;
  if (!record) {
    form.result.replaceChildren(el('p', { class: 'hint', textContent: t('No execution was recorded. Return to To-Do to correct the request.') }));
    return;
  }
  form.result.replaceChildren(agentRunCard(record));
  if (['generating', 'sending'].includes(record.state)) setTimeout(() => {
    if (requestForm === form && tab === 'create-request') loadRequestStatus(form).catch(fail);
  }, 3000);
}
function agentRunCard(run) {
  const open = el('button', { type: 'button', class: 'secondary small', textContent: t('Open agent session') });
  open.onclick = () => openAgentSession(run, open);
  const destination = sessionDestination(run, sessionIsMobile(), !transport.browser);
  open.disabled = !destination.url && !destination.native && !destination.connect;
  if (open.disabled) open.title = destination.reason;
  const pairs = [[t('Run ID'), run.id], [t('Agent session ID'), run.session_id || t('Not recorded')],
    [t('Execution machine'), run.machine_name || run.machine], [t('Effort'), effortNames[run.effort] || run.effort || t('Not recorded')],
    ...[[t('Requested at'), run.created_at], [t('Started at'), run.started_at], [t('Finished at'), run.finished_at]].map(([key, value]) => [key, value ? new Date(value).toLocaleString(uiLocale()) : t('Not recorded')])];
  const copySession = el('button', { type: 'button', class: 'secondary small', textContent: t('Copy session ID'), disabled: !run.session_id });
  copySession.onclick = () => busy(copySession, async () => { if (!await copy(run.session_id)) fail(t('Could not copy the session ID.')); });
  const card = el('article', { class: 'implementation-run', 'data-run-id': run.id },
    el('div', { class: 'todo-context' }, badge(run.purpose === 'request' && run.state === 'created' ? t('Request created') : tx(run.state), ['created', 'done', 'plan_ready'].includes(run.state) ? 'ok' : ['failed', 'uncertain', 'needs_attention', 'permission_prompt'].includes(run.state) ? 'warn' : 'info'),
      el('strong', { textContent: `${providerNames[run.provider] || run.provider} · ${run.model}` }), run.purpose === 'request' ? badge(t('Request creation'), 'muted') : badge(t('Implementation'), 'muted'), sessionLifeBadge(run)),
    run.project ? el('p', { textContent: `${run.project} · ${{ hu: t('HU'), bug: t('Bug'), epic: t('Epic') }[run.kind] || tx(run.kind)}` }) : null,
    run.item?.url ? link(`#${run.item.id} · ${run.item.title}`, run.item.url) : null,
    el('dl', { class: 'todo-fields' }, ...pairs.map(([label, value]) => el('div', {}, el('dt', { textContent: label }), el('dd', { textContent: value })))),
    el('div', { class: 'actions' }, open, copySession),
    open.disabled ? el('p', { class: 'hint', textContent: destination.reason }) : null,
    run.output ? el('p', { class: 'hint', textContent: tx(run.output) }) : null,
    run.failure ? el('p', { class: 'callout warn run-failure', textContent: failureText(run.failure) }) : null,
    run.state === 'uncertain' ? el('p', { class: 'callout warn', textContent: t('The provider may have created the request. Inspect it before creating another; this execution will not be retried.') }) : null,
    run.state === 'needs_attention' && run.purpose === 'request' ? el('p', { class: 'callout warn', textContent: t('The issue was created, but its Epic type or assignment could not be confirmed. Review the original issue.') }) : null);
  return card;
}
let historyGeneration = 0;
async function loadAgentHistory() {
  const generation = ++historyGeneration;
  $('#agent-history-problem').hidden = true;
  $('#agent-history-progress').textContent = t('Loading…');
  try {
    const result = await invoke('implementation_history');
    if (generation !== historyGeneration || tab !== 'agent-history') return;
    $('#agent-history-list').replaceChildren(...result.runs.map(agentRunCard));
    if (!result.runs.length) $('#agent-history-list').append(el('p', { class: 'empty', textContent: t('No agent executions recorded yet.') }));
    $('#agent-history-progress').textContent = result.partial ? t('Showing the latest executions. Older records remain saved.') : tn(result.runs.length, '{count} execution', '{count} executions', { count: result.runs.length });
  } catch (error) {
    if (generation !== historyGeneration) return;
    $('#agent-history-progress').textContent = '';
    $('#agent-history-problem').hidden = false;
    $('#agent-history-problem').textContent = tx(String(error));
  }
}

export { requestForm, openRequest, loadAgentHistory };
