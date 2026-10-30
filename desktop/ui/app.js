const rawInvoke = window.ptaTransport.invoke;
// Also the answer operations: they do not depend on the profile files, so they neither check
// nor advance the form's revision.
const READS = ['snapshot', 'chat', 'github_repositories', 'self_chat_status', 'llm_providers', 'azure_cli_status', 'begin_azure_cli_login', 'open_azure_cli_login', 'finish_azure_cli_login', 'cancel_azure_cli_login', 'github_cli_status', 'audit', 'activity_status', 'activity_history', 'activity_pending', 'answer_refine', 'answer_send', 'list_azure_hus', 'list_github_epics', 'list_github_issues', 'get_azure_hu', 'get_azure_task', 'list_azure_tasks', 'get_github_epic', 'get_github_issue', 'open_work_item'];
// Revision of the profile files the last reply saw. The form's own revision (`current.revision`)
// changes only when it reloads or writes, so saving a stale form fails instead of overwriting
// changes made elsewhere (e.g. with `pta`).
let lastRevision = null;
async function invoke(method, args = {}) {
  if (window.ptaTransport.browser && method === 'activity_open') {
    showTab('registration');
    return { visible: true };
  }
  let result = await rawInvoke('command', { request: {
    method, args, revision: READS.includes(method) || /^(implementation_|machine_|variable_|crate_update_)/.test(method) ? null : current?.revision ?? null, contract: 1
  }});
  if (window.ptaTransport.browser && result.code === 'authorization_pending' && method === 'connect_microsoft') {
    result = await browserMicrosoftLogin(result.data.authorization_url);
  }
  if (result.code === 'authorization_pending' && ['connect_microsoft', 'finish_github_login'].includes(method)) {
    const finish = method === 'connect_microsoft' ? 'finish_microsoft' : 'finish_github_login';
    const deadline = Date.now() + 600_000;
    notify(t('Authorization pending in the browser.'), 'info');
    while (result.code === 'authorization_pending' && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 2000));
      result = await rawInvoke('command', { request: { method: finish, args: {}, revision: null, contract: 1 } });
    }
  }
  if (result.revision) lastRevision = result.revision;
  if (result.revision && current && !READS.includes(method) && !/^(implementation_|machine_|variable_|crate_update_)/.test(method)) current.revision = result.revision;
  if (!result.ok) throw result.message || result.code;
  return result.data;
}

const $ = (selector) => document.querySelector(selector);
/** Snapshot being edited by the forms (saved with «Save changes»). */
let current;
/** Latest persisted snapshot: drives status, overview and the test chat (which reads the saved map). */
let live;
/** How far this installation is from being a working member of the account (metadata only). */
let readiness = null;
let tab = 'overview';
let dirty = false;
let session = crypto.randomUUID();

function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null) continue;
    if (key === 'class') node.className = value;
    else if (key in node) node[key] = value;
    else node.setAttribute(key, value);
  }
  node.append(...children.flat().filter(child => child !== null && child !== undefined && child !== false));
  return node;
}
function icon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', 'icon');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.append(use);
  return svg;
}
const badge = (text, tone = 'muted') => el('span', { class: `badge ${tone}`, textContent: text });
function field(id, value) { $(id).value = value ?? ''; }
function lines(text) { return text.split('\n').map(s => s.trim()).filter(Boolean); }
function names(text) { return text.split(',').map(s => s.trim()).filter(Boolean); }
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

/** This device's theme (Settings › General): `light` or `dark`, or none to follow the system.
 *  It lives in `localStorage`, not in the profile, and applies before the first paint. */
const THEME_KEY = 'pta-theme';
function applyTheme(choice) {
  const theme = ['light', 'dark'].includes(choice) ? choice : null;
  if (theme) document.documentElement.dataset.theme = theme;
  else delete document.documentElement.dataset.theme;
  for (const button of document.querySelectorAll('#theme-choice button')) {
    button.setAttribute('aria-pressed', String(button.dataset.themeChoice === (theme ?? 'system')));
  }
  // The browser's bar takes the page's color: the chosen theme's, or each one's under the system.
  const page = getComputedStyle(document.documentElement).backgroundColor;
  for (const meta of document.querySelectorAll('meta[name="theme-color"]')) {
    meta.dataset.color ??= meta.content;
    meta.content = theme ? page : meta.dataset.color;
  }
}
try { applyTheme(localStorage.getItem(THEME_KEY)); } catch { applyTheme(null); }

let noticeTimer;
function notify(text, tone = 'info') {
  const box = $('#notice');
  $('#notice-text').textContent = tx(String(text?.message ?? text));
  box.className = `toast ${tone}`;
  // Errors interrupt a screen reader; confirmations and progress wait their turn.
  box.setAttribute('role', tone === 'error' ? 'alert' : 'status');
  box.hidden = false;
  clearTimeout(noticeTimer);
  if (tone !== 'error') noticeTimer = setTimeout(() => { box.hidden = true; }, 6000);
}
const done = text => notify(text, 'ok');
const fail = error => notify(error, 'error');

/** Runs one operation with the button showing progress; errors become a notice. */
async function busy(button, task) {
  if (button) { button.disabled = true; button.classList.add('busy'); }
  try { return await task(); }
  catch (error) { fail(error); }
  finally {
    if (button) { button.disabled = false; button.classList.remove('busy'); }
    renderLive();
    if (button?.id === 'activity-run') button.disabled = activityRunning;
    if (implementationForm) implementationReady(implementationForm);
    if (requestForm) implementationReady(requestForm);
  }
}

function showTab(name, anchor) {
  tab = name;
  clearRevealedVariable();
  setNavOverflow(false);
  for (const section of ['overview', 'todo', 'implementation', 'create-request', 'agent-history', 'settings', 'messages', 'knowledge', 'chat', 'registration']) $('#' + section).hidden = section !== name;
  for (const button of document.querySelectorAll('nav button[data-tab]')) {
    button.setAttribute('aria-current', button.dataset.tab === (['implementation', 'create-request'].includes(name) ? 'todo' : name) ? 'page' : 'false');
  }
  $('#nav-more').setAttribute('aria-current', ['knowledge', 'chat', 'agent-history'].includes(name) ? 'page' : 'false');
  if (name === 'messages') loadMessages().catch(fail);
  if (name === 'overview') loadActivity();
  if (name === 'registration') loadRegistration().catch(fail);
  if (name === 'knowledge') loadKnowledge().catch(fail);
  if (name === 'todo') loadTodo();
  if (name === 'agent-history') loadAgentHistory();
  if (name === 'settings') { loadMachines().catch(fail); loadVariables(); loadAccountConfig(); loadUpdates(); }
  const target = anchor && document.getElementById(anchor);
  // A panel inside a tab (a Settings section, an Activities view) opens that tab first.
  const owner = target && document.querySelector(`[aria-controls="${target.closest('[role=tabpanel]')?.id}"]`);
  if (owner) selectTab(owner.parentElement, owner.dataset.group);
  renderSavebar();
  renderActivityNotice();
  if (target) target.scrollIntoView({ behavior: 'smooth', block: 'start' });
  else window.scrollTo(0, 0);
}

function setNavOverflow(open) {
  $('nav').classList.toggle('overflow-open', open);
  $('#nav-more').setAttribute('aria-expanded', String(open));
}

// ---------------------------------------------------------------- To-Do

let todoRequest = 0;
function todoSources() {
  const sources = (live?.map.resources ?? []).filter(r => r.kind === 'tool' && r.tool.type === 'azure_devops_status' && r.enabled);
  const selected = $('#todo-source').value;
  $('#todo-source').replaceChildren(...sources.map(source => el('option', { value: source.id, textContent: source.description || source.id })));
  if (sources.some(source => source.id === selected)) $('#todo-source').value = selected;
  $('#todo-source-field').hidden = $('#todo-provider').value !== 'azure' || sources.length < 2;
  return sources;
}
function todoDate(name, raw) {
  const parsed = raw && new Date(raw);
  return el('div', {}, el('dt', { textContent: name }), el('dd', {}, parsed && !Number.isNaN(parsed.valueOf())
    ? el('time', { dateTime: raw, textContent: parsed.toLocaleDateString(uiLocale(), { dateStyle: 'medium' }) })
    : t('Not recorded')));
}
function todoOriginal(item, selection = null) {
  const implement = el('button', { type: 'button', class: 'icon-button secondary', title: t('Implement work item'), 'aria-label': t('Implement work item') }, icon('wrench'));
  implement.onclick = () => busy(implement, () => openImplementation(item, selection, implement));
  const label = t('Open #{id} in a new tab', { id: item.id });
  const open = window.ptaTransport.browser
    ? el('a', { class: 'icon-button secondary', href: item.url, target: '_blank', rel: 'noopener noreferrer', title: label, 'aria-label': label }, icon('link'))
    : el('button', { type: 'button', class: 'icon-button secondary', title: label, 'aria-label': label }, icon('link'));
  if (!window.ptaTransport.browser) open.onclick = () => busy(open, () => invoke('open_work_item', { url: item.url }));
  const copyButton = el('button', {
    type: 'button', class: 'icon-button secondary', title: t('Copy work item link'),
    'aria-label': t('Copy work item link'),
  }, icon('copy'));
  copyButton.onclick = () => busy(copyButton, async () => {
    if (await copy(item.url)) done(t('Link copied: {url}', { url: item.url }));
    else fail(t('Could not copy the work item link.'));
  });
  const session = el('button', { type: 'button', class: 'icon-button secondary', hidden: true, 'data-session-for': item.url, title: t('Open agent session'), 'aria-label': t('Open agent session') }, icon('chat'));
  return el('div', { class: 'todo-actions' }, implement, open, copyButton, session);
}
let todoDetailRequest = 0;
let todoSelection = null;
let todoListScroll = 0;
let todoReturnFocus = null;
const detailTrails = { todo: [], activity: [] };
function todoStateStyle(state) {
  const value = String(state ?? '').normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLowerCase();
  if (/\b(review|validation|validate|validating|verification|verify|testing|test|qa|uat|quality assurance|user acceptance|resolved|revision|revisando|validacion|validando|verificacion|pruebas|calidad)\b/.test(value)) return { class: 'review', tone: 'warn' };
  if (/\b(done|closed|complete|completed|finished|removed|terminado|finalizado|cerrado|completado|hecho|eliminado)\b/.test(value)) return { class: 'done', tone: 'ok' };
  if (/\b(active|in progress|in development|development|developing|doing|working|assigned|committed|started|en curso|en progreso|en desarrollo|en ejecucion|trabajando|activo|asignado|tomado)\b/.test(value)) return { class: 'in-progress', tone: 'active' };
  if (/\b(new|proposed|approved|to do|todo|backlog|not started|not taken|unassigned|sin asignar|sin tomar|por hacer|no iniciado|no comenzado|aprobado|propuesto|nuevo|nueva)\b/.test(value)) return { class: 'unstarted', tone: 'muted' };
  return { class: '', tone: 'info' };
}
function todoItem(item, onSelect = null, selection = null) {
  const stateStyle = todoStateStyle(item.state);
  const select = el('button', { type: 'button', class: 'todo-select', textContent: `#${item.id} · ${item.title}` });
  select.onclick = () => {
    if (onSelect) return onSelect(select);
    todoListScroll = window.scrollY;
    todoReturnFocus = select;
    todoSelection = { provider: $('#todo-provider').value, source: $('#todo-source').value, item, task: item.kind === 'Bug Desarrollo' };
    loadTodoDetail();
  };
  return el('article', { class: `todo-item ${stateStyle.class}`.trim() },
    el('div', { class: 'todo-heading' }, el('h3', {}, select), todoOriginal(item, selection)),
    el('div', { class: 'todo-context' }, badge(tx(item.state), stateStyle.tone), badge(tx(item.kind || 'HU'), 'muted'), el('span', { textContent: item.project })),
    el('dl', { class: 'todo-dates' }, todoDate(t('Created'), item.created_at), ...item.dates.map(date => todoDate(tx(date.name), date.value))));
}
function closeTodoDetail(restore = true) {
  if (restore && backRelatedItem('todo')) return;
  detailTrails.todo = [];
  ++todoDetailRequest;
  todoSelection = null;
  $('#todo-detail').hidden = true;
  $('#todo-detail-content').replaceChildren();
  $('#todo-master').hidden = false;
  if (restore) {
    todoReturnFocus?.focus({ preventScroll: true });
    window.scrollTo(0, todoListScroll);
  }
}
function todoSafeUrl(raw, base) {
  try {
    const url = new URL(raw, base);
    return url.protocol === 'https:' && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}
function appendLinkedText(text, parent) {
  let last = 0;
  for (const match of text.matchAll(/https:\/\/[^\s<>"\u0000-\u001f]+/g)) {
    let raw = match[0].replace(/[.,;:!?]+$/, '');
    while (raw.endsWith(')') && (raw.match(/\)/g) || []).length > (raw.match(/\(/g) || []).length) raw = raw.slice(0, -1);
    const url = todoSafeUrl(raw);
    parent.append(text.slice(last, match.index), url ? link(raw, url) : raw);
    last = match.index + raw.length;
  }
  parent.append(text.slice(last));
}
function looksLikeMarkdown(text) {
  return /^(?:#{1,6}\s+\S|\s*[-*+]\s+\S|\s*\d+[.)]\s+\S|\s*```|\|.+\|)/m.test(text);
}
/** Render the core's safe rich-text tree; provider HTML never enters the browser's parser. */
function todoRich(text, format, base, content) {
  if (format !== 'html') {
    if (format === 'markdown' || format === 'plainText' || looksLikeMarkdown(text)) return markdown(text);
    const plain = el('div', { class: 'todo-text' });
    appendLinkedText(text, plain);
    return plain;
  }
  if (!Array.isArray(content)) return el('div', { class: 'todo-text', textContent: text });
  const root = el('div', { class: 'md' });
  const allowed = new Set(['p', 'div', 'span', 'strong', 'b', 'em', 'i', 'u', 's', 'del', 'br', 'ul', 'ol', 'li', 'pre', 'code', 'blockquote', 'hr', 'table', 'thead', 'tbody', 'tr', 'th', 'td']);
  let remaining = 12000;
  let clipped = false;
  function rebuild(node, parent, depth = 0) {
    if (--remaining < 0 || depth > 40) { clipped = true; return; }
    if (node.tag === 'notice') { clipped = true; return; }
    if (node.tag === 'text') { appendLinkedText(node.text || '', parent); return; }
    if (node.tag === 'a' || node.tag === 'img') {
      const url = node.url && todoSafeUrl(node.url, base);
      const label = node.tag === 'img' ? node.text || t('View image') : richLabel(node) || url || '';
      parent.append(url ? link(label, url) : label);
      return;
    }
    const tag = /^h[1-6]$/.test(node.tag) ? 'h3' : allowed.has(node.tag) ? node.tag : null;
    if (!tag) return;
    if (tag === 'pre' && !node.children?.some(child => child.tag === 'code') && looksLikeMarkdown(richLabel(node))) {
      parent.append(markdown(richLabel(node)));
      return;
    }
    if (tag === 'code') { parent.append(el(tag, { textContent: richLabel(node) })); return; }
    if (tag === 'pre') { parent.append(el('pre', {}, el('code', { textContent: richLabel(node) }))); return; }
    const safe = el(tag);
    for (const child of node.children || []) rebuild(child, safe, depth + 1);
    parent.append(tag === 'table' ? el('div', { class: 'table' }, safe) : safe);
  }
  const richLabel = node => (node.text || '') + (node.children || []).map(richLabel).join('');
  for (const node of content) rebuild(node, root);
  if (clipped) root.append(el('p', { class: 'hint', textContent: t('This text is too long to display completely. Open the original item for the full content.') }));
  return root;
}
function todoValue(field, base) {
  const value = field.value;
  if (value === null || value === undefined || value === '') return el('span', { class: 'hint', textContent: t('Not recorded') });
  if (field.kind === 'dateTime') return todoDate(tx(field.name), value).lastChild.firstChild;
  const text = typeof value === 'object' ? value.displayName || value.uniqueName || JSON.stringify(value, null, 2) : String(value);
  if (field.kind === 'boolean') return el('span', { textContent: value ? t('Yes') : t('No') });
  if (todoSafeUrl(text, base) && /^https:\/\//.test(text)) return link(text, text);
  return todoRich(text, field.kind, base, field.content);
}
function todoFields(fields, base) {
  return el('dl', { class: 'todo-fields' }, ...fields.map(field => el('div', {}, el('dt', { textContent: tx(field.name) }), el('dd', {}, todoValue(field, base)))));
}
function todoFold(label, children, count) {
  return el('details', { class: 'panel fold-panel' }, el('summary', {}, label, count === undefined ? null : el('span', { class: 'count', textContent: count })), ...children);
}
function openRelatedItem(relation, button, prefix) {
  detailTrails[prefix].push({ selection: prefix === 'todo' ? todoSelection : activityTask,
    nodes: [...$(`#${prefix}-detail-content`).childNodes], scroll: window.scrollY, focus: button });
  if (prefix === 'todo') {
    todoSelection = { ...todoSelection, item: relation.item, task: relation.task };
    loadTodoDetail();
  } else {
    activityTask = { ...activityTask, item: relation.item, task: relation.task };
    loadActivityTask();
  }
}
function backRelatedItem(prefix) {
  const previous = detailTrails[prefix].pop();
  if (!previous) return false;
  if (prefix === 'todo') { ++todoDetailRequest; todoSelection = previous.selection; $('#todo-detail-problem').hidden = true; }
  else { ++activityDetailRequest; activityTask = previous.selection; }
  const content = $(`#${prefix}-detail-content`);
  content.replaceChildren(...previous.nodes);
  content.setAttribute('aria-busy', 'false');
  $(`#${prefix}-detail-progress`).textContent = '';
  $(`#${prefix}-detail-refresh`).disabled = false;
  previous.focus.focus({ preventScroll: true });
  window.scrollTo(0, previous.scroll);
  return true;
}
function todoRelations(detail, prefix) {
  const list = el('div', { class: 'todo-list' });
  const count = el('span', { class: 'count' });
  const select = el('select', {}, el('option', { value: '', textContent: t('All types') }),
    ...[...new Set(detail.relations.map(relation => relation.item?.kind).filter(Boolean))].sort().map(kind => el('option', { value: kind, textContent: tx(kind) })),
    el('option', { value: '__links', textContent: t('Other links') }));
  const render = () => {
    const relations = detail.relations.filter(relation => !select.value || (select.value === '__links' ? !relation.item : relation.item?.kind === select.value));
    count.textContent = `${relations.length} / ${detail.relations.length}`;
    list.replaceChildren(...relations.map(relation => {
      if (relation.item && (relation.task || relation.item.url.startsWith('https://github.com/'))) {
        const parentSelection = prefix === 'todo' ? todoSelection : activityTask;
        const card = todoItem(relation.item, button => openRelatedItem(relation, button, prefix), { source: parentSelection?.source, task: relation.task });
        card.append(badge(tx(relation.name), 'muted'));
        return card;
      }
      return el('div', { class: 'todo-relation' }, badge(tx(relation.name), 'muted'),
        relation.url && todoSafeUrl(relation.url, detail.item.url) ? link(relation.detail || relation.url, relation.url) : el('span', { textContent: relation.detail }));
    }));
    if (!relations.length) list.append(el('p', { class: 'hint', textContent: t('No linked items to show.') }));
  };
  select.onchange = render;
  render();
  return el('section', { class: 'panel' },
    el('div', { class: 'panel-head' }, el('h2', { textContent: t('Links and evidence') }), count),
    el('label', { class: 'todo-relation-filter' }, t('Work item type'), select), list);
}
function renderTodoDetail(detail, prefix = 'todo') {
  const item = detail.item;
  const base = item.url;
  const prominent = new Set(['System.AssignedTo', 'assignees', 'System.AreaPath', 'System.IterationPath', 'System.Tags', 'Microsoft.VSTS.Common.Priority', 'Microsoft.VSTS.Common.Severity', 'labels', 'milestone', 'milestone_due', 'user']);
  const shown = detail.fields.filter(field => prominent.has(field.reference));
  if (!shown.some(field => ['System.AssignedTo', 'assignees'].includes(field.reference))) shown.unshift({ reference: 'System.AssignedTo', name: 'Assigned To', kind: 'string', value: null });
  const content = detail.fields.filter(field => field.reference !== 'System.History' && ((field.value && ['html', 'markdown', 'plainText'].includes(field.kind)) || /description|acceptancecriteria|reprosteps/i.test(field.reference) || (/evidence/i.test(field.reference) && typeof field.value === 'string')));
  const contentOrder = ['System.Description', 'body', 'Microsoft.VSTS.Common.AcceptanceCriteria', 'Microsoft.VSTS.TCM.ReproSteps', 'Microsoft.VSTS.TCM.SystemInfo'];
  const contentRank = field => contentOrder.includes(field.reference) ? contentOrder.indexOf(field.reference) : contentOrder.length;
  content.sort((a, b) => contentRank(a) - contentRank(b) || a.name.localeCompare(b.name));
  const header = el('div', { class: 'panel todo-detail-header' },
    el('div', { class: 'todo-context' }, badge(tx(item.kind), 'muted'), badge(tx(item.state), todoStateStyle(item.state).tone), el('span', { textContent: item.project })),
    el('div', { class: 'todo-heading' }, el('h2', { id: `${prefix}-detail-title`, tabIndex: -1, textContent: `#${item.id} · ${item.title}` }), todoOriginal(item, prefix === 'todo' ? todoSelection : { ...activityTask, provider: 'azure', task: activityTask?.task !== false })),
    todoFields(shown, base),
    el('dl', { class: 'todo-dates' }, todoDate(t('Created'), item.created_at), ...item.dates.map(date => todoDate(tx(date.name), date.value))));
  const panels = content.map(field => el('section', { class: 'panel' }, el('h2', { textContent: tx(field.name) }), todoValue(field, base)));
  const comments = detail.comments.map(comment => el('article', { class: 'todo-comment' },
    el('div', { class: 'todo-context' }, el('strong', { textContent: comment.author || t('Unknown author') }), comment.created_at ? el('time', { dateTime: comment.created_at, textContent: new Date(comment.created_at).toLocaleString(uiLocale(), { dateStyle: 'medium', timeStyle: 'short' }) }) : null,
      comment.modified_at && comment.modified_at !== comment.created_at ? el('span', { textContent: t('Edited') }) : null), todoRich(comment.text, comment.format, base, comment.content)));
  const history = detail.history.map(change => el('article', { class: 'todo-comment' }, el('div', { class: 'todo-context' }, el('strong', { textContent: change.author || t('Unknown author') }), change.at ? el('time', { dateTime: change.at, textContent: new Date(change.at).toLocaleString(uiLocale(), { dateStyle: 'medium', timeStyle: 'short' }) }) : null),
    todoFields(change.fields.map(field => ({ ...field, kind: 'string', value: typeof field.value === 'object' && field.value !== null && ('newValue' in field.value || 'oldValue' in field.value) ? `${todoHistoryValue(field.value.oldValue)} → ${todoHistoryValue(field.value.newValue)}` : field.value })), base)));
  $(`#${prefix}-detail-content`).replaceChildren(header,
    el('section', { class: 'panel', hidden: true, 'data-sessions-for': item.url }),
    ...(detail.partial ? [ el('div', { class: 'callout warn', role: 'status' }, ...detail.warnings.map(warning => el('p', { textContent: tx(warning) })))] : []),
    ...panels,
    el('section', { class: 'panel' }, el('div', { class: 'panel-head' }, el('h2', { textContent: t('Comments') }), el('span', { class: 'count', textContent: comments.length })), ...(comments.length ? comments : [el('p', { class: 'hint', textContent: t('No comments to show.') })])),
    todoRelations(detail, prefix),
    todoFold(t('All fields'), [todoFields(detail.fields, base)], detail.fields.length),
    todoFold(t('Change history'), history.length ? history : [el('p', { class: 'hint', textContent: t('No history to show.') })], history.length));
  loadWorkItemSessions().catch(fail);
}
function todoHistoryValue(value) {
  if (value === undefined || value === null) return t('Not recorded');
  return typeof value === 'object' ? value.displayName || value.uniqueName || JSON.stringify(value) : String(value);
}
async function loadTodoDetail() {
  if (!todoSelection) return;
  const selected = todoSelection;
  const request = ++todoDetailRequest;
  $('#todo-master').hidden = true;
  $('#todo-detail').hidden = false;
  $('#todo-detail-content').replaceChildren();
  $('#todo-detail-content').setAttribute('aria-busy', 'true');
  $('#todo-detail-problem').hidden = true;
  $('#todo-detail-progress').textContent = t('Loading detail…');
  $('#todo-detail-refresh').disabled = true;
  $('#todo-back').focus({ preventScroll: true });
  $('#todo-detail').scrollIntoView({ block: 'start' });
  try {
    const args = { id: selected.item.id, project: selected.item.project };
    if (selected.provider === 'azure') args.source = selected.source;
    const detail = await invoke(selected.provider === 'azure' ? selected.task ? 'get_azure_task' : 'get_azure_hu' : selected.item.kind === 'Epic' ? 'get_github_epic' : 'get_github_issue', args);
    if (request !== todoDetailRequest) return;
    renderTodoDetail(detail);
    $('#todo-detail-title').focus({ preventScroll: true });
    $('#todo-detail-progress').textContent = '';
  } catch (error) {
    if (request !== todoDetailRequest) return;
    $('#todo-detail-progress').textContent = '';
    $('#todo-detail-problem').hidden = false;
    $('#todo-detail-problem').textContent = tx(String(error?.message ?? error));
    $('#todo-detail-content').replaceChildren(el('p', { class: 'hint', textContent: t('Open the original item or refresh to try again.') }), todoOriginal(selected.item));
  } finally {
    if (request === todoDetailRequest) {
      $('#todo-detail-content').setAttribute('aria-busy', 'false');
      $('#todo-detail-refresh').disabled = false;
    }
  }
}
async function loadTodo() {
  closeTodoDetail(false);
  const request = ++todoRequest;
  const azure = $('#todo-provider').value === 'azure';
  const sources = todoSources();
  $('#todo-title').textContent = azure ? t('My HUs and bugs') : $('#todo-github-kind').value === 'all' ? t('My issues') : t('My epics');
  $('#todo-kind-field').hidden = !azure;
  $('#todo-github-kind-field').hidden = azure;
  $('#todo-list').replaceChildren();
  $('#todo-list').setAttribute('aria-busy', 'true');
  $('#todo-progress').textContent = t('Loading…');
  $('#todo-assignment').textContent = '';
  $('#todo-problem').hidden = true;
  $('#todo-warnings').hidden = true;
  $('#todo-refresh').disabled = true;
  try {
    if (azure && !sources.length) throw t('Configure an Azure DevOps activity source in Knowledge to see your HUs.');
    const args = { limit: Number($('#todo-limit').value) };
    if (azure) {
      args.source = $('#todo-source').value;
      if ($('#todo-kind').value) args.kind = $('#todo-kind').value;
    }
    if ($('#todo-state').value) args.state = $('#todo-state').value;
    const result = await invoke(azure ? 'list_azure_hus' : $('#todo-github-kind').value === 'all' ? 'list_github_issues' : 'list_github_epics', args);
    if (request !== todoRequest) return;
    const selected = $('#todo-state').value;
    $('#todo-state').replaceChildren(el('option', { value: '', textContent: t('All states') }), ...result.states.map(state => el('option', { value: state, textContent: tx(state) })));
    $('#todo-state').value = selected;
    $('#todo-assignment').textContent = azure
      ? t('Your assignment fields: {fields}', { fields: result.assignment_fields.join(', ') })
      : $('#todo-github-kind').value === 'all' ? t('Issues assigned to your connected GitHub account.') : t('Issues of type Epic assigned to your connected GitHub account. GitHub provides open and closed states; unavailable dates stay empty.');
    $('#todo-list').replaceChildren(...result.items.map(item => todoItem(item)));
    loadWorkItemSessions().catch(fail);
    if (!result.items.length) $('#todo-list').append(el('p', { class: 'empty', textContent: t('No items match these filters. Try another state.') }));
    $('#todo-progress').textContent = result.has_more
      ? t('Showing the {n} newest items. Increase the limit to see more.', { n: result.items.length })
      : tn(result.items.length, '{n} item', '{n} items');
    if (result.partial) {
      $('#todo-warnings').hidden = false;
      $('#todo-warnings').textContent = t('Some projects could not be read. The list and available states may be incomplete.');
    }
  } catch (error) {
    if (request !== todoRequest) return;
    $('#todo-progress').textContent = '';
    $('#todo-problem').hidden = false;
    $('#todo-problem').textContent = tx(String(error?.message ?? error));
  } finally {
    if (request === todoRequest) {
      $('#todo-list').setAttribute('aria-busy', 'false');
      $('#todo-refresh').disabled = false;
    }
  }
}

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
    for (const [value, label] of [['hu', t('HU')], ['bug', t('Bug')]]) {
      const radio = el('input', { type: 'radio', name: 'request-kind', value, checked: value === ($('#todo-kind').value || 'hu') });
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
  const destination = sessionDestination(run, sessionIsMobile(), !window.ptaTransport.browser);
  open.disabled = !destination.url && !destination.native && !destination.connect;
  if (open.disabled) open.title = destination.reason;
  const pairs = [[t('Run ID'), run.id], [t('Agent session ID'), run.session_id || t('Not recorded')],
    [t('Execution machine'), run.machine_name || run.machine], [t('Effort'), effortNames[run.effort] || run.effort || t('Not recorded')],
    ...[[t('Requested at'), run.created_at], [t('Started at'), run.started_at], [t('Finished at'), run.finished_at]].map(([key, value]) => [key, value ? new Date(value).toLocaleString(uiLocale()) : t('Not recorded')])];
  const copySession = el('button', { type: 'button', class: 'secondary small', textContent: t('Copy session ID'), disabled: !run.session_id });
  copySession.onclick = () => busy(copySession, async () => { if (!await copy(run.session_id)) fail(t('Could not copy the session ID.')); });
  const card = el('article', { class: 'implementation-run', 'data-run-id': run.id },
    el('div', { class: 'todo-context' }, badge(run.purpose === 'request' && run.state === 'created' ? t('Request created') : tx(run.state), ['created', 'done', 'plan_ready'].includes(run.state) ? 'ok' : ['failed', 'uncertain', 'needs_attention'].includes(run.state) ? 'warn' : 'info'),
      el('strong', { textContent: `${providerNames[run.provider] || run.provider} · ${run.model}` }), run.purpose === 'request' ? badge(t('Request creation'), 'muted') : badge(t('Implementation'), 'muted')),
    run.project ? el('p', { textContent: `${run.project} · ${{ hu: t('HU'), bug: t('Bug'), epic: t('Epic') }[run.kind] || tx(run.kind)}` }) : null,
    run.item?.url ? link(`#${run.item.id} · ${run.item.title}`, run.item.url) : null,
    el('dl', { class: 'todo-fields' }, ...pairs.map(([label, value]) => el('div', {}, el('dt', { textContent: label }), el('dd', { textContent: value })))),
    el('div', { class: 'actions' }, open, copySession),
    open.disabled ? el('p', { class: 'hint', textContent: destination.reason }) : null,
    run.output ? el('p', { class: 'hint', textContent: tx(run.output) }) : null,
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
    $('#agent-history-progress').textContent = result.partial ? t('Showing the latest executions. Older records remain saved.') : tn(result.runs.length, '{count} execution', '{count} executions');
  } catch (error) {
    if (generation !== historyGeneration) return;
    $('#agent-history-progress').textContent = '';
    $('#agent-history-problem').hidden = false;
    $('#agent-history-problem').textContent = tx(String(error));
  }
}

// ---------------------------------------------------------------- implementation
function sessionDestination(session, mobile, native = false) {
  if (!session.session_id) return { reason: t('The agent has not reported its session ID yet.') };
  if (['codex', 'claude'].includes(session.provider) && !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(session.session_id)) return { reason: t('This session ID does not have a supported app link.') };
  if (mobile && ['codex', 'claude'].includes(session.provider)) return { connect: true };
  if (session.provider === 'codex') return { url: `codex://threads/${encodeURIComponent(session.session_id)}` };
  if (session.provider === 'claude') {
    if (native) return ['running', 'planning', 'generating', 'sending'].includes(session.state) ? { reason: t('Claude Desktop can open this local session after the CLI finishes. This session is still running.') } : { native: true };
    return { connect: true };
  }
  return { reason: t('This agent does not provide a verified app link for the recorded local session. Its session ID is available in implementation runs.') };
}
function sessionIsMobile() {
  return /Android|iPhone|iPad|iPod/i.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
}
function mobileSessionLink(label, url) {
  return el('a', { class: 'button secondary', href: url, target: '_blank', rel: 'noopener noreferrer', textContent: label });
}

function showMobileSession(session, trigger) {
  const dialog = el('dialog', { class: 'directory-picker', 'aria-label': t('Open agent session') });
  const close = el('button', { type: 'button', class: 'icon-button secondary', 'aria-label': t('Close') }, icon('close'));
  const body = el('div', { class: 'directory-picker-body mobile-session-body' });
  const connect = el('button', { type: 'button', class: 'secondary', textContent: t('Connect mobile session') });
  const operation = action => invoke('implementation_mobile_session', { machine: session.machine, id: session.id, action, confirmed: true });
  const render = result => {
    if (!dialog.isConnected) return;
    session.remote = result.remote;
    if (session.provider === 'claude') {
      const url = result.remote?.url;
      if (!/^https:\/\/claude\.ai\/code\/session_[a-zA-Z0-9_]+$/.test(url || '')) throw new Error(t('The provider did not return a supported remote session URL.'));
      const nativeUrl = `claude://code/${encodeURIComponent(url.split('/').pop())}`;
      const stop = el('button', { type: 'button', class: 'secondary', textContent: t('Disconnect mobile session') });
      stop.onclick = () => busy(stop, async () => { await operation('close'); dialog.close(); });
      body.replaceChildren(...[el('p', { class: 'hint', textContent: t('This session is available in your Claude account while the execution machine stays online. The connection lasts up to two hours; reconnect here when needed.') }),
        el('div', { class: 'actions' }, mobileSessionLink(t('Open in app'), nativeUrl), mobileSessionLink(t('Open on web'), url)),
        el('p', { class: 'hint', textContent: t('If the app is missing or does not open, use the web link. Sign in with the same provider account as on the execution machine.') }), ['running', 'planning', 'generating'].includes(session.state) ? null : stop].filter(Boolean));
    } else {
      const pair = el('button', { type: 'button', class: 'secondary', textContent: t('Pair this phone with Codex') });
      const pairing = el('div', { role: 'status' });
      pair.onclick = () => busy(pair, async () => {
        const result = await operation('pair');
        if (!dialog.isConnected) return;
        const value = result.pairing;
        const code = el('code', { textContent: value.manual_code });
        const copyCode = el('button', { type: 'button', class: 'secondary small', textContent: t('Copy pairing code') });
        copyCode.onclick = () => busy(copyCode, async () => { if (!await copy(value.manual_code)) fail(t('Could not copy the pairing code.')); });
        pairing.replaceChildren(el('p', { class: 'hint', textContent: t('In the ChatGPT app, open Codex, choose to add a computer and enter this code. It expires at {time}.', { time: new Date(value.expires_at * 1000).toLocaleTimeString(uiLocale()) }) }), el('div', { class: 'actions' }, code, copyCode));
      });
      const remoteId = result.remote?.session_id || session.session_id;
      const copySession = el('button', { type: 'button', class: 'secondary small', textContent: t('Copy session ID') });
      copySession.onclick = () => busy(copySession, async () => { if (!await copy(remoteId)) fail(t('Could not copy the session ID.')); });
      body.replaceChildren(...[el('p', { textContent: t('Codex Remote Control is connected to {machine}.', { machine: result.remote?.server_name || session.machine_name || session.machine }) }),
        el('p', { class: 'hint', textContent: t('Open Codex in the ChatGPT mobile app, select this computer, then select the session. A direct mobile link to this local session is not available.') }),
        remoteId !== session.session_id ? el('p', { class: 'hint', textContent: t('This older CLI session has a mobile copy with its conversation history. Continue in that copy; the original session is kept.') }) : null,
        el('p', {}, el('strong', { textContent: t('Mobile session ID') }), ' ', el('code', { textContent: remoteId })), copySession,
        el('details', { class: 'fold' }, el('summary', { textContent: t('This computer is not listed in Codex') }), el('p', { class: 'hint', textContent: t('Your phone only needs this once: if the computer already appears in Codex, ignore this.') }), pair, pairing)].filter(Boolean));

    }
  };
  connect.onclick = () => busy(connect, async () => render(await operation('connect')));
  close.onclick = () => dialog.close();
  dialog.onclose = () => { document.body.classList.remove('directory-picker-open'); dialog.remove();
    const focus = trigger.isConnected ? trigger : [...document.querySelectorAll('[data-run-id]')].find(card => card.dataset.runId === session.id)?.querySelector('button');
    focus?.focus({ preventScroll: true }); };
  dialog.append(el('div', { class: 'directory-picker-head' }, el('h2', { textContent: t('Open agent session') }), close), body);
  body.append(el('p', { class: 'hint', textContent: t('Connecting the recorded session through its execution machine…') }), connect);
  document.body.append(dialog); document.body.classList.add('directory-picker-open'); dialog.showModal();
  connect.onclick();
}
function openAgentSession(session, button) {
  const destination = sessionDestination(session, sessionIsMobile(), !window.ptaTransport.browser);
  if (destination.connect) return showMobileSession(session, button);
  if (destination.native) return busy(button, () => invoke('implementation_open_session', { machine: session.machine, id: session.id }));
  if (!destination.url) { notify(destination.reason, 'info'); return; }
  if (!window.ptaTransport.browser) return busy(button, () => invoke('implementation_open_session', { machine: session.machine, id: session.id }));
  window.location.assign(destination.url);
}

let workItemSessionsRequest = 0;
async function loadWorkItemSessions() {
  const buttons = [...document.querySelectorAll('[data-session-for]')];
  const urls = [...new Set(buttons.map(button => button.dataset.sessionFor))].slice(0, 100);
  if (!urls.length) return;
  const request = ++workItemSessionsRequest;
  const result = await invoke('implementation_sessions', { urls });
  if (request !== workItemSessionsRequest) return;
  for (const button of buttons) {
    const sessions = (result.sessions || []).filter(session => session.item.url === button.dataset.sessionFor && session.session_id);
    const session = sessions.find(session => ['running', 'planning', 'generating', 'sending'].includes(session.state)) || sessions[0];
    button.hidden = !session;
    if (session) {
      button.title = t('Open {provider} session', { provider: providerNames[session.provider] || session.provider });
      button.setAttribute('aria-label', button.title);
      button.onclick = () => openAgentSession(session, button);
    }
  }
  for (const section of document.querySelectorAll('[data-sessions-for]')) {
    const sessions = (result.sessions || []).filter(session => session.item.url === section.dataset.sessionsFor);
    section.hidden = !sessions.length;
    section.replaceChildren(el('h2', { textContent: t('Agent sessions') }), ...sessions.map(session => {
      const open = el('button', { type: 'button', class: 'secondary small', textContent: t('Open agent session') });
      open.onclick = () => openAgentSession(session, open);
      return el('article', { class: 'implementation-run', 'data-run-id': session.id }, el('strong', { textContent: `${providerNames[session.provider] || session.provider} · ${session.model}` }),
        badge(session.online === false ? t('Machine offline. Last reported state: {state}', { state: tx(session.state) }) : tx(session.state), session.state === 'done' ? 'ok' : 'info'),
        el('code', { textContent: session.session_id || t('The agent has not reported its session ID yet.') }),
        el('dl', { class: 'todo-fields' }, ...[[t('Started at'), session.started_at], [t('Finished at'), session.finished_at]].map(([label, value]) => el('div', {}, el('dt', { textContent: label }), el('dd', { textContent: value ? new Date(value).toLocaleString(uiLocale()) : t('Not recorded') })))), open);
    }));
  }
}
setInterval(() => { if (tab === 'agent-history') loadAgentHistory(); }, 10000);
setInterval(() => { if (['todo', 'registration'].includes(tab)) loadWorkItemSessions().catch(fail); }, 10000);
let implementationForm = null;
let implementationRequest = 0;
function implementationSelection(item, selection) {
  const provider = item.url.startsWith('https://github.com/') ? 'github' : 'azure';
  return { provider, id: item.id, project: item.project, source: provider === 'azure' ? selection?.source || $('#todo-source').value || undefined : undefined,
    task: provider === 'azure' && (selection?.task ?? ['Task', 'Bug Desarrollo', 'Documentación', 'Tarea de Prueba'].includes(item.kind)) };
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
      tab = previous.tab; $(`#${tab}`).hidden = false;
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
/** Shared directory sheet: selections are staged until Apply, including across navigation. */
function chooseDirectories({ trigger, title, selected = [], multiple = true, allowEmpty = false, browse, apply }) {
  const selection = new Set(selected);
  const dialog = el('dialog', { class: 'directory-picker', 'aria-label': title });
  const back = el('button', { type: 'button', class: 'icon-button secondary', 'aria-label': t('Parent directory') }, icon('back'));
  const close = el('button', { type: 'button', class: 'icon-button secondary', 'aria-label': t('Cancel') }, icon('close'));
  const body = el('div', { class: 'directory-picker-body' });
  const count = el('span', { class: 'hint', role: 'status' });
  const use = el('button', { type: 'button', textContent: t('Use selected directories') });
  let request = 0;
  const update = () => {
    count.textContent = tn(selection.size, '{n} directory selected', '{n} directories selected');
    use.disabled = !allowEmpty && !selection.size;
  };
  const row = (path, current = false) => {
    const check = el('input', { type: 'checkbox', checked: selection.has(path), 'aria-label': t('Select directory {path}', { path }) });
    check.onchange = () => {
      if (!multiple) { selection.clear(); for (const input of body.querySelectorAll('input')) if (input !== check) input.checked = false; }
      if (check.checked) selection.add(path); else selection.delete(path);
      update();
    };
    const name = el('span', { textContent: current ? t('Select this directory') : path, title: path });
    const open = current ? null : el('button', { type: 'button', class: 'link-button', 'aria-label': t('Browse directory {path}', { path }) }, name);
    if (!current) open.onclick = () => load(path);
    return el('div', { class: 'directory-picker-row' }, el('label', { class: 'check' }, check, current ? name : icon('folder')), open);
  };
  const load = async path => {
    const generation = ++request;
    back.disabled = true;
    body.setAttribute('aria-busy', 'true');
    body.replaceChildren(el('p', { class: 'hint', role: 'status', textContent: t('Loading…') }));
    try {
      const result = await browse(path);
      if (!dialog.open || generation !== request) return;
      back.disabled = Boolean(result.roots);
      back.onclick = () => load(result.parent || '');
      body.replaceChildren(...[el('p', { class: 'directory-picker-location', textContent: result.path || t('Allowed roots') }),
        el('div', { class: 'directory-picker-list' },
          result.path ? el('div', { class: 'directory-picker-row' }, el('button', { type: 'button', class: 'link-button directory-picker-parent', translate: false, textContent: '../', 'aria-label': t('Parent directory'), onclick: () => load(result.parent || '') })) : null,
          result.path ? row(result.path, true) : null, ...result.directories.map(path => row(path))),
        result.directories.length ? null : el('p', { class: 'hint', textContent: t('No subdirectories.') })].filter(Boolean));
    } catch (error) {
      if (!dialog.open || generation !== request) return;
      const retry = el('button', { type: 'button', class: 'secondary', textContent: t('Retry') });
      retry.onclick = () => load(path);
      back.disabled = !path;
      back.onclick = () => load('');
      body.replaceChildren(el('p', { class: 'callout error', role: 'alert', textContent: tx(String(error?.message ?? error)) }), retry);
    } finally {
      if (dialog.open && generation === request) { body.setAttribute('aria-busy', 'false'); update(); }
    }
  };
  close.onclick = () => dialog.close();
  use.onclick = () => { apply([...selection]); dialog.close(); };
  dialog.onclose = () => { ++request; dialog.remove(); document.body.classList.toggle('directory-picker-open', Boolean(document.querySelector('dialog[open]'))); trigger?.focus({ preventScroll: true }); };
  dialog.append(el('div', { class: 'directory-picker-head' }, back, el('h2', { textContent: title }), close), body,
    el('div', { class: 'directory-picker-footer' }, count, use));
  document.body.append(dialog);
  document.body.classList.add('directory-picker-open');
  update(); dialog.showModal(); close.focus(); load('');
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
    const panel = el('article', { class: 'implementation-run' }, badge(tx(run.state), ['done', 'completed', 'created'].includes(run.state) ? 'ok' : 'info'), el('strong', { textContent: `${providerNames[run.provider] || run.provider || ''} · ${run.model || ''}` }),
      el('dl', { class: 'todo-fields' }, ...[[t('Requested at'), run.created_at], [t('Started at'), run.started_at], [t('Finished at'), run.finished_at]].map(([label, value]) => el('div', {}, el('dt', { textContent: label }), el('dd', { textContent: value ? new Date(value).toLocaleString(uiLocale()) : t('Not recorded') })))));
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
        const children = run.plan.children.map(child => {
          const selected = el('input', { type: 'checkbox', checked: true });
          const title = el('input', { value: child.title, maxLength: 200 });
          const description = el('textarea', { value: child.description, rows: 3, maxLength: 16000 });
          panel.append(el('fieldset', {}, el('legend', {}, el('label', { class: 'check' }, selected, t('Create this child'))), el('label', {}, t('Title'), title), el('label', {}, t('Description'), description)));
          return { selected, title, description };
        });
        const create = el('button', { type: 'button', class: 'secondary', textContent: t('Create selected children') });
        let reviewed = false;
        panel.addEventListener('input', () => { reviewed = false; create.textContent = t('Create selected children'); });
        create.onclick = () => busy(create, async () => {
          if (!reviewed) { reviewed = true; create.textContent = t('Confirm child creation'); return; }
          const chosen = children.filter(c => c.selected.checked).map(c => ({ title: c.title.value, description: c.description.value }));
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
setInterval(() => {
  const form = implementationForm;
  if (tab === 'implementation' && form && !form.runs.contains(document.activeElement) && !form.runs.querySelector('input, textarea')) implementationRuns(form).catch(fail);
}, 5000);

let machineRoots = [];
function renderMachineRoots() {
  $('#machine-roots').replaceChildren(...machineRoots.map(path => {
    const remove = el('button', { type: 'button', class: 'secondary small', disabled: $('#machine-roots-choose').disabled, textContent: t('Remove'), 'aria-label': t('Remove directory {path}', { path }) });
    remove.onclick = () => { machineRoots = machineRoots.filter(root => root !== path); renderMachineRoots(); };
    return el('div', { class: 'implementation-path' }, el('code', { textContent: path }), remove);
  }));
}
function renderTeamsMachines(fleet) {
  for (const id of ['machine-list', 'teams-machine-list']) {
    const list = $('#' + id);
    list.replaceChildren(...fleet.machines.map(machine => {
      const radio = el('input', { type: 'radio', name: id, value: machine.id, checked: machine.id === fleet.teams_machine, disabled: machine.trust === 'revoked' || (!machine.online && machine.id !== fleet.teams_machine) });
      radio.onchange = () => busy(null, async () => {
        const radios = [...document.querySelectorAll('.machine-choice input')];
        for (const input of radios) { input.disabled = true; input.checked = input.value === machine.id; }
        try {
          const result = await invoke('machine_teams', { machine: machine.id });
          done(result.handover_pending ? t('Teams machine selected. Waiting for the previous machine to stop.') : t('Teams machine selected. Start the assistant on that machine.'));
        } finally {
          // Refresh ownership alone so a Teams choice never discards unsaved machine settings.
          try { renderTeamsMachines(await invoke('machine_list')); }
          catch (error) { renderTeamsMachines(fleet); throw error; }
          if (tab === 'settings' && !list.closest('[hidden]')) list.querySelector('input:checked')?.focus({ preventScroll: true });
        }
      });
      const phase = machine.assistant_phase;
      const known = ['running', 'stopped', 'starting', 'stopping'].includes(phase);
      const running = phase === 'running';
      const phaseText = { running: t('Running'), stopped: t('Stopped'), starting: t('Starting…'), stopping: t('Stopping…') }[phase] || t('Status unknown');
      const action = running ? 'stop' : 'start';
      const control = el('button', { type: 'button', class: 'secondary small', textContent: running ? t('Stop') : t('Start'),
        'aria-label': running ? t('Stop assistant on {machine}', { machine: machine.name }) : t('Start assistant on {machine}', { machine: machine.name }),
        disabled: machine.trust === 'revoked' || !machine.online || !known || ['starting', 'stopping'].includes(phase) || (!running && machine.id !== fleet.teams_machine) });
      control.onclick = () => busy(control, async () => {
        for (const button of document.querySelectorAll('.machine-choice button')) button.disabled = true;
        try { await invoke('machine_assistant', { machine: machine.id, action }); }
        finally { renderTeamsMachines(await invoke('machine_list')); }
      });
      const manage = el('button', { type: 'button', class: 'secondary small', textContent: t('Manage machine'), 'aria-label': t('Manage machine: {machine}', { machine: machine.name }), 'data-machine-manage': machine.id });
      manage.onclick = () => busy(manage, () => editAccountMachine(machine, manage));
      return el('div', { class: 'machine-choice' },
        el('label', { class: 'check' }, radio, el('strong', { textContent: machine.name }), el('span', { textContent: machine.os })),
        badge(machine.trust === 'revoked' ? t('Revoked') : machine.presence === 'unknown' ? t('Status unknown') : machine.online ? t('Online') : t('Offline'), machine.online && machine.trust !== 'revoked' ? 'ok' : 'muted'),
        el('span', { class: 'hint', textContent: machine.online ? phaseText : t('Last reported assistant state: {state}', { state: phaseText }) }), el('div', { class: 'machine-actions' }, control, manage));
    }));
  }
}
async function editAccountMachine(machine, trigger) {
  const detail = await invoke('machine_detail', { machine: machine.id });
  const dialog = el('dialog', { class: 'directory-picker', 'aria-label': t('Manage machine') });
  const close = el('button', { type: 'button', class: 'secondary', textContent: t('Close') });
  const name = el('input', { value: detail.name, maxLength: 100, disabled: detail.trust === 'revoked' });
  const roots = el('div');
  let selected = [...(detail.roots || [])], changedRoots = false;
  const render = () => roots.replaceChildren(...selected.map(path => el('p', {}, el('code', { textContent: path }))));
  render();
  const choose = el('button', { type: 'button', class: 'secondary', textContent: t('Choose directories'),
    disabled: !detail.online || !detail.capabilities?.directories || detail.trust === 'revoked' });
  choose.onclick = () => chooseDirectories({ trigger: choose, title: t('Allowed directory roots'), selected, allowEmpty: true,
    browse: path => invoke('machine_directories', { machine: machine.id, path }),
    apply: paths => { selected = paths; changedRoots = true; render(); } });
  const save = el('button', { type: 'button', textContent: t('Save changes'), disabled: detail.trust === 'revoked' });
  const refresh = async () => renderTeamsMachines(await invoke('machine_list'));
  save.onclick = () => busy(save, async () => {
    try {
      await invoke('machine_update', { machine: machine.id, name: name.value, ...(changedRoots ? { roots: selected } : {}) });
      dialog.close(); done(t('Machine settings saved.'));
    } finally { await refresh(); }
  });
  const revoke = el('button', { type: 'button', class: 'danger', textContent: t('Revoke machine'), disabled: detail.local || detail.trust === 'revoked' });
  let confirmed = false;
  revoke.onclick = () => busy(revoke, async () => {
    if (!confirmed) { confirmed = true; revoke.textContent = t('Confirm revocation'); return; }
    try { await invoke('machine_revoke', { machine: machine.id }); dialog.close(); done(t('Machine revoked.')); }
    finally { await refresh(); }
  });
  close.onclick = () => dialog.close();
  dialog.onclose = () => { dialog.remove(); document.body.classList.toggle('directory-picker-open', Boolean(document.querySelector('dialog[open]'))); const target = trigger?.isConnected ? trigger : document.querySelector(`#${trigger?.closest('[id]')?.id || 'machine-list'} [data-machine-manage="${machine.id}"]`); target?.focus({ preventScroll: true }); };
  dialog.append(el('div', { class: 'directory-picker-head' }, el('h2', { textContent: t('Manage machine') }), close),
    el('div', { class: 'directory-picker-body' },
      el('p', { textContent: `${detail.os} · ${detail.presence === 'online' ? t('Online') : detail.presence === 'offline' ? t('Offline') : t('Status unknown')}` }),
      el('p', { class: 'hint', textContent: detail.last_seen ? t('Last seen: {time}', { time: new Date(detail.last_seen * 1000).toLocaleString(uiLocale()) }) : t('Status unknown') }),
      el('label', {}, t('Machine name'), name), el('h3', { textContent: t('Allowed directory roots') }), roots, choose,
      el('p', { class: 'hint', textContent: t('Empty roots grant no execution access. Revocation denies future account access; a running program may retain information already received.') })),
    el('div', { class: 'directory-picker-footer' }, revoke, save));
  document.body.append(dialog); document.body.classList.add('directory-picker-open'); dialog.showModal(); close.focus();
}
async function loadMachines() {
  const epoch = accountEpoch;
  const [settings, fleet] = await Promise.all([invoke('machine_settings'), invoke('machine_list')]);
  if (epoch !== accountEpoch) return;
  field('#machine-name', settings.name);
  machineRoots = settings.roots;
  $('#machine-roots-choose').disabled = Boolean(live?.config.server.webhook_prefix);
  renderMachineRoots();
  // The portal is the coordinator itself: browsers approve other computers, never join.
  $('#machine-connect-hint').hidden = window.ptaTransport.browser || Boolean(settings.coordinator);
  $('#machine-disconnect').disabled = !settings.coordinator;
  renderTeamsMachines(fleet);
}

// ---------------------------------------------------------------- settings sections

/** Settings sections with edits not saved yet, so a hidden section never hides them. */
const dirtyGroups = new Set();

/** Selects the tab `name` of a tab list and shows only its panel. */
function selectTab(list, name, focus = false) {
  clearRevealedVariable();
  for (const button of list.querySelectorAll('[role=tab]')) {
    const selected = button.dataset.group === name;
    button.setAttribute('aria-selected', String(selected));
    button.tabIndex = selected ? 0 : -1;
    document.getElementById(button.getAttribute('aria-controls')).hidden = !selected;
    if (selected && focus) button.focus();
  }
}

/** Marks which sections hold unsaved edits and which need something before the assistant works. */
function renderSettingsFlags(attention = {}) {
  for (const button of document.querySelectorAll('#settings-tabs [role=tab]')) {
    const edited = button.querySelector('.flag.edited');
    const warn = button.querySelector('.flag.warn');
    if (edited) edited.hidden = !dirtyGroups.has(button.dataset.group);
    if (warn && button.dataset.group in attention) warn.hidden = !attention[button.dataset.group];
  }
}

function markDirty(event) {
  dirty = true;
  // Called as a listener (the event) or right after a click (the focused control).
  const source = event?.target instanceof Element ? event.target : document.activeElement;
  const group = source?.closest?.('[data-settings-group]');
  if (group) dirtyGroups.add(group.dataset.settingsGroup);
  renderSavebar();
}
function renderSavebar() {
  $('#savebar').hidden = !(dirty && ['settings', 'knowledge'].includes(tab));
  renderSettingsFlags();
}

// ---------------------------------------------------------------- status and overview

const GENERATED = ['GRAPH_WEBHOOK_SECRET', 'STATE_ENCRYPTION_KEY'];
const providerNames = { codex: 'Codex', claude: 'Claude Code', deepseek: 'DeepSeek', opencode: 'OpenCode' };

function expectedSubscriptions(config) {
  return config.graph.discover_all_chats ? 1 : config.graph.allowed_chats.length + config.graph.channels.length;
}
const entraReady = config => !config.graph.client_id.endsWith('0002') && !config.graph.tenant_id.endsWith('0001');
const urlReady = config => !config.server.public_url.includes('example.com');
const readySources = map => map.resources.filter(r => r.enabled && r.external_processing && r.allowed_conversations.length);
const missingCredentials = credentials => Object.entries(credentials).filter(([name, source]) => source === 'missing' && !GENERATED.includes(name)).map(([name]) => name);
const enabledChain = config => config.llm.chain.length
  ? config.llm.chain.filter(choice => choice.enabled)
  : [{ provider: config.llm.provider, model: config.llm.model }];

function assistantState(snapshot) {
  const config = snapshot.loaded_config ?? snapshot.config;
  const observing = config.policy.dry_run;
  if (!snapshot.running) {
    return { tone: 'muted', pill: t('Assistant stopped'), title: t('Assistant stopped'),
      detail: t('It neither receives nor answers Teams messages. The test chat still works.') };
  }
  let tone = observing ? 'info' : 'ok';
  let teams;
  const expected = expectedSubscriptions(config);
  if (!expected) {
    teams = config.graph.self_chat ? t('Personal chat only') : t('No chats set up to receive');
    if (!config.graph.self_chat) tone = 'warn';
  } else if (snapshot.active_subscriptions >= expected) {
    teams = t('Teams: {n} active subscription(s)', { n: snapshot.active_subscriptions });
  } else {
    const code = snapshot.subscription_issue?.match(/graph_http_(\d{3})/)?.[1];
    teams = code ? t('Teams pending (Graph HTTP {code})', { code }) : t('Teams subscription pending');
    tone = 'warn';
  }
  return {
    tone,
    pill: tone === 'warn' ? t('Running · Teams pending') : observing ? t('Running · observing') : t('Running'),
    title: observing ? t('Running in observation mode') : t('Running and answering'),
    detail: `${observing ? t('Drafts proposals without sending them') : t('Answers in Teams as you')} · ${teams}`,
  };
}

/** Phase the host pushes while the assistant starts or stops (`starting`/`stopping`), else null. */
let transition = null;
const transitions = {
  starting: { tone: 'busy', pill: t('Starting…'), title: t('Starting the assistant…'), detail: t('It will receive Teams messages once it is ready.') },
  stopping: { tone: 'busy', pill: t('Stopping…'), title: t('Stopping the assistant…'), detail: t('It stops receiving Teams messages.') },
};

/** One status row; selecting it opens where that part is changed (`go` = showTab arguments).
 *  Its icon tile takes the tone's color; the value says the same in words. */
function tile(label, value, tone, detail, go, symbol) {
  const row = el('button', { type: 'button', class: `status-row ${tone}` },
    el('span', { class: 'status-icon', 'aria-hidden': 'true' }, icon(symbol)),
    el('span', { class: 'status-value' }, el('span', { class: 'status-label', textContent: label }), el('strong', { textContent: value }),
      el('small', { textContent: detail })),
    icon('arrow'));
  row.onclick = () => showTab(...go);
  return row;
}

let azureCliStatus = { state: 'checking', authenticated: false };
let azureCliPending = null;
function loadAzureCliStatus() {
  if (live?.account?.local_execution === false) return Promise.resolve();
  if (azureCliPending) return azureCliPending;
  azureCliPending = invoke('azure_cli_status').then(result => { azureCliStatus = result; })
    .catch(() => { azureCliStatus = { state: 'unavailable', authenticated: false }; })
    .finally(() => { azureCliPending = null; if (live) renderTiles(live); });
  return azureCliPending;
}
function azureCliTile() {
  if (live?.account?.local_execution === false) return personalCliTile(t('Azure CLI'), 'azure-cli-status');
  const state = azureCliStatus;
  const value = { authenticated: t('Authenticated'), not_authenticated: t('Not authenticated'), not_installed: t('Not installed'), unavailable: t('Session unavailable'), checking: t('Checking…') }[state.state] || t('Session unavailable');
  const hint = state.authenticated ? t('Azure DevOps session is valid. Select to refresh.')
    : state.state === 'not_installed' ? t('Install Azure CLI on the machine running this app.')
    : state.state === 'checking' ? t('Checking the Azure DevOps session on this machine…')
    : t('Select to sign in with Microsoft in your browser.');
  const row = tile(t('Azure CLI'), value, state.authenticated ? 'ok' : state.state === 'checking' ? 'muted' : 'warn',
    state.user ? `${state.user} · ${hint}` : hint, ['overview'], 'chip');
  row.id = 'azure-cli-status';
  row.onclick = async () => {
    await busy(row, state.authenticated || ['checking', 'not_installed'].includes(state.state) ? loadAzureCliStatus : azureCliLogin);
    if (tab === 'overview') $('#azure-cli-status')?.focus({ preventScroll: true });
  };
  return row;
}
let githubCliStatus = { state: 'checking', authenticated: false };
let githubCliPending = null;
function loadGithubCliStatus() {
  if (live?.account?.local_execution === false) return Promise.resolve();
  if (githubCliPending) return githubCliPending;
  githubCliPending = invoke('github_cli_status').then(result => { githubCliStatus = result; })
    .catch(() => { githubCliStatus = { state: 'unavailable', authenticated: false }; })
    .finally(() => { githubCliPending = null; if (live) renderTiles(live); });
  return githubCliPending;
}
function githubCliTile() {
  if (live?.account?.local_execution === false) return personalCliTile(t('GitHub CLI'), 'github-cli-status');
  const state = githubCliStatus;
  const value = { authenticated: t('Authenticated'), not_authenticated: t('Not authenticated'), not_installed: t('Not installed'), unavailable: t('Session unavailable'), checking: t('Checking…') }[state.state] || t('Session unavailable');
  const hint = state.authenticated ? t('GitHub session is valid. Select to refresh.')
    : state.state === 'not_installed' ? t('Install GitHub CLI on the machine running this app.')
    : state.state === 'checking' ? t('Checking the GitHub session on this machine…')
    : t('Sign in with gh auth login on the machine running this app, then refresh GitHub CLI status.');
  const row = tile(t('GitHub CLI'), value, state.authenticated ? 'ok' : state.state === 'checking' ? 'muted' : 'warn',
    state.user ? `${state.user} · ${hint}` : hint, ['overview'], 'chip');
  row.id = 'github-cli-status';
  row.onclick = async () => {
    await busy(row, loadGithubCliStatus);
    if (tab === 'overview') $('#github-cli-status')?.focus({ preventScroll: true });
  };
  return row;
}
function personalCliTile(label, id) {
  const row = tile(label, t('Personal machine required'), 'muted', t('CLI sessions belong on your personal trusted machine.'), ['settings', 'panel-web-account'], 'chip');
  row.id = id;
  return row;
}
setInterval(() => { if (current && tab === 'overview') { loadAzureCliStatus(); loadGithubCliStatus(); } }, 60000);

async function azureCliLogin() {
  const dialog = $('#azure-cli-dialog'), status = $('#azure-cli-login-status'), link = $('#azure-cli-link');
  if (dialog.open) return;
  let cancelled = false;
  $('#azure-cli-code').textContent = '';
  $('#azure-cli-copy').disabled = true;
  link.hidden = true; link.removeAttribute('href');
  status.textContent = t('Starting Microsoft sign-in…');
  dialog.showModal();
  const cancel = event => { event?.preventDefault(); cancelled = true; };
  dialog.oncancel = cancel;
  $('#azure-cli-cancel').onclick = cancel;
  try {
    const prompt = await invoke('begin_azure_cli_login');
    if (cancelled) return;
    if (prompt.verification_uri !== 'https://microsoft.com/devicelogin' || !/^[A-Z0-9-]{6,16}$/.test(prompt.user_code)) throw new Error(t('Invalid Microsoft sign-in address.'));
    $('#azure-cli-code').textContent = prompt.user_code;
    $('#azure-cli-copy').disabled = false;
    $('#azure-cli-copy').onclick = () => busy($('#azure-cli-copy'), async () => {
      if (await copy(prompt.user_code)) done(t('Code copied.'));
    });
    link.href = prompt.verification_uri; link.hidden = false;
    link.onclick = event => {
      if (!window.ptaTransport.browser) { event.preventDefault(); invoke('open_azure_cli_login').catch(fail); }
    };
    status.textContent = t('Waiting for Microsoft sign-in…');
    link.focus();
    const deadline = Date.now() + 600_000;
    while (!cancelled && Date.now() < deadline) {
      const result = await rawInvoke('command', { request: { method: 'finish_azure_cli_login', args: {}, revision: null, contract: 1 } });
      if (cancelled) return;
      if (result.ok) {
        azureCliStatus = result.data;
        done(t('Azure CLI connected.'));
        return;
      }
      if (result.code !== 'authorization_pending') throw new Error(tx(result.message || result.code));
      await sleep(2000);
    }
    if (!cancelled) throw new Error(t('Azure CLI sign-in failed or expired. Select Azure CLI to try again.'));
  } finally {
    await rawInvoke('command', { request: { method: 'cancel_azure_cli_login', args: {}, revision: null, contract: 1 } }).catch(() => {});
    dialog.close();
    link.removeAttribute('href'); $('#azure-cli-code').textContent = '';
    await loadAzureCliStatus();
  }
}

function renderTiles(s) {
  const config = s.config;
  const chain = enabledChain(config);
  const [first, ...rest] = chain;
  const ready = readySources(s.map).length;
  const tunnelManaged = config.server.cloudflare_tunnel || s.tunnel_config.trim();
  let host = t('Public URL missing');
  try { if (urlReady(config)) host = new URL(config.server.public_url).host; } catch { /* shown as missing */ }
  $('#tiles').replaceChildren(
    tile(t('Microsoft account'), s.microsoft_connected ? t('Connected') : t('Not connected'), s.microsoft_connected ? 'ok' : 'warn',
      s.microsoft_connected ? t('Reads and answers as you') : t('Connect it in Settings'), ['settings', 'panel-teams'], 'user'),
    azureCliTile(), githubCliTile(),
    tile(t('Mode'), config.policy.dry_run ? t('Observation') : t('Sending on'), config.policy.dry_run ? 'info' : 'ok',
      config.policy.dry_run ? t('Sends nothing to Teams') : t('Answers in Teams'), ['settings', 'panel-behavior'], 'eye'),
    tile(t('Reception'), config.graph.discover_all_chats ? t('All my chats') : t('{n} chat(s)', { n: config.graph.allowed_chats.length }),
      s.running && s.active_subscriptions ? 'ok' : 'muted',
      s.running ? t('{n} active subscription(s)', { n: s.active_subscriptions }) : t('Inactive while stopped'), ['settings', 'panel-teams'], 'antenna'),
    tile(t('Tunnel'), tunnelManaged ? (s.tunnel_running ? t('Running') : t('Stopped')) : t('External'), tunnelManaged && s.tunnel_running ? 'ok' : 'muted', host, ['settings', 'panel-server'], 'globe'),
    tile(t('Models'), first ? `${providerNames[first.provider] || first.provider}` : t('None active'), first ? 'ok' : 'warn',
      first ? `${first.model}${rest.length ? ` · ${t('fallback')}: ${rest.map(c => providerNames[c.provider] || c.provider).join(' → ')}` : ` · ${t('no fallback')}`}` : t('Turn one on in Settings'), ['settings', 'panel-llm'], 'chip'),
    tile(t('Sources'), t('{ready} of {total} ready', { ready, total: s.map.resources.length }), ready ? 'ok' : 'warn', t('Enabled, authorized and with an audience'), ['knowledge'], 'book'),
    tile(t('Personal chat'), config.graph.self_chat ? t('Enabled') : t('Disabled'), config.graph.self_chat ? 'ok' : 'muted',
      config.graph.self_chat ? t('Ask it from your Teams notes') : t('Optional'), ['settings', 'panel-self-chat'], 'chat'),
  );
}

function renderSteps(s) {
  const config = s.config;
  const missing = missingCredentials(s.credentials);
  const steps = [
    { done: !missing.length, title: t('Variables'), text: missing.length ? t('Missing: {names}.', { names: missing.join(', ') }) : t('The required variables are stored.'), go: ['settings', missing.length ? 'panel-consumers' : 'panel-variables'] },
    { done: enabledChain(config).length > 0, title: t('Language model'), text: t('Turn on and order Codex, Claude Code, OpenCode or DeepSeek.'), go: ['settings', 'panel-llm'] },
    { done: entraReady(config), title: t('Entra registration'), text: t('Tenant and client ID of your existing registration.'), go: ['settings', 'panel-teams'] },
    { done: s.microsoft_connected, title: t('Microsoft account'), text: t('Sign in to read and answer as you.'), go: ['settings', 'panel-teams'] },
    { done: urlReady(config), title: t('Public URL and tunnel'), text: t('A stable HTTPS URL that reaches the local port.'), go: ['settings', 'panel-server'] },
    { done: readySources(s.map).length > 0, title: t('Authorized sources'), text: t('At least one source enabled, with external processing and an audience.'), go: ['knowledge'] },
    { done: s.running, title: t('Start the assistant'), text: t('It starts receiving Teams messages.'), start: true },
    { done: !config.policy.dry_run, optional: true, title: t('Turn sending on'), text: t('Review the proposals in Messages and turn observation mode off.'), go: ['settings', 'panel-behavior'] },
  ];
  const required = steps.filter(step => !step.optional);
  const completed = required.filter(step => step.done).length;
  $('#setup-progress').textContent = t('{ready} of {total} ready', { ready: completed, total: required.length });
  $('#setup-bar').style.width = `${Math.round(completed / required.length * 100)}%`;
  const ready = completed === required.length;
  // Once every step, optional ones included, is done the checklist has nothing left to say.
  $('#setup-panel').hidden = steps.every(step => step.done);
  $('#setup-ready').hidden = !ready;
  $('#setup-steps').replaceChildren(...steps.map((step, index) => ({ step, index })).filter(({ step }) => !ready || !step.done).map(({ step, index }) => {
    const mark = el('span', { class: 'step-mark' }, step.done ? icon('check') : String(index + 1));
    const title = el('strong', {}, step.title, step.optional ? el('span', { class: 'optional', textContent: ` · ${t('optional')}` }) : null);
    let action = null;
    if (!step.done && step.go) {
      action = el('button', { class: 'secondary small', textContent: t('Set up') });
      action.onclick = () => showTab(...step.go);
    } else if (!step.done && step.start) {
      action = el('button', { class: 'small', textContent: t('Start') });
      action.onclick = () => startAssistant(action);
    }
    return el('li', { class: `step${step.done ? ' done' : ''}` }, mark, el('div', {}, title, el('p', { textContent: step.text })), action);
  }));
}

function renderLive() {
  const s = live;
  if (!s) return;
  if (s.account) {
    $('#account-identity').hidden = false;
    $('#account-identity').textContent = t('Assistant account: {name}', { name: s.account.username });
    $('#account-execution-limit').hidden = s.account.local_execution !== false;
    if (s.account.shared) {
      $('#panel-web-account').hidden = false;
      $('#web-logout-settings').hidden = !window.ptaTransport.browser;
    }
  }
  const state = transitions[transition] ?? assistantState(s);
  // A running assistant's dot pulses slowly; a starting or stopping one, faster.
  const motion = transition ? '' : s.running ? ' live' : '';
  $('#status').className = `pill ${state.tone}${motion}`;
  $('#status').title = state.detail;
  $('#status span').textContent = state.pill;
  // The hero takes the state's color and the face its expression (style.css).
  $('#hero').className = `panel hero ${state.tone}${motion}`;
  $('#hero-title').textContent = state.title;
  $('#hero-detail').textContent = state.detail;
  $('#version').textContent = `v${s.host_version}${s.headless ? ` · ${t('headless')}` : ''}`;
  $('#restart-offer').hidden = !s.restart_offer;
  $('#start').hidden = s.running;
  $('#restart').hidden = $('#stop').hidden = !s.running;
  for (const button of [$('#start'), $('#restart'), $('#stop')]) button.disabled = Boolean(transition) || button.classList.contains('busy');
  $('#teams-identity-error').hidden = !s.teams_identity_error;
  $('#teams-identity-error').textContent = s.teams_identity_error ? tx(s.teams_identity_error) : '';
  $('#microsoft-state').className = `badge ${s.microsoft_connected ? 'ok' : 'warn'}`;
  $('#microsoft-state').textContent = s.microsoft_connected ? t('Account connected') : t('Account not connected');
  $('#teams-login').textContent = s.microsoft_connected ? t('Reconnect the account') : t('Connect Microsoft account');
  $('#github-state').className = `badge ${s.github_connected ? 'ok' : 'muted'}`;
  $('#github-state').textContent = s.github_connected ? t('Connected') : t('Not connected');
  $('#github-list').disabled = !s.github_connected;
  $('#github-disconnect').disabled = !s.github_connected;
  $('#self-chat-disable').disabled = !s.config.graph.self_chat;
  // Enabling is the next step only while the personal chat is off.
  $('#self-chat-enable').classList.toggle('secondary', Boolean(s.config.graph.self_chat));
  renderTiles(s);
  renderSteps(s);
  renderSettingsFlags({
    variables: missingCredentials(s.credentials).length > 0,
    teams: !s.microsoft_connected || !entraReady(s.config) || !urlReady(s.config),
    models: !enabledChain(s.config).length,
  });
}

/** Refreshes persisted state without touching unsaved form edits. */
async function refreshLive() {
  live = await invoke('snapshot');
  if (current) {
    current.credentials = live.credentials; current.github_connected = live.github_connected;
    // Refresh restored setup without overwriting any unsaved form edits.
    if (!dirty && ['tenant_id', 'client_id', 'user_id'].some(key => current.config.graph[key] !== live.config.graph[key])) {
      // Reload the whole revision so a later save cannot overwrite another synchronized field.
      await reload();
      return;
    }
  }
  renderLive();
  renderChatSources();
}

async function reload() {
  current = await invoke('snapshot');
  current.revision = lastRevision;
  live = structuredClone(current);
  const config = current.config;
  // The interface follows the configured language; a change reloads the page once.
  if (useLanguage(config.llm.language ?? 'es')) return;
  field('#tenant-id', config.graph.tenant_id); field('#client-id', config.graph.client_id);
  field('#user-id', config.graph.user_id); field('#public-url', config.server.public_url);
  field('#bind', config.server.bind); field('#llm-style', config.llm.style);
  field('#llm-language', config.llm.language ?? 'es');
  field('#tunnel-config', current.tunnel_config);
  $('#cloudflare-tunnel').checked = config.server.cloudflare_tunnel;
  field('#allowed-chats', config.graph.allowed_chats.join('\n'));
  $('#dry-run').checked = config.policy.dry_run;
  $('#discover-chats').checked = config.graph.discover_all_chats;
  renderChatList();
  renderRegistrationSettings();
  renderEmbeddings();
  dirty = false;
  dirtyGroups.clear();
  renderSavebar();
  renderLive();
  renderRepositories(); renderResources(); renderContexts(); renderQuestions(); renderPeople(); renderNoteContexts(); renderChatSources();
  loadAzureCliStatus(); loadGithubCliStatus();
  await Promise.all([loadChain(), loadSelfChat()]);
}

setInterval(async () => {
  if (!current) return;
  try { live = await invoke('snapshot'); renderLive(); } catch { /* the next tick retries */ }
  if (tab === 'overview') loadActivity();
}, 15_000);

// ---------------------------------------------------------------- settings

/** The allowed-chats list only applies while not every chat is discovered; its value is kept either way. */
function renderChatList() { $('#allowed-chats-field').hidden = $('#discover-chats').checked; }

function rowText(primary, secondary, extra) {
  return el('div', {}, el('strong', {}, primary, extra), el('small', { textContent: secondary }));
}

// ---------------------------------------------------------------- account standing

/**
 * The opaque identity of the account on screen (from the host's standing), and a counter that
 * moves whenever the screen leaves an account. A reply requested before the move is dropped, so a
 * late answer for the former account can never fill the next one's screen.
 */
let accountKey = null;
let accountEpoch = 0;
/** Whether the coordinator answered, did not, or refused this installation (offline, revoked…). */
let accountStanding = null;

/** Forget everything shown for the former account: lists, drafts, a revealed value and results. */
function leaveAccount() {
  accountEpoch += 1;
  accountKey = null;
  clearTypedVariableValues();
  variableState = { variables: [], pending: [], error: '', sync: null };
  variableAttempt = null;
  consumerState = { consumers: [], launchers: [], error: '' };
  migrationState = { review: null, error: '', choices: {}, result: null };
  for (const key of Object.keys(consumerChoice)) delete consumerChoice[key];
  for (const key of Object.keys(consumerNote)) delete consumerNote[key];
  accountConfig = null;
  accountStanding = null;
  readiness = null;
  renderStanding();
  renderVariables();
  renderConsumers();
  renderMigration();
  renderAccountConfig();
  renderReadiness();
}

/**
 * Take a reply requested at `epoch`. False when the screen left that account meanwhile; a reply
 * for another account than the one shown (requested after the last move) switches the screen.
 */
function adoptAccount(epoch, standing) {
  if (epoch !== accountEpoch) return false;
  const key = standing?.account_key ?? '';
  const switched = accountKey !== null && key !== accountKey;
  if (switched) leaveAccount();
  accountKey = key;
  accountStanding = standing ?? null;
  renderStanding();
  // Other requests of the former screen are dropped; ask again for the account now shown.
  if (switched) setTimeout(() => { loadVariables().catch(() => {}); loadAccountConfig().catch(() => {}); }, 0);
  return true;
}

function renderStanding() {
  renderAccountChip();
  const standing = accountStanding;
  const problem = standing && ['offline', 'revoked'].includes(standing.state);
  const text = !problem ? '' : [
    standing.state === 'offline' ? t('Offline') : t('Signed out or revoked'),
    tx(standing.recovery),
    standing.last_live_at ? t('Last confirmed {when}.', { when: ago(standing.last_live_at * 1000) }) : '',
  ].filter(Boolean).join(' ');
  for (const id of ['#account-standing', '#variables-standing', '#consumers-standing', '#migration-standing']) {
    const line = $(id);
    line.textContent = text;
    line.hidden = !text;
  }
}

// ---------------------------------------------------------------- account variables

/** Metadata only: lists never carry a value, and the forms clear it as soon as it is stored. */
let variableState = { variables: [], pending: [], error: '', sync: null };
/** One entry ID per attempt, so repeating an interrupted save cannot create a duplicate. */
let variableAttempt = null;
/**
 * The one value on screen after an explicit Reveal. It lives only in this variable and the DOM
 * (never in storage, a URL or history) and goes away on Hide, navigation, a hidden page, logout
 * or after VALUE_SECONDS.
 */
let revealedVariable = null;
let revealEpoch = 0;
const VALUE_SECONDS = 30;
/** Entry being edited, armed for deletion, and the description typed so far (kept across a conflict). */
let editingVariable = null;
let deletingVariable = null;
let variableDraft = null;
let armTimer = null;
let clipboardCleanup = null;
const VARIABLE_NAME = /^[A-Za-z_][A-Za-z0-9_]{0,127}$/;
const UNSAFE_TEXT = /[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/;

function clearRevealedVariable(render = true) {
  revealEpoch += 1;
  if (!revealedVariable) return;
  clearTimeout(revealedVariable.timer);
  revealedVariable = null;
  if (render) renderVariables();
}
/** Pagehide and logout also drop what was typed but not yet saved. */
function clearTypedVariableValues() {
  clearRevealedVariable(false);
  for (const field of document.querySelectorAll('#panel-variables textarea')) field.value = '';
  editingVariable = deletingVariable = variableDraft = null;
  renderVariables();
}
document.addEventListener('visibilitychange', () => { if (document.hidden) clearRevealedVariable(); });
window.addEventListener('pagehide', clearTypedVariableValues);

async function digest(text) {
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return Array.from(new Uint8Array(bytes), byte => byte.toString(16).padStart(2, '0')).join('');
}
/**
 * Best effort: a minute after Copy value, clear the clipboard only if it still holds exactly what
 * was copied. Only a fingerprint is kept, and unrelated clipboard contents are never touched.
 */
async function scheduleClipboardCleanup(value) {
  clearTimeout(clipboardCleanup);
  if (!crypto.subtle || !navigator.clipboard?.readText) return;
  const fingerprint = await digest(value);
  clipboardCleanup = setTimeout(async () => {
    try { if (await digest(await navigator.clipboard.readText()) === fingerprint) await navigator.clipboard.writeText(''); } catch { /* left alone without permission or focus */ }
  }, 60_000);
}

/** A change names the version the user saw; another client's newer change refreshes the list instead of being overwritten. */
async function changeVariable(method, variable, extra = {}) {
  try {
    return await invoke(method, { id: variable.id, version: variable.version, ...extra });
  } catch (error) {
    await loadVariables();
    throw error;
  }
}

async function loadVariables() {
  const epoch = accountEpoch;
  try {
    const reply = await invoke('variable_list');
    if (!adoptAccount(epoch, reply.standing)) return;
    variableState = { variables: reply.variables, pending: reply.pending, error: reply.migration_error ?? '', sync: reply.sync ?? null };
  } catch (error) {
    if (epoch !== accountEpoch) return;
    variableState = { variables: [], pending: [], error: String(error?.message ?? error), sync: null };
  }
  renderVariables();
  await loadConsumers();
  await loadMigration();
}

// ---------------------------------------------------------------- credential migration

/** The last review from the host (names, origins and metadata only) and the user's choices. */
let migrationState = { review: null, error: '', choices: {}, result: null };

const MIGRATION_STAGES = {
  planned: 'Not copied yet',
  writing: 'Copying',
  uncertain: 'Outcome unknown: review again, then repeat to resume',
  written: 'Copied, not verified yet',
  verification_failed: 'The account value does not match',
  verified: 'Verified',
  complete: 'Done',
};

async function loadMigration() {
  const epoch = accountEpoch;
  try {
    const review = await invoke('variable_migration_inspect');
    if (!adoptAccount(epoch, review.destination?.standing)) return;
    const stale = migrationState.review?.destination?.key !== review.destination.key;
    migrationState = { review, error: '', choices: stale ? {} : migrationState.choices, result: stale ? null : migrationState.result };
  } catch (error) {
    if (epoch !== accountEpoch) return;
    migrationState = { review: null, error: String(error?.message ?? error), choices: {}, result: null };
  }
  renderMigration();
}

function migrationChoice(row) {
  return migrationState.choices[row.credential] ??= {
    picked: false,
    name: row.suggested.name,
    description: row.suggested.description,
    existing: '',
    bind: !!row.consumer && !row.account_bound,
  };
}

/** The account variable that already has this name, unless it is the entry a migration made itself. */
function takenName(name) {
  const own = new Set((migrationState.review?.credentials ?? []).map(c => c.entry_id));
  return (migrationState.review?.variables ?? []).find(v => v.name.toLowerCase() === name.trim().toLowerCase() && !own.has(v.id));
}

function migrationItems() {
  const { review, choices } = migrationState;
  const items = [];
  for (const row of review?.credentials ?? []) {
    const choice = choices[row.credential];
    if (!choice?.picked || row.status !== 'eligible') continue;
    const item = { credential: row.credential, name: choice.name.trim(), description: choice.description.trim(), bind: choice.bind && !!row.consumer };
    const found = (review.variables ?? []).find(v => v.name.toLowerCase() === item.name.toLowerCase());
    if (found && ['use', 'replace'].includes(choice.existing)) item.existing = { action: choice.existing, version: found.version };
    items.push(item);
  }
  return items;
}

function migrationRow(row) {
  const choice = migrationChoice(row);
  const pick = el('input', { type: 'checkbox', checked: choice.picked, 'aria-label': t('Move {name}', { name: row.credential }) });
  pick.onchange = () => { choice.picked = pick.checked; renderMigration(); };
  const origin = { configuration: t('Settings'), system: t('System store'), environment: t('Environment variable'), file: t('Private file') }[row.origin] ?? row.origin;
  const head = el('label', { class: 'check' }, pick, el('span', { class: 'cred-name', textContent: row.credential, translate: 'no' }), ' ', badge(origin, 'muted'),
    row.consumer ? badge(t('Used by {consumer}', { consumer: row.consumer.title }), 'info') : badge(t('No consumer uses it yet'), 'muted'),
    row.account_bound ? badge(t('Already uses an account variable'), 'ok') : null);
  const parts = [head];
  if (choice.picked) {
    const name = el('input', { value: choice.name, maxLength: 128, translate: 'no', autocomplete: 'off', spellcheck: false });
    name.oninput = () => { choice.name = name.value; renderMigrationActions(); };
    name.onchange = () => renderMigration();
    const description = el('input', { value: choice.description, maxLength: 500, autocomplete: 'off', spellcheck: false });
    description.oninput = () => { choice.description = description.value; renderMigrationActions(); };
    parts.push(el('div', { class: 'grid' },
      el('label', {}, t('Variable name'), name),
      el('label', {}, t('Description'), el('small', {}, t('What it is for, so you and your agents can choose it.')), description)));
    const found = takenName(choice.name);
    if (found) {
      const how = el('select', { 'aria-label': t('A variable named {name} already exists', { name: found.name }) },
        el('option', { value: '', textContent: t('A variable with this name exists: choose what to do') }),
        el('option', { value: 'use', selected: choice.existing === 'use', textContent: t('Use the existing variable (version {n}) if its value matches', { n: found.version }) }),
        el('option', { value: 'replace', selected: choice.existing === 'replace', textContent: t('Replace its value (version {n})', { n: found.version }) }));
      how.onchange = () => { choice.existing = how.value; renderMigrationActions(); };
      parts.push(el('label', {}, t('Name already in your account'), how),
        el('p', { class: 'hint variable-note', textContent: tx(found.description), translate: 'no' }));
    }
    if (row.consumer) {
      const bind = el('input', { type: 'checkbox', checked: choice.bind });
      bind.onchange = () => { choice.bind = bind.checked; };
      parts.push(el('label', { class: 'check' }, bind, t('Switch {consumer} to the account variable once it is verified', { consumer: row.consumer.title })));
    }
  }
  if (row.migration) parts.push(el('p', { class: 'hint variable-note', textContent: t(MIGRATION_STAGES[row.migration.stage] ?? row.migration.stage) }));
  return el('div', { class: 'row variable-row' }, parts);
}

function renderMigrationActions() {
  const items = migrationItems();
  const collision = items.some(item => !item.existing && (takenName(item.name) || (migrationState.review.variables ?? []).some(v => v.name.toLowerCase() === item.name.toLowerCase() && v.description !== item.description)));
  $('#migration-run').disabled = !items.length || items.some(i => !i.description) || collision || !$('#migration-confirm').checked;
}

function renderMigrationResult() {
  const box = $('#migration-result');
  const result = migrationState.result;
  box.hidden = !result;
  box.replaceChildren();
  if (!result) return;
  if (result.message) box.append(el('p', { class: result.complete ? 'hint' : 'callout warn', textContent: tx(result.message) }));
  for (const item of result.items ?? []) {
    box.append(el('div', { class: 'row variable-row' },
      rowText(el('span', { class: 'cred-name', textContent: item.name, translate: 'no' }), t(MIGRATION_STAGES[item.stage] ?? item.stage),
        badge(item.complete ? t('Done') : t('Incomplete'), item.complete ? 'ok' : 'warn')),
      item.error ? el('p', { class: 'hint variable-note', textContent: tx(item.error) }) : null));
  }
  for (const problem of result.problems ?? []) {
    box.append(el('p', { class: 'callout warn', textContent: `${problem.credential}: ${tx(problem.problem)}` }));
  }
  if (result.settings) box.append(el('p', { class: 'hint', textContent: result.settings.stage === 'complete' ? t('This computer now uses the account settings.') : tx(result.settings.error ?? '') }));
  if (result.complete) box.append(el('p', { class: 'hint', textContent: t('The original credentials were preserved as recovery copies. Application credentials are managed in Variables.') + (result.restart_required ? ' ' + t('Restart the assistant to apply this change.') : '') }));
}

function renderMigration() {
  const { review, error } = migrationState;
  const list = $('#migration-list');
  list.replaceChildren();
  $('#migration-destination').hidden = !review;
  $('#migration-excluded').hidden = true;
  $('#migration-settings-field').hidden = true;
  $('#migration-confirm-field').hidden = true;
  $('#migration-local').hidden = true;
  if (error) { list.append(el('p', { class: 'hint', role: 'status', textContent: tx(error) })); $('#migration-run').disabled = true; renderMigrationResult(); return; }
  if (!review) { $('#migration-run').disabled = true; renderMigrationResult(); return; }
  const account = tx(review.destination.account);
  $('#migration-destination').textContent = t('Destination account: {account}', { account });
  const eligible = review.credentials.filter(c => c.status === 'eligible');
  if (!eligible.length) list.append(el('p', { class: 'hint', textContent: t('No configured credential is waiting to be moved.') }));
  for (const row of eligible) list.append(migrationRow(row));
  const excluded = review.credentials.filter(c => c.status === 'excluded' || c.status === 'unavailable');
  $('#migration-excluded').hidden = !excluded.length;
  $('#migration-excluded-list').replaceChildren(...excluded.map(c => el('div', { class: 'row variable-row' },
    rowText(el('span', { class: 'cred-name', textContent: c.credential, translate: 'no' }), tx(c.reason ?? '')))));
  const replica = review.settings?.mode === 'replica';
  $('#migration-settings-field').hidden = !replica;
  $('#migration-confirm-field').hidden = !eligible.length && !replica;
  $('#migration-confirm-text').textContent = t('Copy into {account}', { account });
  $('#migration-local').hidden = false;
  $('#migration-local').textContent = t('Still done on each computer: {list}.', { list: review.remains_local.map(tx).join('; ') });
  renderMigrationActions();
  renderMigrationResult();
}

async function runMigration() {
  const { review } = migrationState;
  const body = { destination: review.destination.key, items: migrationItems() };
  if ($('#migration-settings').checked && review.settings?.mode === 'replica') body.settings = 'adopt_account';
  // A partly done migration reports its per-item outcome together with the error, so the reply is read whole.
  const reply = await rawInvoke('command', { request: { method: 'variable_migration_run', args: body, revision: null, contract: 1 } });
  const result = { ...(reply.data ?? {}), complete: reply.ok && reply.data?.complete === true, message: reply.ok ? '' : String(reply.message ?? reply.code) };
  migrationState.result = result;
  $('#migration-confirm').checked = false;
  await loadMigration();
  if (result.complete) done(t('Selected credentials were copied and verified.'));
}

$('#migration-refresh').onclick = e => busy(e.currentTarget, loadMigration);
$('#migration-run').onclick = e => busy(e.currentTarget, runMigration);
$('#migration-confirm').onchange = () => renderMigrationActions();

// ---------------------------------------------------------------- updates

/** The host's view of this installation's updates; null until the first answer. */
let updates = null;

async function loadUpdates() {
  try { updates = await invoke('crate_update_status'); } catch { updates = null; }
  renderUpdates();
}

/** A host message of several sentences, each translated on its own. */
const sentences = text => text.split(/(?<=\.) /).map(tx).join(' ');

const UPDATE_STATES = { idle: 'Up to date', staging: 'Preparing', staged: 'Prepared', failed: 'Failed', deferred: 'Waiting for work to finish', applying: 'Installing' };
const APPLY_PROGRESS = {
  applying: 'Installing version {version}: replacing the application and pta…',
  restarting: 'Restarting the application…',
  verifying: 'Checking that version {version} is healthy…',
  rolling_back: 'Restoring the previous version…',
};
/** Armed after one press: installing restarts the application, so it takes a second, deliberate press. */
let updateInstallArmed = false;

function renderUpdates() {
  const u = updates;
  const managed = u?.managed === 'server';
  const offered = Boolean(u?.available) && !managed;
  const apply = u?.apply;
  const working = ['applying', 'restarting', 'verifying', 'rolling_back'].includes(apply?.state);
  $('#panel-updates').hidden = !u;
  $('#update-check').hidden = managed;
  $('#update-intro').hidden = managed;
  $('#update-managed').hidden = !managed;
  if (!u) { $('#update-offer').hidden = true; return; }
  $('#update-managed').textContent = managed ? tx(u.message) : '';
  const state = managed ? '' : working ? t(UPDATE_STATES.applying) : offered && u.state === 'idle' ? t('Update available') : t(UPDATE_STATES[u.state] ?? u.state);
  $('#update-state').textContent = state;
  $('#update-state').className = `badge ${u.state === 'failed' || apply?.state === 'failed' ? 'warn' : offered || u.state === 'staged' ? 'ok' : 'muted'}`;
  $('#update-state').hidden = !state;
  const rows = [[t('Running'), u.running]];
  if (!managed) rows.push([t('Installed'), u.installed ?? t('Unknown')], [t('Available'), u.available ?? t('Nothing newer')]);
  $('#update-versions').replaceChildren(...rows.map(([label, value]) => el('div', { class: 'row' },
    el('span', { textContent: label }), el('code', { translate: 'no', textContent: value }))));
  const diagnostic = !managed && u.install && !u.install.supported ? u.install.diagnostic : u.check_error ?? (!managed && !working ? u.apply_unsupported : null);
  $('#update-diagnostic').hidden = !diagnostic;
  $('#update-diagnostic').textContent = diagnostic ? sentences(diagnostic) : '';
  const stage = u.stage;
  const building = u.state === 'staging';
  $('#update-progress').hidden = !(building || u.state === 'staged' || u.state === 'deferred' || working);
  if (working) {
    $('#update-progress').textContent = t(APPLY_PROGRESS[apply.state], { version: apply.to });
  } else if (building) {
    const crates = stage?.progress?.crates ?? 0;
    $('#update-progress').textContent = stage?.phase === 'verifying'
      ? t('Verifying the new application and pta pair…')
      : t('Building version {version} ({n} packages compiled)…', { version: stage?.version ?? '', n: crates });
  } else if (u.state === 'staged' || u.state === 'deferred') {
    $('#update-progress').textContent = t('Version {version} is built and verified. It is not installed yet: the installed version keeps running.', { version: u.staged.version });
  }
  const deferred = u.state === 'deferred' ? u.deferred : null;
  $('#update-deferred').hidden = !deferred;
  if (deferred) {
    $('#update-deferred-text').textContent = t('Version {version} will be installed as soon as this work finishes. Nothing is interrupted:', { version: deferred.version });
    $('#update-deferred-reasons').replaceChildren(...deferred.reasons.map(reason => el('li', { textContent: tx(reason) })));
  }
  const failed = u.state === 'failed' && stage?.error;
  $('#update-error').hidden = !failed;
  $('#update-error').textContent = failed ? sentences(stage.error) : '';
  const tail = failed ? stage.tail ?? [] : [];
  $('#update-tail').hidden = !tail.length;
  $('#update-tail-text').textContent = tail.join('\n');
  // What happened to the last installation, only ever from the host's journal.
  const outcome = !working && ['success', 'rolled_back', 'failed'].includes(apply?.state) ? apply : null;
  $('#update-result').hidden = !outcome;
  $('#update-result').className = `callout${outcome && outcome.state !== 'success' ? ' warn' : ''}`;
  if (outcome) {
    $('#update-result-text').textContent = outcome.state === 'success'
      ? t('Version {version} is installed and healthy.', { version: outcome.to })
      : outcome.state === 'rolled_back'
        ? t('Version {to} was not kept and version {from} is running again.', { to: outcome.to, from: outcome.from })
        : t('The update did not finish and needs your attention.');
    const reason = outcome.state === 'success' ? '' : [outcome.error, outcome.recovery].filter(Boolean).map(sentences).join(' ');
    $('#update-result-error').hidden = !reason;
    $('#update-result-error').textContent = reason;
    $('#update-result-notes').replaceChildren(...(outcome.notes ?? []).map(note => el('li', { textContent: sentences(note) })));
  }
  $('#update-stage').hidden = managed;
  $('#update-stage').disabled = !u.can_stage;
  const canApply = Boolean(u.can_apply) && !deferred;
  $('#update-apply').hidden = !canApply;
  $('#update-apply').textContent = updateInstallArmed ? t('Tap again to install and restart') : t('Install update');
  $('#update-cancel').hidden = !deferred;
  $('#update-dismiss').hidden = !offered || u.dismissed;
  const notice = offered && !u.dismissed && !working && (u.can_stage || canApply) && !outcome?.error;
  $('#update-offer').hidden = !notice;
  if (notice) {
    const prepared = canApply || u.state === 'staged';
    $('#update-offer-title').textContent = prepared ? t('A new version is prepared') : t('A new version is available');
    $('#update-offer-text').textContent = prepared
      ? t('Version {available} is prepared and verified. Installing it replaces the application and pta and restarts the application; if the new version is not healthy, the previous one is restored.', { available: u.available })
      : t('Version {available} is available; this computer runs {running}. Preparing it builds it next to the installed one without changing anything.', { available: u.available, running: u.running });
    $('#update-offer-stage').hidden = prepared;
    $('#update-offer-apply').hidden = !canApply;
    $('#update-offer-apply').textContent = updateInstallArmed ? t('Tap again to install and restart') : t('Install update');
  }
  $('#update-offer-stage').disabled = !u.can_stage;
}

async function stageUpdate() {
  if (!updates?.available) return;
  await invoke('crate_update_stage', { version: updates.available });
  await loadUpdates();
}
async function dismissUpdate() {
  if (!updates?.available) return;
  await invoke('crate_update_dismiss', { version: updates.available });
  await loadUpdates();
}
async function installUpdate(event) {
  if (!updates?.can_apply) return;
  if (!updateInstallArmed) {
    updateInstallArmed = true;
    renderUpdates();
    setTimeout(() => { updateInstallArmed = false; renderUpdates(); }, 6000);
    return;
  }
  updateInstallArmed = false;
  await busy(event.currentTarget, async () => {
    // The application restarts when the update starts: the reply may be the last thing this window sees.
    await invoke('crate_update_apply', { version: updates.staged?.version ?? updates.available });
    await loadUpdates();
  });
  renderUpdates();
}
async function cancelUpdate() {
  await invoke('crate_update_cancel');
  await loadUpdates();
}
// A busy button is re-enabled when its task ends; render again so the host's state decides.
const updating = (e, task) => busy(e.currentTarget, task).then(renderUpdates);
$('#update-check').onclick = e => updating(e, async () => {
  await invoke('crate_update_check');
  await loadUpdates();
});
$('#update-stage').onclick = e => updating(e, stageUpdate);
$('#update-offer-stage').onclick = e => updating(e, stageUpdate);
$('#update-apply').onclick = installUpdate;
$('#update-offer-apply').onclick = installUpdate;
$('#update-cancel').onclick = e => updating(e, cancelUpdate);
$('#update-dismiss').onclick = e => updating(e, dismissUpdate);
$('#update-offer-dismiss').onclick = e => updating(e, dismissUpdate);
setTimeout(loadUpdates, 1500);
setInterval(() => { if (current) loadUpdates(); }, 5 * 60_000);
setInterval(() => { if (['staging', 'applying', 'deferred'].includes(updates?.state)) loadUpdates(); }, 3_000);

// ---------------------------------------------------------------- account configuration

/** What this installation knows about the account's configuration (paths and credentials never). */
let accountConfig = null;
let accountResetArmed = false;

async function loadAccountConfig() {
  const epoch = accountEpoch;
  try {
    const reply = await invoke('portable_status');
    if (!adoptAccount(epoch, reply.standing)) return;
    accountConfig = reply.sync;
  } catch {
    if (epoch !== accountEpoch) return;
    accountConfig = null;
  }
  renderAccountConfig();
}

const ACCOUNT_STATES = {
  synced: ['Synchronized', 'ok'], stale: ['Newer version available', 'warn'], diverged: ['Unsaved changes', 'warn'],
  conflict: ['Conflict', 'warn'], error: ['Failed', 'warn'], never: ['Not loaded yet', 'muted'], syncing: ['Synchronizing…', 'muted'],
};

function renderAccountConfig() {
  const panel = $('#panel-account-config');
  const sync = accountConfig;
  panel.hidden = sync?.mode !== 'replica';
  if (panel.hidden) return renderAccountChip();
  const [label, tone] = ACCOUNT_STATES[sync.state] ?? ACCOUNT_STATES.never;
  const badge = $('#account-config-state');
  badge.textContent = t(label);
  badge.className = `badge ${tone}`;
  const loaded = sync.loaded ? t('Loaded revision {loaded}; account revision {saved}.', { loaded: sync.loaded.generation, saved: (sync.saved ?? sync.loaded).generation }) : '';
  const when = sync.synced_at ? t('Synchronized {when}.', { when: ago(sync.synced_at * 1000) }) : t('Not synchronized yet.');
  $('#account-config-summary').textContent = `${loaded} ${when}`.trim();
  const problem = $('#account-config-problem');
  const message = sync.conflict ? `${t('A change conflicted with a newer one. Synchronize, review the result and try again.')} ${tx(sync.conflict)}`
    : sync.state === 'diverged' ? t('This computer has configuration changes that were never saved to the account. Nothing was replaced.')
    : sync.state === 'never' && sync.local_changes ? t("This computer was configured before it signed in, so the account's configuration was not loaded over it. Replace this computer's settings with the account's to restore them.")
    : sync.error ? tx(sync.error.message) : '';
  problem.textContent = message.trim();
  problem.hidden = !problem.textContent;
  renderAccountChip();
  const setup = $('#account-config-setup');
  setup.replaceChildren();
  const items = [...(sync.local_setup ?? [])];
  if (items.length) setup.append(el('h3', { textContent: t('Still needed on this computer') }));
  for (const item of items) setup.append(el('p', { class: 'hint', textContent: tx(item.message) }));
  const hidden = (sync.unavailable?.sources?.length ?? 0) + (sync.unavailable?.questions?.length ?? 0);
  if (hidden) setup.append(el('p', { class: 'hint', textContent: tn(hidden, '{n} item from your account is unavailable on this computer.', '{n} items from your account are unavailable on this computer.') }));
  const reset = $('#account-config-reset');
  reset.hidden = !['diverged', 'conflict', 'error'].includes(sync.state) && !(sync.state === 'never' && (sync.error || sync.local_changes));
  reset.textContent = accountResetArmed ? t('Tap again to replace') : t("Replace this computer's settings with the account's");
}

$('#account-config-reset').onclick = event => {
  if (!accountResetArmed) {
    accountResetArmed = true;
    renderAccountConfig();
    setTimeout(() => { accountResetArmed = false; renderAccountConfig(); }, 4000);
    return;
  }
  accountResetArmed = false;
  busy(event.currentTarget, async () => {
    await syncAccount({ adopt: true });
    done(t('This computer now uses the account configuration. The previous settings are kept in a private backup.'));
  });
};
setInterval(() => { if (tab === 'settings' && !$('#panel-account-config').hidden) loadAccountConfig(); }, 10_000);

// ---------------------------------------------------------------- account readiness

const READINESS_STEPS = {
  coordinator: ['panel-machines', () => t('Coordinator and sign-in')],
  connection: ['panel-machines', () => t('Coordinator accepts this computer')],
  machine: ['panel-machines', () => t('Listed on your account')],
  configuration: ['panel-account-config', () => t('Account configuration')],
  variables: ['panel-variables', () => t('Variables')],
  migration: ['panel-migration', () => t('Existing credentials')],
  providers: ['panel-llm', () => t('Model providers')],
  roots: ['panel-machines', () => t('Directories for coding agents')],
  teams: ['panel-teams', () => t('Microsoft Teams')],
  versions: ['panel-updates', () => t('Versions')],
};
const READINESS_STATES = {
  done: ['Done', 'ok'], todo: ['To do', 'warn'], waiting: ['Waiting', 'info'], attention: ['Needs attention', 'warn'],
  unknown: ['Not checked', 'muted'], optional: ['Optional', 'muted'],
};

async function loadReadiness() {
  const epoch = accountEpoch;
  try {
    const report = await invoke('setup_readiness');
    if (epoch !== accountEpoch) return;
    readiness = report;
  } catch {
    if (epoch !== accountEpoch) return;
    readiness = null;
  }
  renderReadiness();
  if (live) renderSteps(live);
}

/** The facts worth a line under a step, in the interface language. */
function readinessFacts(step) {
  const f = step.facts ?? {};
  const when = at => ago(at * 1000);
  switch (step.id) {
    case 'connection':
      return f.checked_at ? t('Checked {when}.', { when: when(f.checked_at) }) : '';
    case 'machine':
      return f.machines ? tn(f.machines, '{n} machine on your account; {online} online.', '{n} machines on your account; {online} online.', { online: f.online }) : '';
    case 'configuration':
      return f.loaded ? t('Loaded revision {loaded}; account revision {saved}.', { loaded: f.loaded.generation, saved: (f.saved ?? f.loaded).generation }) : '';
    case 'variables':
      return tn(f.count ?? 0, '{n} variable stored.', '{n} variables stored.');
    case 'providers':
      return f.providers?.length ? f.providers.join(', ') : '';
    case 'roots':
      return tn(f.count ?? 0, '{n} directory selected.', '{n} directories selected.');
    case 'teams':
      return [
        f.microsoft_signed_in ? t('Signed in to Microsoft.') : t('Not signed in to Microsoft.'),
        f.running ? t('The assistant is running.') : t('The assistant is stopped.'),
        t('{received} received · {sent} sent.', { received: f.received, sent: f.sent }),
        f.last_received_at ? t('Last message received {when}.', { when: when(f.last_received_at) }) : t('No Teams message has been received yet.'),
      ].join(' ');
    case 'versions':
      return t('Running {running} · installed {installed} · available {available}.', { running: f.running ?? '—', installed: f.installed ?? '—', available: f.available ?? '—' });
    default:
      return '';
  }
}

function expectedRow(item) {
  const text = item.state === 'present' ? t('{name}: stored, version {n}', { name: item.name, n: item.version })
    : item.state === 'waiting' ? t('{name}: waiting for you to type the value', { name: item.name })
      : t('{name}: not stored yet', { name: item.name });
  return el('li', { textContent: text, translate: 'no' });
}

function renderReadiness() {
  const panel = $('#account-setup-panel');
  const report = readiness;
  const coordinator = report?.steps.find(step => step.id === 'coordinator');
  if (report?.mode === 'signed_in' && !accountConfig) loadAccountConfig();
  renderAccountChip();
  maybeOfferSignIn();
  // A coordinator, or a portal browser on one, has no account to join: nothing to guide.
  panel.hidden = !report || (report.mode === 'standalone' && coordinator?.state === 'optional' && !canSignIn());
  if (panel.hidden) return;
  const state = $('#account-setup-state');
  const signedOut = report.mode === 'standalone';
  state.textContent = signedOut ? t('Not signed in') : report.ready ? t('Ready') : t('Needs attention');
  state.className = `badge ${report.ready && !signedOut ? 'ok' : 'warn'}`;
  const next = $('#account-setup-next');
  next.textContent = report.next ? tx(report.next.detail) : '';
  next.hidden = !report.next || report.ready;
  const shown = report.steps.filter(step => step.state !== 'not_applicable');
  $('#account-setup-steps').replaceChildren(...shown.map((step, index) => {
    const [panelId, title] = READINESS_STEPS[step.id] ?? ['panel-machines', () => step.id];
    const [label, tone] = READINESS_STATES[step.state] ?? READINESS_STATES.unknown;
    const mark = el('span', { class: 'step-mark' }, step.state === 'done' ? icon('check') : String(index + 1));
    const scope = badge(step.scope === 'coordinator' ? t('Account') : t('This computer'), 'muted');
    const expected = (step.facts?.expected ?? []).map(expectedRow);
    const facts = readinessFacts(step);
    const body = el('div', {}, el('strong', {}, title(), ' ', badge(t(label), tone), ' ', scope),
      el('p', { textContent: tx(step.detail) }),
      facts ? el('p', { class: 'hint', textContent: facts }) : null,
      expected.length ? el('ul', { class: 'hint' }, expected) : null);
    let action = null;
    const signIn = canSignIn() && ['coordinator', 'connection', 'machine'].includes(step.id) && step.state !== 'waiting';
    if (signIn && step.id === 'coordinator') {
      action = el('button', { class: 'small', textContent: t('Sign in') });
      action.onclick = () => openAccountLogin();
    } else if (step.state !== 'done' && step.state !== 'not_applicable') {
      action = el('button', { class: 'secondary small', textContent: t('Set up') });
      action.onclick = () => showTab('settings', panelId);
    }
    return el('li', { class: `step${step.state === 'done' ? ' done' : ''}` }, mark, body, action);
  }));
}

$('#account-setup-refresh').onclick = event => busy(event.currentTarget, loadReadiness);
setTimeout(loadReadiness, 1200);
setInterval(() => { if (current && tab === 'overview') loadReadiness(); }, 30_000);

// ---------------------------------------------------------------- account sign-in and synchronization

/** Set on this device when its user chose to work without an account; the sidebar still offers sign-in. */
const SIGN_IN_SKIPPED_KEY = 'pta-sign-in-skipped';
let signInOffered = false;

/** Whether this window may add its computer to an account: the owner's own window on a computer
 *  that is not signed in and is not itself the coordinator other computers use. */
function canSignIn() {
  const report = readiness;
  return Boolean(report) && !window.ptaTransport.browser && report.local_owner
    && (report.mode === 'standalone' ? !report.coordinating : report.mode === 'signed_in' && accountStanding?.state === 'revoked');
}

/**
 * Brings the account's variables and configuration to this computer from its coordinator, which is
 * authoritative. `adopt` replaces settings this computer had before signing in (the host keeps a
 * private backup). Resolves whether the configuration changed.
 */
async function syncAccount({ adopt = false } = {}) {
  // Unsaved edits would be saved over the account's newer configuration later: settle them first.
  if (dirty) throw t('Save or discard your changes before synchronizing with your account.');
  // One host step brings the variable catalog, then the configuration (`pta account sync`, MCP account_sync).
  let changed = false, problem = null;
  try {
    const reply = await invoke('account_sync', adopt ? { reset: true } : {});
    changed = reply.changes?.changed === true;
  } catch (error) { problem = error; }
  // The forms show what the account restored (configuration, Microsoft identifiers) right away,
  // so a later save never writes an older copy over it.
  await reload();
  await Promise.all([loadVariables(), loadAccountConfig(), loadReadiness()]).catch(() => {});
  if (problem) throw problem;
  return changed;
}

/** Signs this computer in from the sign-in screen, then loads the account here. */
async function signIn(url, adopt) {
  if (!await machineLogin(url)) return;
  await syncAccount({ adopt });
  try { localStorage.removeItem(SIGN_IN_SKIPPED_KEY); } catch { /* this page only */ }
  done(t("Signed in. Your account's settings and variables are on this computer."));
  showTab('overview');
}

async function openAccountLogin() {
  const dialog = $('#account-login-dialog');
  if (dialog.open || $('#machine-login-dialog').open) return;
  $('#account-login-status').textContent = '';
  const input = $('#account-login-url');
  if (!input.value) {
    const settings = await invoke('machine_settings').catch(() => null);
    input.value = settings?.coordinator || settings?.coordinator_origin || '';
  }
  $('#account-login-adopt').checked = true;
  dialog.showModal();
  (input.value ? $('#account-login-submit') : input).focus();
}

/** Offers the sign-in screen once per window to a computer that is not part of an account yet. */
function maybeOfferSignIn() {
  if (!canSignIn() || readiness.mode !== 'standalone' || signInOffered) return;
  if (readiness.steps.find(step => step.id === 'coordinator')?.state === 'waiting') return;
  let skipped = false;
  try { skipped = localStorage.getItem(SIGN_IN_SKIPPED_KEY) === '1'; } catch { /* offer it */ }
  signInOffered = true;
  if (!skipped) openAccountLogin().catch(fail);
}

$('#account-login-form').onsubmit = event => {
  event.preventDefault();
  const url = $('#account-login-url').value.trim(), adopt = $('#account-login-adopt').checked;
  $('#account-login-dialog').close();
  busy($('#account-chip-action'), () => signIn(url, adopt));
};
$('#account-login-skip').onclick = () => {
  try { localStorage.setItem(SIGN_IN_SKIPPED_KEY, '1'); } catch { /* asked again next time */ }
  $('#account-login-dialog').close();
  notify(t('This computer works on its own. Sign in any time from the sidebar.'));
};

/** The account configuration's state, as the indicator words it. */
function chipState(state) {
  switch (state) {
    case 'synced': return [t('Up to date'), 'ok'];
    case 'syncing': return [t('Synchronizing…'), 'info'];
    case 'stale': return [t('Newer version available'), 'warn'];
    case 'diverged': return [t('Unsaved changes'), 'warn'];
    case 'conflict': return [t('Conflict'), 'warn'];
    case 'error': return [t('Synchronization failed'), 'warn'];
    default: return [t('Not loaded yet'), 'warn'];
  }
}

/** The sidebar's account indicator: always in sight in the owner's window, with the next action. */
function renderAccountChip() {
  const chip = $('#account-chip');
  const report = readiness;
  chip.hidden = !report || window.ptaTransport.browser || !report.local_owner || report.mode === 'portal_account'
    || (report.mode === 'standalone' && report.coordinating);
  if (chip.hidden) return;
  let title, detail, tone, label, glyph;
  const standing = accountStanding?.state ?? report.steps.find(step => step.id === 'connection')?.facts?.standing;
  const host = (report.coordinator ?? '').replace(/^https:\/\//, '');
  if (report.mode === 'standalone') {
    const waiting = report.steps.find(step => step.id === 'coordinator')?.state === 'waiting';
    [title, detail, tone, label, glyph] = [t('Not signed in'), waiting ? t('A sign-in is waiting for your approval.') : t('Your account settings are not on this computer.'), 'warn', t('Sign in'), 'user'];
  } else if (standing === 'revoked') {
    [title, detail, tone, label, glyph] = [t('Sign-in no longer valid'), host, 'error', t('Sign in again'), 'user'];
  } else {
    const sync = accountConfig;
    const [state, stateTone] = sync?.mode === 'replica' ? chipState(sync.state) : ['', 'ok'];
    const when = sync?.synced_at ? t('Synchronized {when}.', { when: ago(sync.synced_at * 1000) }) : '';
    title = standing === 'offline' ? t('Coordinator offline') : t('Signed in');
    detail = [host, state, sync?.state === 'synced' ? when : ''].filter(Boolean).join(' · ');
    tone = standing === 'offline' ? 'warn' : stateTone;
    [label, glyph] = [t('Synchronize'), 'refresh'];
  }
  chip.className = `account-chip ${tone}`;
  $('#account-chip-dot').className = `dot ${tone}`;
  $('#account-chip-title').textContent = title;
  $('#account-chip-detail').textContent = detail;
  $('#account-chip-label').textContent = label;
  $('#account-chip-icon').setAttribute('href', `#i-${glyph}`);
  const action = $('#account-chip-action');
  action.classList.toggle('secondary', glyph === 'refresh');
  action.title = `${title}. ${detail}`;
}

$('#account-chip-action').onclick = event => {
  const button = event.currentTarget;
  if (readiness?.mode === 'signed_in' && accountStanding?.state !== 'revoked') {
    busy(button, async () => {
      const changed = await syncAccount();
      done(changed ? t("Your account's latest settings and variables are on this computer.") : t('Already up to date with your account.'));
    });
  } else openAccountLogin().catch(fail);
};
// The indicator follows the account while the window is open (a local read; the host syncs by itself).
setInterval(() => { if (readiness?.mode === 'signed_in' && !document.hidden) loadAccountConfig(); }, 60_000);

// ---------------------------------------------------------------- variable consumers

/** Which variable each consumer uses (name and version only) and the approved local launchers. */
let consumerState = { consumers: [], launchers: [], error: '' };
/** The variable picked in each selector, kept across refreshes until it is applied. */
const consumerChoice = {};
/** The last result shown under each consumer: a status sentence, never a value. */
const consumerNote = {};

async function loadConsumers() {
  const epoch = accountEpoch;
  try {
    const reply = await invoke('variable_consumer_list');
    if (!adoptAccount(epoch, reply.standing)) return;
    consumerState = { consumers: reply.consumers, launchers: reply.launchers ?? [], error: '' };
  } catch (error) {
    if (epoch !== accountEpoch) return;
    consumerState = { consumers: [], launchers: [], error: String(error?.message ?? error) };
  }
  renderConsumers();
}

const CHECK_RESULTS = {
  ok: '{destination} accepted the key.',
  rejected: '{destination} rejected the key.',
  failed: '{destination} answered with an unexpected error.',
  unreachable: 'Could not reach {destination}.',
};

function consumerRow(consumer) {
  const { id, binding } = consumer;
  const title = el('span', { class: 'cred-name', textContent: tx(consumer.title), translate: 'no' });
  const usable = binding?.state === 'ok';
  const configured = variableState.variables.find(v => v.name.toUpperCase() === consumer.credential);
  const status = !binding ? (configured ? badge(t('Using {name} · version {n}', { name: configured.name, n: configured.version }), 'ok') : badge(t('No variable chosen'), 'muted'))
    : usable ? badge(t('Using {name} · version {n}', { name: binding.variable.name, n: binding.variable.version }), 'ok')
    : badge(binding.state === 'deleted' ? t('Variable deleted') : t('Variable missing'), 'warn');
  const chosen = consumerChoice[id] ?? binding?.id ?? '';
  const select = el('select', { 'aria-label': t('Variable for {consumer}', { consumer: consumer.title }) },
    el('option', { value: '', textContent: t('Choose a variable') }),
    variableState.variables.map(v => el('option', { value: v.id, textContent: `${v.name} · v${v.version}`, selected: v.id === chosen, translate: 'no' })));
  const use = el('button', { type: 'button', textContent: t('Use this variable') });
  const sync = () => { use.disabled = !select.value || (usable && select.value === binding.id); };
  select.onchange = () => { consumerChoice[id] = select.value; sync(); };
  sync();
  use.onclick = () => busy(use, async () => {
    const variable = variableState.variables.find(v => v.id === select.value);
    const reply = await invoke('variable_consumer_bind', { consumer: id, variable: select.value, version: variable?.version });
    delete consumerChoice[id];
    consumerNote[id] = t('{consumer} now uses {name}.', { consumer: consumer.title, name: reply.binding.variable.name }) + (reply.restart_required ? ' ' + t('Restart the assistant to apply this change.') : '');
    await loadConsumers();
  });
  const actions = [select, use];
  if (binding) {
    const test = el('button', { type: 'button', class: 'secondary', textContent: t('Test key'), disabled: !usable,
      'aria-label': t('Test key with {destination}', { destination: consumer.destination }) });
    test.onclick = () => busy(test, async () => {
      const reply = await invoke('variable_consumer_check', { consumer: id });
      consumerNote[id] = t(CHECK_RESULTS[reply.result.status] ?? CHECK_RESULTS.failed, { destination: consumer.destination });
      await loadConsumers();
    });
    const stop = el('button', { type: 'button', class: 'secondary', textContent: t('Stop using') });
    stop.onclick = () => busy(stop, async () => {
      const reply = await invoke('variable_consumer_unbind', { consumer: id });
      delete consumerChoice[id];
      consumerNote[id] = t('{consumer} now uses its configured variable.', { consumer: consumer.title }) + (reply.restart_required ? ' ' + t('Restart the assistant to apply this change.') : '');
      await loadConsumers();
    });
    actions.push(test, stop);
  }
  return el('div', { class: 'row variable-row' },
    rowText(title, tx(consumer.summary), status),
    el('p', { class: 'hint variable-note', textContent: t('Sent only to {destination}. The value is never shown here.', { destination: consumer.destination }) }),
    el('div', { class: 'variable-actions' }, actions),
    consumerNote[id] ? el('p', { class: 'hint variable-note', role: 'status', textContent: consumerNote[id] }) : null);
}

function launcherRow(launcher) {
  const title = el('span', { class: 'cred-name', textContent: launcher.id, translate: 'no' });
  const uses = launcher.variables.map(v => v.state === 'ok'
    ? t('{env} from {name}', { env: v.env, name: v.variable.name })
    : t('{env}: variable deleted or missing', { env: v.env })).join(', ');
  const remove = el('button', { type: 'button', class: 'danger', textContent: t('Remove'), 'aria-label': t('Remove launcher {name}', { name: launcher.id }) });
  remove.onclick = () => busy(remove, async () => {
    await invoke('variable_launcher_remove', { id: launcher.id });
    await loadConsumers();
    done(t('Launcher {name} removed.', { name: launcher.id }));
  });
  const status = launcher.approved ? badge(t('Approved'), 'ok') : badge(t('Waiting for your approval'), 'warn');
  const actions = [remove];
  if (!launcher.approved) {
    // Only a person at this window approves; the digest names the definition shown here.
    const approve = el('button', { type: 'button', textContent: t('Approve'), 'aria-label': t('Approve launcher {name}', { name: launcher.id }) });
    approve.onclick = () => busy(approve, async () => {
      await invoke('variable_launcher_approve', { id: launcher.id, digest: launcher.digest });
      await loadConsumers();
      done(t('Launcher {name} approved.', { name: launcher.id }));
    });
    actions.unshift(approve);
  }
  const command = [launcher.program, ...(launcher.args ?? [])].join(' ');
  return el('div', { class: 'row variable-row' },
    rowText(title, launcher.description, status),
    el('p', { class: 'hint variable-note', textContent: command, translate: 'no' }),
    el('p', { class: 'hint variable-note', textContent: launcher.directory, translate: 'no' }),
    el('p', { class: 'hint variable-note', textContent: uses, translate: 'no' }),
    el('div', { class: 'variable-actions' }, actions));
}

/** What reads a `[secrets]` reference: the sources and settings that name it. */
function referenceUses(reference) {
  const uses = [];
  for (const resource of current?.map?.resources ?? []) {
    const tool = resource.tool;
    if (!tool) continue;
    const kind = toolNames[tool.type] ?? tool.type;
    if (tool.secret_ref === reference) uses.push(t('{kind} source {id}', { kind, id: resource.id }));
    if (tool.link_secret_ref === reference) uses.push(t('linking work to work items in {id}', { id: resource.id }));
  }
  if (current?.config?.knowledge_index?.embeddings?.secret_ref === reference) uses.push(t('semantic search'));
  return uses;
}

/** One configured credential reference: which variable it uses by name, and a choice of another. */
function referenceRow(reference, name) {
  const key = `ref:${reference}`;
  const uses = referenceUses(reference);
  const title = el('span', { class: 'cred-name', textContent: reference, translate: 'no' });
  const using = variableState.variables.find(v => v.name.toUpperCase() === name.toUpperCase());
  const status = using ? badge(t('Using {name} · version {n}', { name: using.name, n: using.version }), 'ok')
    : badge(t('Missing {name}', { name }), 'warn');
  const summary = uses.length ? t('Used by {uses}.', { uses: uses.join(', ') }) : t('Not used by any source.');
  const chosen = consumerChoice[key] ?? using?.name ?? '';
  const select = el('select', { 'aria-label': t('Variable for {consumer}', { consumer: reference }) },
    el('option', { value: '', textContent: t('Choose a variable') }),
    variableState.variables.map(v => el('option', { value: v.name, textContent: `${v.name} · v${v.version}`, selected: v.name === chosen, translate: 'no' })));
  const use = el('button', { type: 'button', textContent: t('Use this variable') });
  const sync = () => { use.disabled = !select.value || select.value === name; };
  select.onchange = () => { consumerChoice[key] = select.value; sync(); };
  sync();
  use.onclick = () => busy(use, async () => {
    const chosenName = select.value;
    current.config.secrets[reference] = chosenName;
    try { await save(); } catch (error) { await reload(); throw error; }
    delete consumerChoice[key];
    consumerNote[key] = t('{consumer} now uses {name}.', { consumer: reference, name: chosenName });
    renderConsumers();
  });
  return el('div', { class: 'row variable-row' },
    rowText(title, summary, status),
    el('div', { class: 'variable-actions' }, [select, use]),
    consumerNote[key] ? el('p', { class: 'hint variable-note', role: 'status', textContent: consumerNote[key] }) : null);
}

/** Plain text only: summaries come from the host's fixed registry; names and paths are user data. */
function renderConsumers() {
  const { consumers, launchers, error } = consumerState;
  const list = $('#consumers');
  list.replaceChildren();
  if (error) list.append(el('p', { class: 'hint', role: 'status', textContent: tx(error) }));
  for (const consumer of consumers) list.append(consumerRow(consumer));
  for (const [reference, name] of Object.entries(current?.config?.secrets ?? {})) list.append(referenceRow(reference, name));
  $('#launchers').hidden = !launchers.length;
  $('#launcher-list').replaceChildren(...launchers.map(launcherRow));
}

/** Plain text only: names and descriptions are user data, never markup or instructions. */
/** One line about where the catalog comes from and how current it is (metadata only, never a value). */
function syncSummary(sync) {
  if (!sync) return '';
  if (sync.mode === 'authoritative') return t('This computer holds your variables itself, so they are always current. Revision {n}.', { n: sync.revision });
  const loaded = sync.revision == null ? '' : t('Revision {n}, synchronized {when}.', { n: sync.revision, when: ago(sync.synced_at * 1000) });
  switch (sync.state) {
    case 'syncing': return t('Synchronizing…');
    case 'synced': return t('Synchronized with your account. {loaded}', { loaded });
    case 'conflict': return `${t('A change conflicted with a newer one. The list was refreshed: review it and try again.')} ${tx(sync.conflict ?? '')}`.trim();
    case 'error': return `${t('Synchronization failed; the last synchronized catalog is shown.')} ${loaded} ${tx(sync.error?.message ?? '')}`.trim();
    default: return t('Not synchronized yet.');
  }
}

function renderVariables() {
  const { variables, pending, error, sync } = variableState;
  const line = $('#variables-sync');
  line.textContent = syncSummary(sync);
  line.hidden = !line.textContent;
  const query = $('#variables-search').value.trim().toLowerCase();
  const shown = variables.filter(v => !query || v.name.toLowerCase().includes(query) || v.description.toLowerCase().includes(query));
  const list = $('#variables');
  list.replaceChildren();
  $('#variables-count').hidden = !variables.length;
  $('#variables-count').textContent = tn(variables.length, '{n} variable', '{n} variables');
  $('#variables-search-field').hidden = variables.length < 6;
  if (error) {
    list.append(el('p', { class: 'hint', role: 'status', textContent: tx(error) }));
  } else if (!variables.length) {
    list.append(el('p', { class: 'hint', textContent: t('No variables yet. Add one below, or ask your agent to request one.') }));
  } else if (!shown.length) {
    list.append(el('p', { class: 'hint', textContent: t('No variable matches this search.') }));
  }
  for (const variable of shown) list.append(variableRow(variable));
  renderPendingVariables(pending);
  updateVariableForm();
}

function variableRow(variable) {
  const { id, name } = variable;
  const title = el('span', { class: 'cred-name', textContent: name, translate: 'no' });
  const visible = revealedVariable?.id === id && revealedVariable.version === variable.version;
  const value = visible
    ? el('pre', { class: 'variable-value', textContent: revealedVariable.value, translate: 'no', tabIndex: 0, 'aria-label': t('Value of {name}', { name }) })
    : el('span', { class: 'variable-mask', textContent: '••••••••••', role: 'img', 'aria-label': t('Value hidden') });
  const reveal = el('button', { type: 'button', class: 'secondary', 'aria-pressed': String(visible),
    'aria-label': visible ? t('Hide the value of {name}', { name }) : t('Reveal the value of {name}', { name }) },
    icon(visible ? 'eye-off' : 'eye'), visible ? t('Hide') : t('Reveal'));
  reveal.onclick = () => busy(reveal, async () => {
    if (visible) return clearRevealedVariable();
    const epoch = accountEpoch, view = revealEpoch;
    const reply = await invoke('variable_reveal', { id });
    if (epoch !== accountEpoch || view !== revealEpoch || document.hidden) return;
    clearRevealedVariable(false);
    // The reply names the current version: show it against fresh metadata, never a stale row.
    variableState.variables = variableState.variables.map(v => v.id === id ? reply.variable : v);
    revealedVariable = { id, version: reply.variable.version, value: reply.value, timer: setTimeout(clearRevealedVariable, VALUE_SECONDS * 1000) };
    renderVariables();
  });
  const copyValue = el('button', { type: 'button', class: 'secondary', textContent: t('Copy value'), 'aria-label': t('Copy value of {name}', { name }) });
  copyValue.onclick = () => busy(copyValue, async () => {
    const epoch = accountEpoch, view = revealEpoch;
    const reply = await invoke('variable_reveal', { id });
    if (epoch !== accountEpoch || view !== revealEpoch || document.hidden) return;
    // Only the clipboard receives it: no toast, notice or log carries the value.
    if (!await copy(reply.value)) throw t('The value could not be copied. Reveal it and copy it by hand.');
    await scheduleClipboardCleanup(reply.value);
    done(t('Value of {name} copied. The clipboard is cleared in a minute if it is unchanged.', { name }));
  });
  const edit = el('button', { type: 'button', class: 'secondary', textContent: t('Edit'), 'aria-expanded': String(editingVariable === id), 'aria-label': t('Edit {name}', { name }) });
  edit.onclick = () => {
    editingVariable = editingVariable === id ? null : id;
    variableDraft = null;
    renderVariables();
  };
  const armed = deletingVariable === id;
  const remove = el('button', { type: 'button', class: 'danger', textContent: armed ? t('Confirm deletion') : t('Delete'),
    'aria-label': armed ? t('Confirm deletion of {name}', { name }) : t('Delete {name}', { name }) });
  remove.onclick = () => busy(remove, async () => {
    if (!armed) {
      // An irreversible action asks for a second tap, and disarms itself.
      deletingVariable = id;
      clearTimeout(armTimer);
      armTimer = setTimeout(() => { if (deletingVariable === id) { deletingVariable = null; renderVariables(); } }, 8000);
      return renderVariables();
    }
    deletingVariable = null;
    await changeVariable('variable_delete', variable);
    if (revealedVariable?.id === id) clearRevealedVariable(false);
    if (editingVariable === id) editingVariable = null;
    await loadVariables();
    done(t('{name} deleted.', { name }));
  });
  const actions = [reveal, copyValue, edit, remove];
  if (armed) {
    const keep = el('button', { type: 'button', class: 'secondary', textContent: t('Cancel'), 'aria-label': t('Cancel deletion of {name}', { name }) });
    keep.onclick = () => { deletingVariable = null; renderVariables(); };
    actions.push(keep);
  }
  return el('div', { class: 'row variable-row' },
    rowText(title, variable.description, badge(t('Stored · version {n}', { n: variable.version }), 'ok')),
    el('div', { class: 'variable-value-box' }, value),
    el('div', { class: 'variable-actions' }, actions),
    armed ? el('p', { class: 'hint variable-note', role: 'status', textContent: t('Deleting removes it for good and its value cannot be recovered. It does not revoke the credential at its provider.') }) : null,
    editingVariable === id ? variableEditor(variable) : null);
}

/** Two independent edits of the same entry: description only, and a replacement value only. */
function variableEditor(variable) {
  const { id, name } = variable;
  const description = el('input', { value: variableDraft?.id === id ? variableDraft.description : variable.description, maxLength: 500,
    autocomplete: 'off', spellcheck: false, 'aria-label': t('Description of {name}', { name }) });
  const saveDescription = el('button', { type: 'button', textContent: t('Save description') });
  const sync = () => { saveDescription.disabled = !description.value.trim() || description.value.trim() === variable.description; };
  description.oninput = () => { variableDraft = { id, description: description.value }; sync(); };
  sync();
  saveDescription.onclick = () => busy(saveDescription, async () => {
    const text = description.value.trim();
    if (!text || text.length > 500 || UNSAFE_TEXT.test(text)) throw t('A description is plain text on one line, up to 500 characters.');
    await changeVariable('variable_update', variable, { description: text });
    variableDraft = editingVariable = null;
    await loadVariables();
    done(t('Description of {name} updated.', { name }));
  });

  const input = el('textarea', { class: 'secret-entry', rows: 2, autocomplete: 'off', autocapitalize: 'off', spellcheck: false,
    placeholder: t('Enter the new value'), 'aria-label': t('New value of {name}', { name }) });
  const show = el('input', { type: 'checkbox' });
  show.onchange = () => input.classList.toggle('shown', show.checked);
  const replace = el('button', { type: 'button', textContent: t('Replace value'), disabled: true });
  input.oninput = () => { replace.disabled = !input.value; };
  replace.onclick = () => busy(replace, async () => {
    checkVariableValue(input.value);
    const next = input.value;
    input.value = '';
    await changeVariable('variable_update', variable, { value: next });
    if (revealedVariable?.id === id) clearRevealedVariable(false);
    variableDraft = editingVariable = null;
    await loadVariables();
    done(t('Value of {name} replaced. Its name and description are unchanged.', { name }));
  });
  const close = el('button', { type: 'button', class: 'secondary', textContent: t('Cancel') });
  close.onclick = () => { input.value = ''; editingVariable = variableDraft = null; renderVariables(); };
  return el('div', { class: 'variable-edit' },
    el('label', {}, t('Description'), description),
    el('div', { class: 'actions' }, saveDescription),
    el('label', {}, t('New value'), el('small', { textContent: t('The name, description and references stay the same.') }), input),
    el('label', { class: 'check' }, show, icon('eye'), ` ${t('Show the value while I type')}`),
    el('div', { class: 'actions' }, replace, close));
}

function renderPendingVariables(pending) {
  const box = $('#variables-pending');
  box.hidden = !pending.length;
  box.replaceChildren();
  if (!pending.length) return;
  box.append(el('p', { class: 'hint', textContent: t('Your agent asked you to store these. Type each value here: the agent never sees it.') }));
  for (const entry of pending) {
    const title = el('span', { class: 'cred-name', textContent: entry.name, translate: 'no' });
    const input = el('textarea', { class: 'secret-entry', rows: 1, autocomplete: 'off', autocapitalize: 'off', spellcheck: false,
      placeholder: t('Enter the value'), 'aria-label': t('Value of {name}', { name: entry.name }) });
    const save = el('button', { textContent: t('Save'), disabled: true });
    const cancel = el('button', { class: 'secondary', textContent: t('Cancel'), 'aria-label': t('Cancel request for {name}', { name: entry.name }) });
    input.oninput = () => { save.disabled = !input.value; };
    save.onclick = () => busy(save, async () => {
      checkVariableValue(input.value);
      await invoke('variable_entry_complete', { request: entry.request, value: input.value });
      input.value = '';
      await loadVariables();
      done(t('{name} stored.', { name: entry.name }));
    });
    cancel.onclick = () => busy(cancel, async () => {
      await invoke('variable_entry_cancel', { request: entry.request });
      await loadVariables();
      done(t('Request for {name} cancelled.', { name: entry.name }));
    });
    box.append(el('div', { class: 'row' }, rowText(title, entry.description, badge(t('Waiting for you'), 'warn')), input, save, cancel));
  }
}

function checkVariableValue(value) {
  if (!value || value.includes('\u0000') || new TextEncoder().encode(value).length > 16384) throw t('A value has 1 byte to 16 KiB of text without NUL characters.');
}

function variableForm() {
  return { name: $('#variable-name').value.trim(), description: $('#variable-description').value.trim(), value: $('#variable-value').value };
}
function updateVariableForm() {
  const form = variableForm();
  $('#variable-create').disabled = !form.name || !form.description || !form.value;
}

$('#variables-search').oninput = renderVariables;
for (const id of ['#variable-name', '#variable-description', '#variable-value']) $(id).oninput = updateVariableForm;
$('#variable-show').onchange = event => $('#variable-value').classList.toggle('shown', event.target.checked);
$('#variable-create').onclick = event => busy(event.currentTarget, async () => {
  const form = variableForm();
  if (!VARIABLE_NAME.test(form.name)) throw t('A variable name uses letters, digits and _, cannot start with a digit and has at most 128 characters.');
  if (form.description.length > 500 || UNSAFE_TEXT.test(form.description)) throw t('A description is plain text on one line, up to 500 characters.');
  checkVariableValue(form.value);
  const fingerprint = JSON.stringify([form.name, form.description, form.value]);
  if (variableAttempt?.fingerprint !== fingerprint) variableAttempt = { id: crypto.randomUUID(), fingerprint };
  const reply = await invoke('variable_create', { id: variableAttempt.id, ...form });
  variableAttempt = null;
  for (const id of ['#variable-name', '#variable-description', '#variable-value']) $(id).value = '';
  await loadVariables();
  done(reply.replayed ? t('{name} was already stored.', { name: form.name }) : t('{name} stored.', { name: form.name }));
});
$('#variables-refresh').onclick = event => busy(event.currentTarget, async () => {
  // Rereads this computer's list; bringing the account's catalog here is the sidebar's Synchronize.
  await loadVariables();
  done(t('Variables refreshed.'));
});
// The periodic refresh never rebuilds a form the user is typing in.
const typingVariable = () => editingVariable || document.activeElement?.closest?.('#consumers') || [...document.querySelectorAll('#variables-pending textarea')].some(field => field.value);
setInterval(() => { if (tab === 'settings' && !$('#panel-variables').closest('[role=tabpanel]').hidden && !typingVariable()) loadVariables(); }, 10_000);

const effortNames = { default: t('Model default'), none: t('No reasoning'), minimal: t('Minimal'), low: t('Low'), medium: t('Medium'), high: t('High'), xhigh: t('Very high'), max: t('Maximum') };
let llmCatalog = { providers: [] };
let llmRows = [];

/** Keep each configured model in priority order, including repeated providers. */
function chainRows(config) {
  return config.llm.chain.length
    ? config.llm.chain.map(choice => ({ ...choice }))
    : [{ provider: config.llm.provider, model: config.llm.model, effort: 'max', enabled: true }];
}

function providerStatus(info) {
  if (!info) return t('Provider not available in this version');
  if (info.transport === 'cli') return info.available ? `${t('CLI installed')}${info.version ? ' · ' + info.version : ''}` : t('CLI not installed on this computer');
  const source = live.credentials[info.credential];
  return source && source !== 'missing' && source !== 'unused' ? t('API · {name} set', { name: info.credential }) : t('API · {name} missing in Variables', { name: info.credential });
}

function select(caption, label, options, value) {
  const element = el('select', { 'aria-label': label });
  for (const [optionValue, text] of options) element.append(new Option(text, optionValue));
  element.value = value;
  return [el('label', { class: 'field' }, el('small', { textContent: caption }), element), element];
}

function renderChain() {
  const list = $('#llm-chain');
  list.replaceChildren();
  const providerSelect = $('#add-llm-provider');
  const selected = providerSelect.value;
  providerSelect.replaceChildren(...llmCatalog.providers.map(info => new Option(providerNames[info.id] || info.id, info.id)));
  if (selected) providerSelect.value = selected;
  const addedProvider = llmCatalog.providers.find(info => info.id === providerSelect.value);
  $('#add-llm').disabled = llmRows.length >= 16 || !addedProvider || (addedProvider.id === 'opencode' && !addedProvider.models.length);
  providerSelect.onchange = renderChain;
  let active = 0;
  llmRows.forEach((row, index) => {
    const info = llmCatalog.providers.find(provider => provider.id === row.provider);
    const name = providerNames[row.provider] || row.provider;
    const rank = el('span', { class: 'rank', textContent: !row.enabled ? t('Off') : active++ === 0 ? t('Default') : t('Fallback {n}', { n: active - 1 }) });
    const toggle = el('input', { type: 'checkbox', checked: row.enabled, disabled: !row.enabled && !(info?.available), 'aria-label': t('Turn on {name}', { name }) });
    toggle.onchange = () => { row.enabled = toggle.checked; markDirty({ target: list }); renderChain(); };
    const label = el('label', { class: 'check' }, toggle,
      el('div', {}, el('strong', { textContent: name }), el('small', { textContent: providerStatus(info) })));
    const models = (info?.models ?? []).map(model => [model.id, model.name]);
    const openCode = row.provider === 'opencode';
    const knownModel = models.some(([id]) => id === row.model);
    if (row.model && !knownModel && !openCode) models.unshift([row.model, `${row.model} (${t('current')})`]);
    const [modelField, model] = select(t('Model'), t('{name} model', { name }), models, row.model);
    if (openCode && !knownModel) {
      model.prepend(el('option', { value: '', textContent: t('Select an OpenCode favorite'), disabled: true, selected: true }));
      modelField.append(el('small', { textContent: t('Saved model: {model}. It is not in the available favorites.', { model: row.model }) }));
    }
    model.disabled = openCode && !models.length;
    model.title = row.model;
    const efforts = () => info?.models.find(m => m.id === row.model)?.efforts ?? (openCode ? [] : info?.efforts ?? [row.effort]);
    const effortOptions = () => {
      const values = efforts();
      return (openCode || values.includes(row.effort) ? values : [row.effort, ...values]).map(value => [value, openCode && value !== 'default' ? value : effortNames[value] || value]);
    };
    const [effortField, effort] = select(t('Effort'), t('{name} effort', { name }), effortOptions(), row.effort);
    if (openCode && !efforts().includes(row.effort)) {
      effort.prepend(el('option', { value: '', textContent: t('Select a supported variant'), disabled: true, selected: true }));
      effortField.append(el('small', { textContent: t('Saved effort: {effort}.', { effort: row.effort }) }));
    }
    effort.disabled = openCode && !efforts().length;
    model.onchange = () => {
      row.model = model.value;
      const supported = efforts();
      if (!supported.includes(row.effort)) row.effort = info?.models.find(m => m.id === row.model)?.default_effort ?? supported[0];
      markDirty({ target: list });
      renderChain();
    };
    effort.onchange = () => { row.effort = effort.value; markDirty({ target: list }); };
    const order = el('div', { class: 'order' });
    for (const [text, offset, aria] of [['↑', -1, t('Raise the priority of {name}', { name })], ['↓', 1, t('Lower the priority of {name}', { name })]]) {
      const button = el('button', { class: 'secondary', textContent: text, 'aria-label': aria, disabled: !llmRows[index + offset] });
      button.onclick = () => {
        [llmRows[index], llmRows[index + offset]] = [llmRows[index + offset], llmRows[index]];
        markDirty({ target: list });
        renderChain();
      };
      order.append(button);
    }
    const remove = el('button', { class: 'secondary small', textContent: t('Remove'), 'aria-label': t('Remove {name} model', { name }) });
    remove.onclick = () => { llmRows.splice(index, 1); markDirty({ target: list }); renderChain(); };
    order.append(remove);
    list.append(el('li', { class: 'chain-row' + (row.enabled ? '' : ' off') }, rank, label, modelField, effortField, order));
  });
}

async function loadChain() {
  // Without detection the configured order still loads, so saving never drops it.
  try { llmCatalog = await invoke('llm_providers'); }
  catch (error) { fail(error); }
  llmRows = chainRows(current.config);
  renderChain();
}

async function loadSelfChat() {
  const config = current.config;
  $('#self-chat-badge').className = `badge ${config.graph.self_chat ? 'ok' : 'muted'}`;
  $('#self-chat-badge').textContent = config.graph.self_chat ? t('Enabled') : t('Disabled');
  if (!config.graph.self_chat) {
    $('#self-chat-state').textContent = t('Write to yourself in Teams (your notes or a chat with yourself) and the assistant answers right there, with the sources authorized for that chat.');
    return;
  }
  let diagnostic;
  try { diagnostic = (await invoke('self_chat_status')).diagnostics; } catch { /* shown as pending */ }
  $('#self-chat-state').textContent = t('Chat {id} · received by webhook and polled every 10 s · last poll: {poll} · unresolved outputs: {n}', {
    id: config.graph.self_chat.id,
    poll: diagnostic?.cursor ? new Date(diagnostic.cursor[1] * 1000).toLocaleString(uiLocale()) : t('pending'),
    n: diagnostic?.unresolved_outputs ?? 0,
  });
}

function collectSettings() {
  const config = current.config;
  config.llm.chain = llmRows.map(({ provider, model, effort, enabled }) => ({ provider, model, effort, enabled }));
  config.llm.style = $('#llm-style').value.trim();
  config.llm.language = $('#llm-language').value;
  config.graph.tenant_id = $('#tenant-id').value.trim();
  config.graph.client_id = $('#client-id').value.trim();
  config.graph.user_id = $('#user-id').value.trim();
  config.graph.allowed_chats = lines($('#allowed-chats').value);
  config.graph.discover_all_chats = $('#discover-chats').checked;
  config.server.public_url = $('#public-url').value.trim();
  config.server.bind = $('#bind').value.trim();
  config.server.cloudflare_tunnel = $('#cloudflare-tunnel').checked;
  config.policy.dry_run = $('#dry-run').checked;
  collectRegistrationSettings();
  collectEmbeddings();
}

/** The reference the embeddings credential is mapped from in `[secrets]`. */
const EMBEDDINGS_SECRET = 'secret://knowledge/embeddings';

function collectEmbeddings() {
  const config = current.config;
  const url = $('#embeddings-url').value.trim();
  const model = $('#embeddings-model').value.trim();
  const credential = $('#embeddings-credential').value.trim();
  if (!url && !model && !credential) {
    delete config.knowledge_index;
    delete config.secrets[EMBEDDINGS_SECRET];
    return;
  }
  if (!url || !model) throw t('Semantic search needs both the endpoint URL and the model, or neither.');
  if (credential && !/^[A-Z][A-Z0-9_]{0,63}$/.test(credential)) throw t('The credential name uses capital letters, digits and _, like OPENAI_API_KEY.');
  if (credential) config.secrets[EMBEDDINGS_SECRET] = credential;
  else delete config.secrets[EMBEDDINGS_SECRET];
  config.knowledge_index = { embeddings: { url, model, ...(credential ? { secret_ref: EMBEDDINGS_SECRET } : {}) } };
}

function renderEmbeddings() {
  const embeddings = current.config.knowledge_index?.embeddings;
  field('#embeddings-url', embeddings?.url);
  field('#embeddings-model', embeddings?.model);
  field('#embeddings-credential', embeddings?.secret_ref ? current.config.secrets[embeddings.secret_ref] : '');
}

async function save() {
  if (!llmRows.some(row => row.enabled)) throw t('Turn on at least one language model.');
  collectSettings();
  await invoke('save_settings', { config: current.config, map: current.map, tunnelConfig: $('#tunnel-config').value.trim() });
  await reload();
}

/** Operations that change the profile on the host: pending edits are saved first, then everything reloads. */
async function applying(task) {
  if (dirty) await save();
  const result = await task();
  await reload();
  return result;
}

function startAssistant(button) {
  return busy(button, async () => {
    if (dirty) await save();
    await invoke('start_assistant');
    await refreshLive();
    done(t('Assistant started.'));
  });
}

// ---------------------------------------------------------------- daily task record

const activityWindow = new URLSearchParams(window.location.search).get('activity');
let activityRunning = false;
let activityDay = null;
let activityTask = null;
let activityDayRequest = 0;
let activityDetailRequest = 0;
let activityListRequest = 0;
let activityDaysPayload = '';
let activityDayFocus = null;
let activityTaskFocus = null;
let activityDaysScroll = 0;
let activityTasksScroll = 0;
const hoursText = hours => t('{hours} h', { hours: num(hours) });
const dayText = day => new Date(`${day}T12:00:00`).toLocaleDateString(uiLocale(), { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' });

function renderRegistrationSettings() {
  const s = current.config.activity_registration;
  $('#registration-enabled').checked = s.enabled;
  for (const [id, value] of Object.entries({ hours: s.daily_hours, schedule: s.schedule, at: s.at,
    interval: s.interval_minutes, start: s.window_start, end: s.window_end, zone: s.time_zone,
    effort: s.effort_field })) field('#registration-' + id, value);
  for (const day of document.querySelectorAll('#registration-weekdays input')) day.checked = s.weekdays.includes(Number(day.dataset.day));
  renderScheduleFields();
  const sources = current.map.resources.filter(r => r.tool?.type === 'azure_devops_status');
  $('#registration-sources').replaceChildren(...sources.map(r => {
    const check = el('input', { type: 'checkbox', checked: s.sources.includes(r.id), disabled: !r.enabled });
    check.dataset.source = r.id;
    check.onchange = markDirty;
    return el('label', { class: 'check' }, check, el('span', {}, r.description || r.id,
      !r.enabled ? el('small', { textContent: t('Enable this source in Knowledge first.') }) : null));
  }));
  if (!sources.length) $('#registration-sources').append(el('p', { class: 'hint', textContent: t('Add an Azure DevOps source in Knowledge to read existing tasks.') }));
}
function renderScheduleFields() {
  const daily = $('#registration-schedule').value !== 'interval';
  $('#registration-at-field').hidden = !daily;
  for (const id of ['#registration-interval-field', '#registration-start-field', '#registration-end-field']) $(id).hidden = daily;
}
function collectRegistrationSettings() {
  const s = current.config.activity_registration;
  if (!document.querySelector('#registration-weekdays input:checked')) {
    showTab('settings', 'panel-registration-schedule');
    throw t('Choose at least one workday.');
  }
  Object.assign(s, { enabled: $('#registration-enabled').checked,
    daily_hours: Number($('#registration-hours').value), schedule: $('#registration-schedule').value,
    at: $('#registration-at').value, interval_minutes: Number($('#registration-interval').value),
    window_start: $('#registration-start').value, window_end: $('#registration-end').value,
    time_zone: $('#registration-zone').value.trim(),
    weekdays: [...document.querySelectorAll('#registration-weekdays input:checked')].map(day => Number(day.dataset.day)),
    effort_field: $('#registration-effort').value.trim(),
    sources: [...document.querySelectorAll('#registration-sources input:checked')].map(c => c.dataset.source) });
}
function renderActivityNotice() {
  $('#activity-notice').hidden = !activityRunning;
  $('#activity-notice-text').textContent = t('Saving the day’s tasks…');
}
function activityDayCard(day) {
  const open = el('button', { type: 'button', class: 'card card-button' },
    el('span', { class: 'card-main' }, el('strong', { textContent: dayText(day.day) }),
      el('span', { class: 'card-meta' }, badge(tn(day.tasks, '1 task', '{n} tasks'), 'muted'),
        el('span', { textContent: hoursText(day.hours) }),
        day.target_hours > day.hours ? badge(t('Below daily target'), 'warn') : null)), icon('arrow'));
  open.onclick = () => { activityDaysScroll = window.scrollY; activityDayFocus = open; openActivityDay(day.day).catch(fail); };
  return open;
}
function showActivityView(view) {
  for (const name of ['master', 'day-view', 'detail']) $(`#activity-${name}`).hidden = name !== view;
}
async function openActivityDay(day, focus = true) {
  detailTrails.activity = [];
  const request = ++activityDayRequest;
  ++activityDetailRequest;
  activityDay = day; activityTask = null;
  showActivityView('day-view');
  $('#activity-day-title').textContent = dayText(day);
  $('#activity-day-tasks').replaceChildren();
  $('#activity-day-warning').hidden = true;
  $('#activity-day-progress').textContent = t('Loading…');
  $('#activity-day-tasks').setAttribute('aria-busy', 'true');
  if (focus) { $('#activity-day-title').focus({ preventScroll: true }); $('#activity-day-view').scrollIntoView({ block: 'start' }); }
  try {
    const history = await invoke('activity_history', { day });
    if (request !== activityDayRequest) return;
    const record = history.record;
    $('#activity-day-progress').textContent = record ? `${tn(record.tasks.length, '1 task', '{n} tasks')} · ${hoursText(record.hours)}` : t('This day has not been saved yet.');
    const below = record && record.target_hours > record.hours;
    $('#activity-day-warning').hidden = !below;
    $('#activity-day-warning').textContent = below ? t('{recorded} of {target} h. The daily target is advisory; all tasks were saved.', { recorded: num(record.hours), target: num(record.target_hours) }) : '';
    $('#activity-day-tasks').replaceChildren(...(record?.tasks || []).map(task => {
      const card = todoItem(task.item, button => {
        activityTasksScroll = window.scrollY; activityTaskFocus = button;
        activityTask = task; loadActivityTask();
      }, { task: true, source: task.source });
      card.append(el('div', { class: 'todo-context' }, el('span', { textContent: hoursText(task.hours) }),
        task.parent ? link(`#${task.parent.id} ${task.parent.title}`, task.parent.url) : null));
      return card;
    }));
    if (record && !record.tasks.length) $('#activity-day-tasks').append(el('p', { class: 'empty', textContent: t('No tasks were recorded for this day.') }));
  } catch (error) {
    if (request === activityDayRequest) $('#activity-day-progress').textContent = tx(String(error?.message ?? error));
  } finally {
    if (request === activityDayRequest) $('#activity-day-tasks').setAttribute('aria-busy', 'false');
  }
}
async function loadActivityTask() {
  if (!activityTask) return;
  const request = ++activityDetailRequest;
  const task = activityTask;
  showActivityView('detail');
  $('#activity-detail-content').replaceChildren();
  $('#activity-detail-content').setAttribute('aria-busy', 'true');
  $('#activity-detail-progress').textContent = t('Loading detail…');
  $('#activity-detail-refresh').disabled = true;
  $('#activity-task-back').focus({ preventScroll: true });
  $('#activity-detail').scrollIntoView({ block: 'start' });
  try {
    const args = { id: task.item.id, project: task.item.project };
    if (task.source) args.source = task.source;
    const detail = await invoke('get_azure_task', args);
    if (request !== activityDetailRequest) return;
    renderTodoDetail(detail, 'activity');
    $('#activity-detail-title').focus({ preventScroll: true });
    $('#activity-detail-progress').textContent = '';
  } catch (error) {
    if (request !== activityDetailRequest) return;
    $('#activity-detail-progress').textContent = tx(String(error?.message ?? error));
    $('#activity-detail-content').replaceChildren(el('p', { class: 'hint', textContent: t('Open the original item or refresh to try again.') }), todoOriginal(task.item));
  } finally {
    if (request === activityDetailRequest) {
      $('#activity-detail-content').setAttribute('aria-busy', 'false');
      $('#activity-detail-refresh').disabled = false;
    }
  }
}
async function loadRegistration() {
  const request = ++activityListRequest;
  const args = {};
  if ($('#activity-log-from').value) args.from = $('#activity-log-from').value;
  if ($('#activity-log-to').value) args.day = $('#activity-log-to').value;
  const history = await invoke('activity_history', args);
  if (request !== activityListRequest) return;
  if (!$('#activity-day').value) field('#activity-day', history.day);
  const finished = activityRunning && !history.running;
  activityRunning = history.running;
  $('#activity-run').disabled = activityRunning;
  $('#activity-progress').hidden = !activityRunning;
  $('#nav-activities').hidden = true;
  renderActivityNotice();
  const payload = JSON.stringify(history.days);
  if (payload !== activityDaysPayload) {
    activityDaysPayload = payload;
    $('#activity-log').replaceChildren(...history.days.map(activityDayCard));
    if (!history.days.length) $('#activity-log').append(el('p', { class: 'empty', textContent: t('No recorded days yet. Save a day’s tasks or enable the schedule in Settings.') }));
    $('#activity-log-total').textContent = tn(history.days.length, '1 day', '{n} days');
  }
  const latest = history.last_run || history.runs[0];
  $('#activity-result').hidden = !latest || !['failed', 'interrupted'].includes(latest.status);
  $('#activity-result').textContent = latest?.diagnostics?.error ? tx(latest.diagnostics.error) : t('The last task snapshot did not finish. Try saving the day again.');
  if (finished && latest?.status === 'completed') done(t('Daily tasks saved.'));
  if (finished && activityDay && !activityTask) await openActivityDay(activityDay, false);
}

// ---------------------------------------------------------------- messages

const statusLabels = {
  sent: [t('Answered'), 'ok'], dry_run: [t('Observed (not sent)'), 'info'], sending: [t('Sending'), 'active'],
  processing: [t('In progress'), 'active'], pending: [t('Queued'), 'active'], failed: [t('Error'), 'error'],
  uncertain: [t('Uncertain send'), 'error'], refining: [t('Generating'), 'active'],
};
const reasonLabels = {
  question_associated: t('The message was associated with a question. Generate an answer to review it.'),
  no_matching_question: t('No configured question matched. Associate it with a question to handle similar messages.'),
  question_matching_unavailable: t('The question could not be identified. Left for review.'),
  question_sources_unavailable: t('The question was identified, but none of its sources is enabled and authorized for this conversation.'),
  rejected_answer_repeated: t('The answer repeats a response you rejected for this question. Left for review.'),
  ineligible_message: t('Not addressed to the assistant: your own, from a group without a mention, too old, empty or an answer of the app.'),
  sensitive_question: t('The question contains sensitive data; it is left to you.'),
  no_authorized_resource: t('No source is authorized for this conversation.'),
  informational_message: t('Informational message: it asks for no answer.'),
  personal_request: t('It asks you for a call, a meeting, a joint review or your availability: it is left to you.'),
  deterministic_greeting: t('Greeting answered with the configured text.'),
  deterministic_thanks: t('Shared link: the sender was thanked.'),
  supported_answer: t('Answer generated.'),
  unsafe_proposal: t('Withheld: it contained sensitive data or did not fit in a Teams message.'),
  unsafe_answer: t('Withheld: it contained sensitive data or did not fit in a Teams message.'),
  reference_selection_failed: t('Withheld while choosing references (earlier version).'),
  final_gate: t('Withheld by the final review (earlier version).'),
  message_changed: t('The message changed, was deleted or stopped being eligible before sending.'),
  send_result_unknown_manual_review: t('It is unknown whether the send arrived: check it in Teams (never retried).'),
  read_or_provider_error: t('Error reading Teams or calling a provider; it is retried.'),
  generating: t('Writing the answer.'), retrieving: t('Searching the sources.'), tool_started: t('Reading a tool.'),
  selecting_references: t('Choosing references.'), final_review: t('Final review.'),
  holding_reply: t('Sending the holding notice.'),
  linking: t('Linking work to work items.'),
  links_planned: t('Link plan shown; waiting for your confirmation.'),
  links_applied: t('Confirmed links written to Azure DevOps (never retried).'),
  links_cancelled: t('Link plan cancelled; nothing was linked.'),
  links_unresolved: t('The link request could not be resolved; nothing was planned.'),
  links_not_configured: t('Linking asked for, but no source has a write credential.'),
  refining: t('Writing the answer at your request.'),
  draft_ready: t('Answer ready: review it and send it from here.'),
  refinement_failed: t('The answer could not be written again; the earlier proposal is kept.'),
  manual_send: t('Sent by you from the app after reviewing it.'),
};
const kindLabels = { self: t('Personal chat'), direct: t('Direct'), group: t('Group'), unsupported: t('Other') };
const intentLabels = { report_tasks: t('Report tasks'), question: t('question'), activity_review: t('activity review'), greeting: t('greeting'), shared_link: t('shared link'), link: t('link work to work items'),
  statement: t('informational'), personal: t('request for you') };
const linkLabels = { sending: t('sending'), linked: t('linked'), already_linked: t('already linked'), failed: t('failed'), uncertain: t('uncertain (check the work item; never retried)') };
let messageRows = [];
let messageFilter = 'all';
const openMessages = new Set();
/** The message to scroll to once Messages renders it (opened from Home). */
let focusMessage = null;

function messageState(row) {
  const reason = row.audit.reason || '';
  if (row.status === 'ignored') {
    if (reason === 'ineligible_message') return [t('Not addressed'), 'muted'];
    if (reason === 'draft_ready') return [t('Draft ready'), 'info'];
    if (/^(unsafe_|invalid_references|reference_selection_failed|final_gate|refinement_failed)/.test(reason)) return [t('Withheld'), 'warn'];
    return [t('No answer'), 'muted'];
  }
  return statusLabels[row.status] || [row.status, 'muted'];
}

function reasonText(reason, audit) {
  if (!reason) return '';
  if (reason === 'refinement_failed' && !audit?.proposed?.trim()) return t('The answer could not be written; try again.');
  if (reason.startsWith('invalid_references')) return t('Withheld: references or links that could not be verified ({detail}).', { detail: reason.replace('invalid_references: ', '') });
  return reasonLabels[reason] || reason;
}

const millis = row => row.audit.received_at ?? row.created_at * 1000;
const relative = new Intl.RelativeTimeFormat(uiLocale(), { numeric: 'auto' });
function ago(at) {
  const seconds = Math.round((at - Date.now()) / 1000);
  for (const [unit, size] of [['day', 86400], ['hour', 3600], ['minute', 60]]) {
    if (Math.abs(seconds) >= size) return relative.format(Math.round(seconds / size), unit);
  }
  return t('just now');
}
const when = row => new Date(millis(row)).toLocaleString(uiLocale());
const whenShort = row => new Date(millis(row)).toLocaleString(uiLocale(), { dateStyle: 'medium', timeStyle: 'short' });
const ineligible = row => row.audit.reason === 'ineligible_message' && row.status === 'ignored';
/** Lowercase without accents, so «revision» finds «revisión». */
const folded = text => String(text ?? '').normalize('NFD').replace(/\p{Diacritic}/gu, '').toLowerCase();
/** Whether a message's recorded text, outcome or kind contains every searched word. */
function matchesSearch(row, query) {
  const words = folded(query).split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const audit = row.audit;
  const text = folded([audit.question, audit.resolved_question, audit.topic, audit.proposed, audit.sent, audit.refinement,
    audit.provider, reasonText(audit.reason), messageState(row)[0], kindOf(row)].join(' '));
  return words.every(word => text.includes(word));
}

function excerpt(row) {
  const audit = row.audit;
  const reply = audit.sent || audit.proposed;
  return audit.question
    || (reply ? `${t('Answer')}: ${reply.replace(/\s+/g, ' ').slice(0, 140)}` : null)
    || (audit.reason === 'ineligible_message' ? t('(the text of messages not addressed to the assistant is not stored)') : t('(no text recorded)'));
}

function kindOf(row) {
  return kindLabels[row.audit.conversation_kind] || (row.resource.includes('48:notes') ? t('Personal chat') : row.resource.startsWith('simulation:') ? t('Simulation') : 'Teams');
}

function messageField(container, label, text, pre = false) {
  if (!text) return;
  const body = pre === 'markdown' ? markdown(text) : el(pre ? 'pre' : 'p', { textContent: text });
  container.append(el('div', { class: 'field-block' }, el('strong', { textContent: label }), body));
}

/** What the answer cited and the Wiki pages it read without citing: the answer links them in
 *  its text, and this record keeps the full list. */
function referencesField(container, audit) {
  const cited = new Set(audit.used_sources || []);
  const seen = new Set();
  const once = ref => !seen.has(ref.url) && seen.add(ref.url);
  const used = (audit.references || []).filter(ref => cited.has(ref.id) && once(ref));
  const pages = (audit.references || []).filter(ref => ref.kind === 'wiki' && !cited.has(ref.id) && once(ref));
  const list = refs => el('ul', {}, ...refs.map(ref => el('li', {}, link(ref.label || ref.id, ref.url))));
  if (used.length) container.append(el('div', { class: 'field-block' }, el('strong', { textContent: t('Cited references') }), list(used)));
  if (pages.length) container.append(el('div', { class: 'field-block' }, el('strong', { textContent: t('Wiki pages read, not cited') }), list(pages)));
}

function matchesFilter(row, filter) {
  const [, tone] = messageState(row);
  return filter === 'all' || (filter === 'sent' && tone === 'ok')
    || (filter === 'withheld' && (tone === 'warn' || row.audit.reason === 'draft_ready' || (tone === 'muted' && row.audit.reason !== 'ineligible_message')))
    || (filter === 'error' && tone === 'error') || (filter === 'active' && tone === 'active');
}

/** Context typed for an answer, kept across the list's refreshes. */
const answerContext = new Map();
/** Sources chosen for an answer (resource → ids), kept across refreshes; absent means its last choice. */
const answerSources = new Map();
/** Messages whose source chooser is open. */
const openSourceChoices = new Set();
let sendArmed = null;
/** A message addressed to the assistant that was not sent an answer can get one written from here
 *  (one with sensitive data never reaches a model). */
const unsent = row => ['ignored', 'dry_run'].includes(row.status) && !row.audit.sent && !!row.audit.question
  && row.audit.reason !== 'sensitive_question' && !row.resource.startsWith('simulation:');
/** An unsent message whose answer exists (withheld, observed or written here) can be sent. */
const unsentDraft = row => unsent(row) && !!row.audit.proposed?.trim();

/** The sources the user may choose for an answer: enabled and allowed external processing, like the
 *  test chat. None chosen means the ones authorized for the conversation. */
function sourceChooser(row) {
  const usable = live.map.resources.filter(r => r.enabled && r.external_processing);
  const chosen = answerSources.get(row.resource) ?? new Set((row.audit.chosen_sources || []).filter(id => usable.some(r => r.id === id)));
  const conversation = row.resource.replace(/\/messages\/.*$/, '');
  const authorized = r => r.allowed_conversations.some(c => c === conversation || c === '*');
  const summaryNote = el('small');
  const describe = () => {
    summaryNote.textContent = chosen.size ? tn(chosen.size, '1 chosen', '{n} chosen') : t('Automatic');
  };
  describe();
  const fold = el('details', { class: 'fold answer-sources', open: openSourceChoices.has(row.resource) },
    el('summary', {}, t('Sources'), summaryNote));
  fold.ontoggle = () => { if (fold.open) openSourceChoices.add(row.resource); else openSourceChoices.delete(row.resource); };
  if (!usable.length) {
    fold.append(el('p', { class: 'hint', textContent: t('No sources are ready. In Knowledge, enable one, authorize its external processing and save.') }));
    return fold;
  }
  fold.append(el('p', { class: 'hint', textContent: t('Choose what the assistant reads for this answer. With none chosen, it uses the sources authorized for this chat.') }));
  const list = el('div', { class: 'choices' });
  for (const resource of usable) {
    const box = el('input', { type: 'checkbox', value: resource.id, checked: chosen.has(resource.id) });
    box.onchange = () => {
      if (box.checked) chosen.add(resource.id); else chosen.delete(resource.id);
      answerSources.set(row.resource, chosen);
      describe();
    };
    list.append(el('label', { class: 'check' }, box,
      el('span', {}, el('strong', { textContent: resource.id }),
        el('small', { textContent: `${kindName(resource)} · ${resource.description}${authorized(resource) ? ` · ${t('authorized for this chat')}` : ''}` }))));
  }
  fold.append(list);
  return fold;
}

/** Generate the answer to an unanswered message, or once it exists: regenerate it with context, or send it. */
function answerActions(row) {
  const drafted = unsentDraft(row);
  const sources = sourceChooser(row);
  const context = drafted ? el('textarea', { class: 'answer-context', rows: 3, maxLength: 4000,
    placeholder: t('E.g. It is already in validation and Luis Felipe owns it; the production deployment is planned for 10/20 and the production variables are still missing.'),
    value: answerContext.get(row.resource) || '' }) : null;
  if (context) {
    context.setAttribute('data-resource', row.resource);
    context.oninput = () => answerContext.set(row.resource, context.value);
  }
  const generate = el('button', { class: drafted ? 'secondary' : '', type: 'button', textContent: drafted ? t('Regenerate answer') : t('Generate answer') });
  generate.onclick = () => busy(generate, async () => {
    const chosen = [...sources.querySelectorAll('input:checked')].map(box => box.value);
    await invoke('answer_refine', { resource: row.resource, context: context?.value ?? '', sources: chosen });
    answerContext.delete(row.resource);
    answerSources.delete(row.resource);
    notify(t('Writing the answer…'), 'info');
    await followRefinement(row.resource);
  });
  if (!drafted) {
    return el('div', { class: 'field-block draft-actions' },
      el('strong', { textContent: t('Answer this message') }),
      el('p', { class: 'hint', textContent: t('The assistant reads the conversation and the sources and writes an answer for you to review. Nothing is sent until you send it.') }),
      sources,
      el('div', { class: 'actions' }, generate));
  }
  const send = el('button', { type: 'button', textContent: t('Send answer') });
  send.onclick = () => {
    // Sending is final: the first tap asks, the second one sends.
    if (sendArmed !== row.resource) {
      sendArmed = row.resource;
      send.textContent = t('Tap again to send');
      setTimeout(() => {
        if (sendArmed === row.resource) { sendArmed = null; send.textContent = t('Send answer'); }
      }, 4000);
      return;
    }
    sendArmed = null;
    send.textContent = t('Send answer');
    busy(send, async () => {
      const result = await invoke('answer_send', { resource: row.resource });
      if (result.status === 'sent') done(t('Answer sent to Teams.'));
      else notify(t('It is unknown whether the send arrived: check it in Teams (never retried).'), 'error');
      await loadMessages();
    });
  };
  return el('div', { class: 'field-block draft-actions' },
    el('strong', { textContent: t('Review and send') }),
    el('label', {}, t('Context for the assistant'), context),
    sources,
    el('div', { class: 'actions' }, generate, send),
    el('p', { class: 'hint', textContent: t('Regenerating writes the answer again with your context and the chosen sources, without sending it. Sending posts the answer shown above once, as you.') }));
}

/** An answer exists (sent or proposed) and the message is not being processed. */
const rateable = row => !!(row.audit.sent || row.audit.proposed)?.trim()
  && !['pending', 'processing', 'refining', 'sending'].includes(row.status);
/** Ratings being edited, kept across refreshes: resource → { rating, comment, example, context }. */
const feedbackDrafts = new Map();

/** Rate an answer, keep it as an example for similar requests, or save it as a verified note. */
function feedbackBlock(row) {
  const saved = row.audit.feedback ?? {};
  const draft = feedbackDrafts.get(row.resource) ?? { rating: saved.rating ?? null, comment: saved.comment ?? '', example: !!saved.example, context: saved.context ?? '' };
  const remember = () => feedbackDrafts.set(row.resource, draft);
  const ratings = el('div', { class: 'segmented', role: 'group', 'aria-label': t('Rate this answer') });
  const example = el('input', { type: 'checkbox', checked: draft.example });
  const exampleRow = el('label', { class: 'check' }, example, t('Use it as an example for similar requests'));
  const contexts = current?.map?.contexts ?? [];
  const context = el('select', { 'aria-label': t('Context of the example') },
    el('option', { value: '', textContent: t('Every context') }),
    ...contexts.map(c => el('option', { value: c.id, textContent: c.name || c.id, selected: draft.context === c.id })));
  const exampleFields = el('div', { class: 'example-fields' }, exampleRow, ...(contexts.length ? [context] : []),
    el('p', { class: 'hint', textContent: t('An example guides the structure and tone of later answers in any chat, never their facts. Do not choose answers with private details.') }));
  const sync = () => {
    for (const button of ratings.querySelectorAll('button')) button.setAttribute('aria-pressed', String(button.dataset.rating === draft.rating));
    exampleFields.hidden = !!row.audit.identified_question || draft.rating !== 'good';
  };
  for (const [rating, label] of [['good', t('Good answer')], ['bad', t('Needs work')]]) {
    const button = el('button', { type: 'button', textContent: label });
    button.dataset.rating = rating;
    button.onclick = () => { draft.rating = draft.rating === rating ? null : rating; if (draft.rating !== 'good') draft.example = false; example.checked = draft.example; remember(); sync(); };
    ratings.append(button);
  }
  example.onchange = () => { draft.example = example.checked; remember(); };
  context.onchange = () => { draft.context = context.value; remember(); };
  const comment = el('textarea', { class: 'feedback-comment', rows: 2, maxLength: 2000, value: draft.comment,
    placeholder: t('What was right or what should change') });
  comment.setAttribute('data-resource', row.resource);
  comment.oninput = () => { draft.comment = comment.value; remember(); };
  const save = el('button', { type: 'button', class: 'secondary', textContent: t('Save rating') });
  save.onclick = () => busy(save, async () => {
    await invoke('answer_feedback', { resource: row.resource, rating: draft.rating, comment: draft.comment.trim(), example: draft.example, context: draft.context || null });
    feedbackDrafts.delete(row.resource);
    done(draft.rating ? t('Rating saved.') : t('Rating cleared.'));
    if (!row.resource.startsWith('simulation:')) await loadMessages();
  });
  const asNote = el('button', { type: 'button', class: 'secondary', textContent: t('Save as a note') });
  asNote.onclick = () => noteFromAnswer(row);
  sync();
  return el('div', { class: 'field-block feedback' },
    el('strong', { textContent: t('Your rating') }),
    ratings, exampleFields,
    ...(row.audit.identified_question ? [el('p', { class: 'hint', textContent: t('Good ratings suggest this approach. Bad ratings tell the assistant what to avoid for this question. Current facts always come from the selected sources.') })] : []),
    el('label', {}, t('Comment'), comment),
    el('div', { class: 'actions' }, asNote, save));
}

/** Opens Knowledge › Notes with the answer as a new verified note. */
function noteFromAnswer(row) {
  const answer = (row.audit.sent || row.audit.proposed || '').replace(/\[([^\]\n]+)\]\([^)\s]*\)/g, '$1');
  showTab('knowledge');
  selectTab($('#knowledge-tabs'), 'notes');
  $('#note-form').open = true;
  noteOrigin = { origin: 'answer', reference: row.resource };
  field('#note-title', (row.audit.resolved_question || row.audit.question || '').slice(0, 200));
  field('#note-text', answer);
  $('#note-title').focus();
}

/** Opens the profile of the person who wrote, or a new one with their name and Teams ID. */
function personLink(row) {
  const known = (current?.map?.people ?? []).find(p => p.id === row.audit.person);
  const button = el('button', { type: 'button', class: 'link-button', textContent: known
    ? t('How you address {name}', { name: known.name }) : t('Add {name} to People', { name: row.audit.sender_name }) });
  button.onclick = () => {
    showTab('knowledge');
    selectTab($('#knowledge-tabs'), 'people');
    if (known) { openPeople.add(known.id); renderPeople(); return; }
    $('#person-form').open = true;
    field('#person-name', row.audit.sender_name);
    field('#person-sender', row.audit.sender_id ?? '');
    $('#person-address').focus();
  };
  return el('div', { class: 'field-block' }, button);
}

/** Follows an answer being written until it is ready, kept or failed. */
async function followRefinement(resource) {
  const deadline = Date.now() + 25 * 60_000;
  while (Date.now() < deadline) {
    await sleep(3000);
    await loadMessages();
    const row = messageRows.find(r => r.resource === resource);
    if (row?.status === 'refining') continue;
    if (row?.audit.reason === 'draft_ready') done(t('Answer ready: review it and send it from here.'));
    else if (row?.audit.reason === 'refinement_failed') fail(reasonText(row.audit.reason, row.audit));
    else if (row) notify(reasonText(row.audit.reason, row.audit), 'info');
    return;
  }
}

function renderMessages() {
  const list = $('#messages-list');
  // A refresh rebuilds the list: the context being typed keeps its focus and cursor.
  const typing = ['answer-context', 'feedback-comment'].some(name => document.activeElement?.classList?.contains(name)) ? document.activeElement : null;
  const typingAt = typing ? [typing.dataset.resource, typing.selectionStart, typing.selectionEnd] : null;
  const hideIneligible = $('#messages-hide-ineligible').checked;
  const query = $('#messages-search').value;
  const visible = messageRows.filter(row => !(hideIneligible && ineligible(row)) && matchesSearch(row, query));
  const rows = visible.filter(row => matchesFilter(row, messageFilter));
  // Each filter shows how many of the visible messages it holds.
  for (const button of document.querySelectorAll('#messages-filters button')) {
    const count = visible.filter(row => matchesFilter(row, button.dataset.filter)).length;
    button.querySelector('.count').textContent = count ? String(count) : '';
  }
  $('#messages-summary').textContent = rows.length === messageRows.length
    ? t('{n} recent messages', { n: messageRows.length })
    : t('Showing {shown} of {total} recent messages', { shown: rows.length, total: messageRows.length });
  list.replaceChildren();
  if (!rows.length) {
    const searching = Boolean(query.trim());
    const clear = searching ? el('button', { type: 'button', class: 'secondary small', textContent: t('Clear search') }) : null;
    if (clear) clear.onclick = () => { field('#messages-search', ''); renderMessages(); $('#messages-search').focus(); };
    list.append(el('div', { class: 'empty' }, el('p', { textContent: searching ? t('No messages match this search.')
      : messageRows.length ? t('No messages for this filter.') : t('No messages yet. They show up here when the assistant receives one from Teams.') }), clear));
    return;
  }
  for (const row of rows) {
    const audit = row.audit;
    const [label, tone] = messageState(row);
    const item = el('details', { class: 'message', open: openMessages.has(row.resource) });
    item.dataset.resource = row.resource;
    item.ontoggle = () => { if (item.open) openMessages.add(row.resource); else openMessages.delete(row.resource); };
    item.append(el('summary', {}, badge(label, tone), el('span', { class: 'kind', textContent: kindOf(row) }),
      el('time', { textContent: whenShort(row), title: when(row) }), el('span', { class: 'excerpt', textContent: excerpt(row) })));
    const body = el('div', { class: 'message-body' });
    messageField(body, t('Message'), audit.question, true);
    if (audit.resolved_question) messageField(body, t('Interpreted with the context'), `${audit.resolved_question}${audit.topic ? ` · ${t('topic searched')}: ${audit.topic}` : ''}`);
    messageField(body, t('Identified question'), audit.identified_question ? t(audit.identified_question.text) : t('No question identified'));
    if (audit.available_sources?.length) messageField(body, t('Available sources'), audit.available_sources.map(sourceTitle).join(', '));
    if (audit.selected_sources?.length) messageField(body, t('Sources selected to answer'), audit.selected_sources.map(sourceTitle).join(', '));
    if (!audit.identified_question && ['no_matching_question', 'question_matching_unavailable'].includes(audit.reason)) body.append(questionAssociation(row));
    messageField(body, t('Outcome'), reasonText(audit.reason, audit));
    if (row.status === 'sent') messageField(body, t('Answer sent'), audit.sent || audit.proposed, 'markdown');
    else messageField(body, row.status === 'dry_run' ? t('Proposal (observation mode)') : t('Proposed answer (not sent)'), audit.proposed, 'markdown');
    referencesField(body, audit);
    if (audit.refinement) messageField(body, t('Your context for the assistant'), audit.refinement, true);
    if (row.status === 'refining') {
      messageField(body, t('Generating'), audit.proposed?.trim()
        ? t('The assistant is writing the answer again with your context; it is not sent until you send it.')
        : t('The assistant is reading the conversation and writing an answer; it is not sent until you send it.'));
    }
    if (unsent(row)) body.append(answerActions(row));
    if (rateable(row)) body.append(feedbackBlock(row));
    if (audit.sender_name && audit.conversation_kind !== 'self') body.append(personLink(row));
    const details = [];
    if (audit.intent) details.push(`${t('Intent')}: ${intentLabels[audit.intent] || audit.intent}`);
    if (audit.history_messages) details.push(t('Context: {n} earlier messages', { n: audit.history_messages }));
    if (audit.provider) details.push(`${t('Model')}: ${audit.provider}`);
    if (audit.provider_fallbacks?.length) details.push(`${t('Failed first')}: ${audit.provider_fallbacks.join(', ')}`);
    if (audit.source) details.push(`${t('Sources')}: ${audit.source}`);
    if (audit.references?.length) details.push(`${t('References: {cited} cited of {total}', { cited: audit.used_sources?.length ?? 0, total: audit.references.length })}${audit.reference_selection === 'code' ? ` ${t('(chosen by code; no model chose them)')}` : ''}`);
    if (audit.final_check) details.push(`${t('Final review (earlier version)')}: ${audit.final_check === 'unavailable' ? t('unavailable') : audit.final_check}`);
    const notice = { sending: t('sending'), sent: t('sent'), uncertain: t('uncertain (never retried)') };
    if (audit.holding_reply) details.push(`${t('Holding notice')}: ${notice[audit.holding_reply] || audit.holding_reply}`);
    if (audit.withheld_notice) details.push(`${t('Withheld-answer notice')}: ${notice[audit.withheld_notice] || audit.withheld_notice}`);
    for (const link of audit.links || []) {
      const target = link.created_title
        ? t('new task {id}«{title}» under #{parent} {parentTitle}', { id: link.created ? `#${link.created} ` : '', title: link.created_title, parent: link.work_item, parentTitle: link.title })
        : `#${link.work_item} ${link.title}`;
      details.push(`${t('Registered')}: ${link.activity} → ${target}: ${linkLabels[link.status] || link.status}`);
    }
    messageField(body, t('Details'), details.join('\n'), true);
    if (audit.trace?.length) {
      const log = el('ol', { class: 'trace' });
      for (const step of audit.trace) {
        log.append(el('li', {}, el('time', { textContent: new Date(step.at).toLocaleTimeString(uiLocale()) }), el('b', { textContent: step.step }),
          step.detail ? ` — ${step.detail}` : ''));
      }
      body.append(el('div', { class: 'field-block' }, el('strong', { textContent: t('Log') }), log));
    } else {
      messageField(body, t('Log'), t('This message was processed by an earlier version, without a step log.'));
    }
    body.append(el('small', { class: 'resource-id', textContent: row.resource }));
    item.append(body);
    list.append(item);
  }
  const restored = typingAt && [...list.querySelectorAll(`.${typing.classList.contains('feedback-comment') ? 'feedback-comment' : 'answer-context'}`)].find(node => node.dataset.resource === typingAt[0]);
  if (restored) { restored.focus(); restored.setSelectionRange(typingAt[1], typingAt[2]); }
  // A message opened from Home: show it once it is in the list.
  const focused = focusMessage && [...list.querySelectorAll('.message')].find(node => node.dataset.resource === focusMessage);
  if (focused) {
    focusMessage = null;
    focused.scrollIntoView({ block: 'start' });
    focused.querySelector('summary').focus({ preventScroll: true });
  }
}

/** Opens one message in Messages, expanded, whatever filter or search was active. */
function openMessage(resource) {
  focusMessage = resource;
  openMessages.add(resource);
  field('#messages-search', '');
  setFilter('all');
  showTab('messages');
}

function renderAttention() {
  const since = Date.now() - 24 * 3600_000;
  const errors = messageRows.filter(row => messageState(row)[1] === 'error' && millis(row) >= since).length;
  const counter = $('#nav-attention');
  counter.hidden = !errors;
  counter.textContent = errors;
  counter.title = t('{n} message(s) with an error or an uncertain send in the last 24 h', { n: errors });
}

async function loadMessages() {
  messageRows = await invoke('audit', { limit: 100, resource: null, content: true });
  renderMessages();
  renderAttention();
}

async function loadActivity() {
  try { messageRows = await invoke('audit', { limit: 100, resource: null, content: true }); }
  catch { messageRows = []; }
  renderAttention();
  const relevant = messageRows.filter(row => !ineligible(row));
  const tally = (filter) => relevant.filter(row => matchesFilter(row, filter)).length;
  const stats = [[t('Answered'), tally('sent'), 'ok', 'sent'], [t('Withheld or unanswered'), tally('withheld'), 'warn', 'withheld'],
    [t('Errors'), tally('error'), 'error', 'error'], [t('In progress'), tally('active'), 'active', 'active']];
  // Every outcome keeps its tile; only those with messages take their color.
  $('#activity-stats').replaceChildren(...stats.map(([label, n, tone, filter]) => {
    const stat = el('button', { type: 'button', class: `stat ${n ? tone : 'muted'}`, title: t('See in Messages') },
      el('strong', { textContent: String(n) }), el('span', { textContent: label }));
    stat.onclick = () => { setFilter(filter); showTab('messages'); };
    return stat;
  }));
  const recent = relevant.slice(0, 6);
  $('#activity-list').replaceChildren(...(recent.length ? recent.map(row => {
    const [label, tone] = messageState(row);
    const open = el('button', { type: 'button', class: 'activity-row', title: t('Open in Messages') }, badge(label, tone),
      el('time', { textContent: ago(millis(row)), title: when(row) }), el('span', { class: 'excerpt', textContent: excerpt(row) }));
    open.onclick = () => openMessage(row.resource);
    return open;
  }) : [el('div', { class: 'empty' }, el('p', { textContent: t('No messages addressed to the assistant yet.') }))]));
}

function setFilter(filter) {
  messageFilter = filter;
  for (const button of document.querySelectorAll('#messages-filters button')) {
    button.setAttribute('aria-pressed', String(button.dataset.filter === filter));
  }
  renderMessages();
}

// ---------------------------------------------------------------- knowledge

const toolNames = {
  azure_devops_wiki: 'Azure DevOps Wiki', azure_devops_status: t('Azure DevOps activity'), azure_devops: t('Azure DevOps work items'),
  sql_server: 'SQL Server', rabbitmq: t('RabbitMQ queue'), http: 'HTTP GET', teams_messages: t('My Teams messages'),
  activity_record: t('Activity record'),
};

/** The activity record as a new source: disabled, without audiences or external processing. */
function recordSource() {
  return { id: 'activity-record', description: t('Existing Azure tasks saved by day, with their HU and hours.'),
    topics: [], enabled: false, external_processing: false, allowed_conversations: [], allowed_senders: [],
    kind: 'tool', tool: { type: 'activity_record' } };
}

/** An inline Azure DevOps catalog as its organizations' names. */
const catalogText = catalog => catalog.map(c => c.organization.replace(/^https:\/\/dev\.azure\.com\//, '').replace(/\/$/, '')).join(', ');

function kindName(resource) {
  if (resource.kind === 'file') return t('File');
  if (resource.kind === 'url' || resource.kind === 'urls') return 'URL';
  if (resource.kind === 'repository') return t('Repository');
  if (resource.kind === 'notes') return t('Notes');
  return toolNames[resource.tool?.type] || resource.tool?.type || t('Tool');
}

/** What the knowledge index holds per source (`index_status`), loaded with the Knowledge section. */
let indexStatus = { sources: [] };
const indexOf = resource => indexStatus.sources.find(s => s.id === resource.id)?.index;

function indexText(resource) {
  const held = indexOf(resource);
  if (resource.kind === 'notes') return tn(indexStatus.notes?.documents ?? 0, '1 note', '{n} notes');
  if (!held?.synced_at) return t('Not indexed yet: it is indexed the first time an answer reads it');
  return t('{files} files, {sections} sections indexed {when}', { files: held.documents, sections: held.sections, when: ago(held.synced_at * 1000) });
}

function accessText(resource) {
  if (resource.kind === 'file') return `${resource.repository} / ${resource.path}`;
  if (resource.kind === 'url') return resource.url;
  if (resource.kind === 'urls') return resource.urls.join(' · ');
  if (resource.kind === 'repository') return `${t('Repository')}: ${resource.repository} · ${indexText(resource)}`;
  if (resource.kind === 'notes') return resource.contexts?.length
    ? t('Your verified notes of {contexts} and the general ones', { contexts: resource.contexts.join(', ') })
    : `${t('Your verified notes')} · ${indexText(resource)}`;
  const tool = resource.tool ?? {};
  if (tool.page_urls) return tool.page_urls.join(' · ');
  if (tool.organization) return `${tool.organization} / ${tool.project}`;
  if (tool.repository) return `${t('Catalog')}: ${tool.repository} / ${tool.path}`;
  if (tool.catalog?.length) return `${t('Catalog')}: ${catalogText(tool.catalog)}`;
  if (tool.type === 'activity_record') return t('Daily task record, read on this device');
  if (tool.url) return tool.url;
  if (tool.operation) return `${t('Fixed query')}: ${tool.operation}`;
  if (tool.type === 'teams_messages') return t('Your own messages, read with the connected Microsoft account');
  return t('Read-only tool');
}

function sourceState(resource) {
  if (!resource.enabled) return badge(t('Disabled'), 'muted');
  if (!resource.external_processing) return badge(t('No external processing'), 'warn');
  if (!resource.allowed_conversations.length) return badge(t('No Teams audience'), 'warn');
  return badge(t('Ready'), 'ok');
}

/** Sources whose edit fold is open, kept across re-renders. */
const openSources = new Set();

function renderResources() {
  const container = $('#resources');
  container.replaceChildren();
  const resources = current.map.resources;
  const wikiChoice = $('#url-source-wiki');
  const wikiValue = wikiChoice.value;
  wikiChoice.replaceChildren(el('option', { value: '', textContent: t('Public pages (no sign-in)') }), ...resources.filter(r => r.tool?.type === 'azure_devops_wiki' && !r.tool.page_urls).map(r => el('option', { value: r.id, textContent: r.id })));
  if ([...wikiChoice.options].some(o => o.value === wikiValue)) wikiChoice.value = wikiValue;
  $('#sources-summary').textContent = resources.length ? t('{ready} of {total} ready for Teams', { ready: readySources(current.map).length, total: resources.length }) : '';
  if (!resources.length) {
    container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No sources yet. Add a repository or a file from one, or register the Wiki with pta sources add.') })));
  }
  $('#record-offer').hidden = resources.some(r => r.tool?.type === 'activity_record');
  for (const resource of resources) {
    let state = sourceState(resource);
    const facts = el('p', { class: 'resource-facts' });
    // The row reads as a summary; the text fields live in the fold below and keep it current.
    const renderFacts = () => facts.replaceChildren(
      el('span', { textContent: resource.description || t('No description') }),
      el('small', { textContent: [accessText(resource),
        `${t('Topics')}: ${resource.topics.join(', ') || '—'}`,
        `${t('Audience')}: ${resource.allowed_conversations.join(', ') || t('none')}`].join(' · ') }));
    renderFacts();
    const input = (label, value, placeholder, onchange) => {
      const element = el('input', { value, placeholder, spellcheck: false });
      element.oninput = () => { onchange(element.value); renderFacts(); markDirty(); };
      return el('label', {}, label, element);
    };
    const toggles = el('div', { class: 'resource-toggles' });
    for (const [key, text] of [['enabled', t('Enabled')], ['external_processing', t('Authorize external processing')]]) {
      const box = el('input', { type: 'checkbox', checked: resource[key] });
      box.onchange = () => { resource[key] = box.checked; markDirty(); renderResources(); };
      toggles.append(el('label', { class: 'check' }, box, text));
    }
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove source') });
    remove.onclick = () => {
      if (current.map.questions?.some(q => q.sources.includes(resource.id))) return fail(t('This source belongs to a question. Change or remove that question first.'));
      current.map.resources = current.map.resources.filter(r => r !== resource);
      for (const context of current.map.contexts ?? []) context.sources = context.sources.filter(id => id !== resource.id);
      markDirty(); renderResources(); renderContexts();
    };
    const actions = [remove];
    if (resource.kind === 'repository') {
      const refresh = el('button', { type: 'button', class: 'secondary small', textContent: t('Refresh index') });
      refresh.onclick = () => busy(refresh, async () => {
        const [result] = await invoke('index_refresh', { source: resource.id });
        await loadIndexStatus();
        const sync = result?.sync ?? {};
        done(t('{id} indexed: {added} new, {updated} changed, {removed} removed files.', { id: resource.id, added: sync.added ?? 0, updated: sync.updated ?? 0, removed: sync.removed ?? 0 }));
      });
      actions.unshift(refresh);
    }
    const edit = el('details', { class: 'fold resource-edit', open: openSources.has(resource.id) },
      el('summary', { textContent: t('Edit description, topics and audience') }),
      el('div', { class: 'grid' },
        input(t('Description'), resource.description, t('Which questions it answers'), value => { resource.description = value; }),
        input(t('Topics'), resource.topics.join(', '), t('Comma separated'), value => { resource.topics = names(value); }),
        input(t('Authorized conversations'), resource.allowed_conversations.join(', '), t('Comma-separated chat IDs, or *'), value => {
          resource.allowed_conversations = names(value);
          const next = sourceState(resource);
          state.replaceWith(next);
          state = next;
        })),
      el('div', { class: 'actions' }, ...actions));
    if (resource.kind === 'urls' || Array.isArray(resource.tool?.page_urls)) {
      const urls = el('textarea', { rows: 4, value: (resource.urls ?? resource.tool.page_urls).join('\n'), spellcheck: false });
      urls.oninput = () => { if (resource.kind === 'urls') resource.urls = lines(urls.value); else resource.tool.page_urls = lines(urls.value); renderFacts(); markDirty({ target: urls }); };
      edit.insertBefore(el('label', {}, t('URLs, one per line'), urls), edit.lastElementChild);
    }
    edit.ontoggle = () => { if (edit.open) openSources.add(resource.id); else openSources.delete(resource.id); };
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { textContent: resource.id }), badge(kindName(resource), 'active'), el('span', { class: 'grow' }), state),
      facts, toggles, edit));
  }
}

function renderRepositories() {
  const container = $('#repositories');
  container.replaceChildren();
  const choices = $('#source-repo');
  choices.replaceChildren();
  const entries = Object.entries(current.map.repositories);
  if (!entries.length) container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No repositories. Add a local checkout or clone one from GitHub.') })));
  for (const [alias, path] of entries) {
    choices.append(new Option(alias, alias));
    const input = el('code', { textContent: path });
    const choose = el('button', { type: 'button', class: 'secondary', textContent: t('Choose directory'), 'aria-label': t('Choose directory for {alias}', { alias }) });
    choose.onclick = () => chooseDirectories({ trigger: choose, title: t('Path of {alias}', { alias }), selected: [path], multiple: false,
      browse: path => invoke('machine_browse', { path }), apply: paths => { current.map.repositories[alias] = paths[0]; input.textContent = paths[0]; markDirty({ target: choose }); } });
    const update = el('button', { class: 'secondary', textContent: t('Update') });
    update.onclick = () => busy(update, async () => { await invoke('update_github_repository', { alias }); done(t('{alias} updated.', { alias })); });
    const remove = el('button', { class: 'danger', textContent: t('Remove') });
    remove.onclick = () => {
      const tools = current.map.resources.filter(r => r.kind === 'tool' && r.tool?.repository === alias);
      if (tools.length) return fail(t('{alias} cannot be removed: {ids} use it. Remove those sources first.', { alias, ids: tools.map(r => r.id).join(', ') }));
      const files = current.map.resources.filter(r => r.repository === alias);
      if (current.map.questions?.some(q => files.some(r => q.sources.includes(r.id)))) return fail(t('This source belongs to a question. Change or remove that question first.'));
      delete current.map.repositories[alias];
      current.map.resources = current.map.resources.filter(r => r.repository !== alias);
      markDirty();
      renderRepositories(); renderResources();
      if (files.length) notify(t('Its sources were removed too: {ids}.', { ids: files.map(r => r.id).join(', ') }));
    };
    const source = current.map.resources.find(r => r.kind === 'repository' && r.repository === alias);
    const asSource = el('button', { class: 'secondary', textContent: t('Use as a source') });
    asSource.onclick = () => { addRepositorySource(alias, t('Knowledge in the {alias} repository', { alias })); };
    container.append(el('div', { class: 'row' },
      rowText(alias, source ? t('Git checkout · source {id}', { id: source.id }) : t('Git checkout')), input, choose,
      ...(source ? [] : [asSource]), update, remove));
  }
}

/** A repository as a source of its own, disabled until the user authorizes it. */
function addRepositorySource(alias, description) {
  let id = alias.replace(/_/g, '-');
  for (let n = 2; current.map.resources.some(r => r.id === id); n++) id = `${alias.replace(/_/g, '-').slice(0, 60)}-${n}`;
  current.map.resources.push({ id, description, topics: [], enabled: false, external_processing: false,
    allowed_conversations: [], allowed_senders: [], kind: 'repository', repository: alias });
  openSources.add(id);
  markDirty({ target: $('#repositories') });
  renderRepositories(); renderResources();
  notify(t('Source {id} added, disabled. Enable it, authorize it, choose its conversations and save.', { id }));
}

// ---------------------------------------------------------------- contexts

/** Contexts whose edit fold is open, kept across re-renders. */
const openContexts = new Set();

function renderContexts() {
  const container = $('#contexts');
  container.replaceChildren();
  const contexts = current.map.contexts ??= [];
  $('#contexts-summary').textContent = contexts.length ? tn(contexts.length, '1 context', '{n} contexts') : '';
  if (!contexts.length) container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No contexts yet. Add one for each subject you answer about, with the words that name it.') })));
  for (const context of contexts) {
    const facts = el('p', { class: 'resource-facts' });
    const renderFacts = () => facts.replaceChildren(
      el('span', { textContent: context.description || t('No description') }),
      el('small', { textContent: [`${t('Keywords')}: ${context.keywords.join(', ') || '—'}`,
        `${t('Sources')}: ${context.sources.join(', ') || t('none')}`].join(' · ') }));
    renderFacts();
    const input = (label, value, placeholder, onchange) => {
      const element = el('input', { value, placeholder, spellcheck: false });
      element.oninput = () => { onchange(element.value); renderFacts(); markDirty(); };
      return el('label', {}, label, element);
    };
    const area = (label, value, placeholder, onchange) => {
      const element = el('textarea', { value, placeholder, rows: 3 });
      element.oninput = () => { onchange(element.value); renderFacts(); markDirty(); };
      return el('label', {}, label, element);
    };
    const sources = el('fieldset', { class: 'choices' }, el('legend', { textContent: t('Sources it reads') }));
    for (const resource of current.map.resources) {
      const box = el('input', { type: 'checkbox', value: resource.id, checked: context.sources.includes(resource.id) });
      box.onchange = () => {
        context.sources = box.checked ? [...context.sources, resource.id] : context.sources.filter(id => id !== resource.id);
        renderFacts(); markDirty();
      };
      sources.append(el('label', { class: 'check' }, box, el('span', {}, el('strong', { textContent: resource.id }), el('small', { textContent: kindName(resource) }))));
    }
    if (!current.map.resources.length) sources.append(el('p', { class: 'hint', textContent: t('Add sources first.') }));
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove context') });
    remove.onclick = () => { current.map.contexts = contexts.filter(c => c !== context); markDirty(); renderContexts(); renderNoteContexts(); };
    const edit = el('details', { class: 'fold resource-edit', open: openContexts.has(context.id) },
      el('summary', { textContent: t('Edit keywords, sources and instructions') }),
      el('div', { class: 'grid' },
        input(t('Name'), context.name, t('Billing'), value => { context.name = value; }),
        input(t('Keywords'), context.keywords.join(', '), t('Comma separated'), value => { context.keywords = names(value); })),
      input(t('Description'), context.description, t('What the subject covers'), value => { context.description = value; }),
      sources,
      area(t('Instructions for answers'), context.instructions, t('Terms to use, tone, what to always include'), value => { context.instructions = value; }),
      el('div', { class: 'actions' }, remove));
    edit.ontoggle = () => { if (edit.open) openContexts.add(context.id); else openContexts.delete(context.id); };
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: context.name || context.id }), badge(context.id, 'muted'), el('span', { class: 'grow' }),
        badge(tn(context.sources.length, '1 source', '{n} sources'), context.sources.length ? 'active' : 'warn')),
      facts, edit));
  }
}

// ---------------------------------------------------------------- questions

const openQuestions = new Set();
const newQuestionSources = new Set();
const sourceTitle = id => { const r = current?.map?.resources.find(r => r.id === id); return r ? `${r.id} · ${kindName(r)}` : id; };
function questionSourceChoices(selected, changed) {
  const choices = el('fieldset', { class: 'choices' }, el('legend', { textContent: t('Sources for this question') }));
  for (const source of current.map.resources) {
    const box = el('input', { type: 'checkbox', value: source.id, checked: selected.has(source.id) });
    box.onchange = () => { if (box.checked) selected.add(source.id); else selected.delete(source.id); changed([...selected]); };
    choices.append(el('label', { class: 'check' }, box, el('span', {}, el('strong', { textContent: source.id }),
      el('small', { textContent: `${kindName(source)}${!source.enabled || !source.external_processing ? ` · ${t('Unavailable until enabled and authorized')}` : ''}` }))));
  }
  return choices;
}
function renderQuestions() {
  const container = $('#questions');
  container.replaceChildren();
  const questions = current.map.questions ??= [];
  $('#questions-summary').textContent = tn(questions.length, '1 question', '{n} questions');
  const choices = questionSourceChoices(newQuestionSources, () => {});
  choices.id = 'question-sources';
  $('#question-sources').replaceWith(choices);
  if (!questions.length) container.append(el('p', { class: 'hint', textContent: t('No questions yet. Add one to let the assistant recognize equivalent requests.') }));
  for (const question of questions) {
    const text = el('input', { value: t(question.text), maxLength: 1000 });
    text.oninput = () => { question.text = text.value; markDirty({ target: container }); };
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove question') });
    remove.onclick = () => { current.map.questions = questions.filter(q => q.id !== question.id); markDirty({ target: container }); renderQuestions(); };
    const edit = el('details', { class: 'fold resource-edit', open: openQuestions.has(question.id) },
      el('summary', { textContent: t('Edit question') }), el('label', {}, t('Question'), text),
      questionSourceChoices(new Set(question.sources), sources => { question.sources = sources; markDirty({ target: container }); }),
      el('div', { class: 'field-block' }, el('strong', { textContent: t('Associated requests') }),
        ...(question.examples?.length ? [el('ul', {}, ...question.examples.map(example => el('li', { textContent: example })))] : [el('p', { class: 'hint', textContent: t('Requests associated during review appear here.') })])),
      el('div', { class: 'actions' }, remove));
    edit.ontoggle = () => { if (edit.open) openQuestions.add(question.id); else openQuestions.delete(question.id); };
    container.append(el('div', { class: 'resource' }, el('strong', { textContent: t(question.text) }),
      el('p', { class: 'resource-facts', textContent: question.sources.map(sourceTitle).join(', ') }), edit));
  }
}

function questionAssociation(row, associated = () => {}) {
  const select = el('select', {}, el('option', { value: '', textContent: t('Create a new question') }),
    ...(current.map.questions ?? []).map(q => el('option', { value: q.id, textContent: t(q.text) })));
  const text = el('input', { value: row.audit.question ?? '', maxLength: 1000 });
  const textLabel = el('label', {}, t('New question'), text);
  let selected = new Set();
  let choices = questionSourceChoices(selected, () => {});
  select.onchange = () => {
    textLabel.hidden = !!select.value;
    selected = new Set(current.map.questions?.find(q => q.id === select.value)?.sources ?? []);
    const next = questionSourceChoices(selected, () => {}); choices.replaceWith(next); choices = next;
  };
  const outcome = el('p', { class: 'hint', role: 'status' });
  const save = el('button', { type: 'button', class: 'secondary', textContent: t('Save association') });
  const form = el('details', { class: 'fold question-association' }, el('summary', { textContent: t('Associate question') }),
    el('label', {}, t('Existing question or new'), select), textLabel, choices,
    el('p', { class: 'hint', textContent: t('The message becomes an example of this question. Source audiences still apply. Saving does not send an answer.') }), save, outcome);
  save.onclick = () => busy(save, async () => {
    if (!selected.size || (!select.value && !text.value.trim())) throw t('Write a question and select at least one source.');
    const result = await applying(() => invoke('question_associate', { resource: row.resource,
      ...(select.value ? { question_id: select.value } : { text: text.value.trim() }), sources: [...selected] }));
    row.audit.identified_question = result.question;
    associated(result.question);
    outcome.textContent = t('Associated with: {question}', { question: t(result.question.text) });
    done(t('Question associated. You can review it in Questions.'));
    if (!row.resource.startsWith('simulation:')) await loadMessages();
  });
  return form;
}

// ---------------------------------------------------------------- people

/** People whose edit fold is open, kept across re-renders. */
const openPeople = new Set();

function renderPeople() {
  const container = $('#people');
  container.replaceChildren();
  const people = current.map.people ??= [];
  $('#people-summary').textContent = people.length ? tn(people.length, '1 person', '{n} people') : '';
  if (!people.length) container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No people yet. Add someone here, or from one of their messages in Messages.') })));
  for (const person of people) {
    const facts = el('p', { class: 'resource-facts' });
    const renderFacts = () => facts.replaceChildren(el('span', { textContent: person.address || t('No note on how to address them') }),
      ...(person.notes ? [el('small', { textContent: person.notes })] : []));
    renderFacts();
    const input = (label, value, placeholder, onchange, options = {}) => {
      const element = el('input', { value, placeholder, spellcheck: false, ...options });
      element.oninput = () => { onchange(element.value); renderFacts(); markDirty(); };
      return el('label', {}, label, element);
    };
    const area = (label, value, placeholder, onchange) => {
      const element = el('textarea', { value, placeholder, rows: 2 });
      element.oninput = () => { onchange(element.value); renderFacts(); markDirty(); };
      return el('label', {}, label, element);
    };
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove person') });
    remove.onclick = () => { current.map.people = people.filter(p => p !== person); markDirty(); renderPeople(); };
    const edit = el('details', { class: 'fold resource-edit', open: openPeople.has(person.id) },
      el('summary', { textContent: t('Edit how you address them') }),
      el('div', { class: 'grid' },
        input(t('Name'), person.name, 'Ana Pérez', value => { person.name = value; }),
        input(t('Other names'), (person.aliases ?? []).join(', '), t('Comma separated'), value => { person.aliases = names(value); })),
      area(t('How you address them'), person.address, t('Informal, by first name, two or three sentences'), value => { person.address = value; }),
      area(t('What to keep in mind'), person.notes, t('Their role, what they usually ask'), value => { person.notes = value; }),
      input(t('Teams user IDs'), (person.senders ?? []).join(', '), t('Comma separated'), value => { person.senders = names(value); }, { translate: 'no' }),
      el('div', { class: 'actions' }, remove));
    edit.ontoggle = () => { if (edit.open) openPeople.add(person.id); else openPeople.delete(person.id); };
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: person.name }), el('span', { class: 'grow' }),
        badge(person.senders?.length ? t('Teams ID known') : t('By name'), person.senders?.length ? 'ok' : 'muted')),
      facts, edit));
  }
}

// ---------------------------------------------------------------- notes

let notesList = [];
/** Where the note being added comes from, when it is an answer: { origin, reference }. */
let noteOrigin = null;
/** Notes whose edit fold is open, and the one whose removal waits for a second tap. */
const openNotes = new Set();
let removeArmed = null;

async function loadIndexStatus() {
  indexStatus = await invoke('index_status');
  renderResources();
}

async function loadNotes() {
  const query = $('#notes-query').value.trim();
  notesList = await invoke('notes_list', query ? { query, limit: 100 } : { limit: 200 });
  renderNotes();
}

/** The contexts a note can belong to, as checkboxes. */
function contextChoices(container, chosen, onchange) {
  container.querySelectorAll('label, p').forEach(node => node.remove());
  const contexts = current.map.contexts ?? [];
  if (!contexts.length) container.append(el('p', { class: 'hint', textContent: t('Without contexts, a note belongs to every subject.') }));
  for (const context of contexts) {
    const box = el('input', { type: 'checkbox', value: context.id, checked: chosen.includes(context.id) });
    box.onchange = () => onchange(box.checked ? [...chosen.filter(id => id !== context.id), context.id] : chosen.filter(id => id !== context.id));
    container.append(el('label', { class: 'check' }, box, el('span', { textContent: context.name || context.id })));
  }
}
let newNoteContexts = [];
function renderNoteContexts() {
  contextChoices($('#note-contexts'), newNoteContexts, value => { newNoteContexts = value; renderNoteContexts(); });
}

const originNames = { agent: t('From an agent'), answer: t('From an answer'), import: t('Imported') };

function renderNotes() {
  const container = $('#notes');
  container.replaceChildren();
  $('#notes-summary').textContent = tn(notesList.length, '1 note', '{n} notes');
  $('#notes-offer').hidden = current.map.resources.some(r => r.kind === 'notes');
  if (!notesList.length) {
    container.append(el('div', { class: 'empty' }, el('p', { textContent: $('#notes-query').value.trim()
      ? t('No note matches the search.') : t('No notes yet. Add what you verified, or ask an agent to add it with pta notes add.') })));
  }
  for (const note of notesList) {
    const draft = structuredClone(note);
    const title = el('input', { value: note.title });
    title.oninput = () => { draft.title = title.value; };
    const text = el('textarea', { value: note.text, rows: 5 });
    text.oninput = () => { draft.text = text.value; };
    const keywords = el('input', { value: note.keywords.join(', ') });
    keywords.oninput = () => { draft.keywords = names(keywords.value); };
    const contexts = el('fieldset', { class: 'choices' }, el('legend', { textContent: t('Contexts') }));
    const renderChoices = () => contextChoices(contexts, draft.contexts, value => { draft.contexts = value; renderChoices(); });
    renderChoices();
    const save = el('button', { type: 'button', class: 'secondary small', textContent: t('Save note') });
    save.onclick = () => busy(save, async () => {
      await invoke('notes_save', { note: draft });
      done(t('Note saved.'));
      await loadNotes();
    });
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove note') });
    remove.onclick = () => {
      if (removeArmed !== note.id) {
        removeArmed = note.id;
        remove.textContent = t('Tap again to remove');
        setTimeout(() => { if (removeArmed === note.id) { removeArmed = null; remove.textContent = t('Remove note'); } }, 4000);
        return;
      }
      removeArmed = null;
      busy(remove, async () => { await invoke('notes_remove', { id: note.id }); openNotes.delete(note.id); done(t('Note removed.')); await loadNotes(); });
    };
    const edit = el('details', { class: 'fold resource-edit', open: openNotes.has(note.id) },
      el('summary', { textContent: t('Edit note') }),
      el('label', {}, t('Title'), title), el('label', {}, t('Text'), text),
      el('label', {}, t('Keywords'), keywords), contexts,
      el('div', { class: 'actions' }, save, remove));
    edit.ontoggle = () => { if (edit.open) openNotes.add(note.id); else openNotes.delete(note.id); };
    const contextNames = note.contexts.map(id => (current.map.contexts ?? []).find(c => c.id === id)?.name || id);
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: note.title }), el('span', { class: 'grow' }),
        ...(originNames[note.origin] ? [badge(originNames[note.origin], 'info')] : []),
        ...(contextNames.length ? contextNames.map(name => badge(name, 'active')) : [badge(t('Every context'), 'muted')])),
      el('p', { class: 'resource-facts' },
        el('span', { textContent: note.text.length > 280 ? `${note.text.slice(0, 280)}…` : note.text }),
        el('small', { textContent: [note.keywords.length ? `${t('Keywords')}: ${note.keywords.join(', ')}` : '',
          t('Updated {when}', { when: ago(note.updated_at * 1000) })].filter(Boolean).join(' · ') })),
      edit));
  }
}

async function loadKnowledge() {
  await Promise.all([loadIndexStatus(), loadNotes()]);
}

async function listGithub() {
  const repos = await invoke('github_repositories');
  const container = $('#github-repositories');
  container.replaceChildren();
  if (!repos.length) { container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No repositories are installed for this GitHub App.') }))); return; }
  for (const repo of repos) {
    const alias = el('input', { spellcheck: false, 'aria-label': t('Local alias of {name}', { name: repo.full_name }),
      value: repo.full_name.split('/').pop().toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/^[^a-z]+/, 'repo-').slice(0, 64) });
    const clone = el('button', { class: 'secondary', textContent: t('Clone and add') });
    clone.onclick = () => busy(clone, () => applying(async () => {
      await invoke('clone_github_repository', { fullName: repo.full_name, alias: alias.value.trim() });
      done(t('{name} added.', { name: repo.full_name }));
    }));
    container.append(el('div', { class: 'row' }, rowText(repo.full_name, repo.private ? t('Private') : t('Public')), alias, clone));
  }
}

// ---------------------------------------------------------------- test chat

const chosen = new Set();

function renderChatSources() {
  const container = $('#chat-sources');
  container.replaceChildren();
  // The simulation reads the saved knowledge map, so offer what is persisted.
  const usable = live.map.resources.filter(r => r.enabled && r.external_processing);
  for (const id of [...chosen]) if (!usable.some(r => r.id === id)) chosen.delete(id);
  $('#chat-all').hidden = usable.length < 2;
  if (!usable.length) {
    container.append(el('p', { class: 'hint', textContent: t('No sources are ready. In Knowledge, enable one, authorize its external processing and save.') }));
    return;
  }
  for (const resource of usable) {
    const box = el('input', { type: 'checkbox', value: resource.id, checked: chosen.has(resource.id) });
    box.onchange = () => { if (box.checked) chosen.add(resource.id); else chosen.delete(resource.id); };
    container.append(el('label', { class: 'check' }, box,
      el('span', {}, el('strong', { textContent: resource.id }), el('small', { textContent: `${kindName(resource)} · ${resource.description}` }))));
  }
}

async function copy(text) {
  try { await navigator.clipboard.writeText(text); return true; } catch { /* fall back below */ }
  const area = el('textarea', { value: text, class: 'sr-only', readOnly: true });
  document.body.append(area);
  area.select();
  let copied = false;
  try { copied = document.execCommand('copy'); } catch { copied = false; }
  area.remove();
  return copied;
}

/** In the browser a link opens in a new tab; the app window never navigates, so there it copies the URL. */
function link(label, url) {
  if (window.ptaTransport.browser) return el('a', { class: 'ref', href: url, target: '_blank', rel: 'noopener noreferrer', textContent: label });
  const button = el('button', { type: 'button', class: 'ref', textContent: label, title: t('Copy link: {url}', { url }) });
  button.onclick = async () => {
    if (await copy(url)) done(t('Link copied: {url}', { url }));
    else notify(t('Link: {url}', { url }));
  };
  return button;
}

const INLINE = new RegExp([
  /\\([!-\/:-@[-`{-~])/, // escaped punctuation
  /`([^`]+)`/,
  /\*\*\*(\S(?:.*?\S)?)\*\*\*/,
  /\*\*(\S(?:.*?\S)?)\*\*|(?<!\w)__(\S(?:.*?\S)?)__(?!\w)/,
  /~~(\S(?:.*?\S)?)~~/,
  /\*([^\s*](?:[^*]*?[^\s*])?)\*|(?<!\w)_([^\s_](?:[^_]*?[^\s_])?)_(?!\w)/,
  /\{(red|orange|green|blue|gray):([^{}]*\S[^{}]*)\}/,
  /\[([^\]]+)\]\((https:\/\/[^\s)]+)\)/,
  /(https:\/\/[^\s<>"\u0000-\u001f]+)/,
].map(part => part.source).join('|'), 'g');

function inline(text, parent) {
  const wrap = (tag, body, props) => { const node = el(tag, props); inline(body, node); return node; };
  let last = 0;
  for (const match of text.matchAll(INLINE)) {
    if (match.index > last) appendLinkedText(text.slice(last, match.index), parent);
    const [, escaped, code, both, bold, underBold, strike, italic, underItalic, tone, toned, label, url, bare] = match;
    if (escaped !== undefined) parent.append(escaped);
    else if (code !== undefined) parent.append(el('code', { textContent: code }));
    else if (both !== undefined) parent.append(el('strong', {}, wrap('em', both)));
    else if ((bold ?? underBold) !== undefined) parent.append(wrap('strong', bold ?? underBold));
    else if (strike !== undefined) parent.append(wrap('s', strike));
    else if ((italic ?? underItalic) !== undefined) parent.append(wrap('em', italic ?? underItalic));
    else if (tone !== undefined) parent.append(wrap('span', toned.trim(), { class: `tone-${tone}` }));
    else if (bare !== undefined) appendLinkedText(bare, parent);
    else parent.append(todoSafeUrl(url) ? link(label, url) : label);
    last = match.index + match[0].length;
  }
  if (last < text.length) appendLinkedText(text.slice(last), parent);
}

const LIST_ITEM = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;
const FENCE = /^\s*```/;
const RULE = /^([-*_])(?:\s*\1){2,}\s*$/;
const HEADING = /^(#{1,6})\s+(\S.*)$/;
const QUOTE = /^\s*>\s?(.*)$/;

/** Cells of a pipe-table row: `|` separates them unless escaped or inside a code span; the outer pipes are optional. */
function cells(row) {
  const text = row.trim().replace(/^\|/, '');
  const out = [''];
  let code = false;
  for (let k = 0; k < text.length; k++) {
    if (text[k] === '\\' && text[k + 1] === '|') { out[out.length - 1] += '|'; k++; }
    else if (text[k] === '|' && !code) out.push('');
    else { code = code !== (text[k] === '`'); out[out.length - 1] += text[k]; }
  }
  if (text.endsWith('|') && out[out.length - 1] === '') out.pop();
  return out.map(cell => cell.trim());
}
const isTable = (lines, i) => lines[i].includes('|') && Boolean(lines[i + 1]?.includes('|'))
  && cells(lines[i + 1]).length === cells(lines[i]).length && cells(lines[i + 1]).every(cell => /^:?-+:?$/.test(cell));
// Lines that open a block of their own end the paragraph or list before them.
const opensBlock = (lines, i, quotes) => FENCE.test(lines[i]) || RULE.test(lines[i]) || HEADING.test(lines[i])
  || (quotes && QUOTE.test(lines[i])) || isTable(lines, i);

function table(header, rows) {
  const row = (tag, values) => el('tr', {}, ...header.map((_, k) => { const cell = el(tag); inline(values[k] ?? '', cell); return cell; }));
  return el('div', { class: 'table' }, el('table', {}, el('thead', {}, row('th', header)), el('tbody', {}, ...rows.map(values => row('td', values)))));
}

/** Renders the Markdown the assistant writes (the same subset Teams receives as HTML), without innerHTML. Quotes are one level deep. */
function markdown(text, quotes = true) {
  const root = el('div', { class: 'md' });
  const source = text.replace(/\r/g, '').split('\n');
  let i = 0;
  while (i < source.length) {
    const line = source[i];
    if (FENCE.test(line)) {
      const code = [];
      i++;
      while (i < source.length && !/^\s*```\s*$/.test(source[i])) code.push(source[i++]);
      i++;
      root.append(el('pre', {}, el('code', { textContent: code.join('\n') })));
      continue;
    }
    if (!line.trim()) { i++; continue; }
    if (RULE.test(line)) { root.append(el('hr')); i++; continue; }
    const heading = line.match(HEADING);
    if (heading) {
      const node = el(heading[1].length <= 2 ? 'h2' : 'h3');
      inline(heading[2].trim(), node);
      root.append(node);
      i++;
      continue;
    }
    if (quotes && QUOTE.test(line)) {
      const quoted = [];
      while (i < source.length && QUOTE.test(source[i])) quoted.push(source[i++].match(QUOTE)[1]);
      root.append(el('blockquote', {}, markdown(quoted.join('\n'), false)));
      continue;
    }
    if (isTable(source, i)) {
      const header = cells(source[i]);
      const rows = [];
      i += 2;
      while (i < source.length && source[i].includes('|') && source[i].trim() && !FENCE.test(source[i])) rows.push(cells(source[i++]));
      root.append(table(header, rows));
      continue;
    }
    if (LIST_ITEM.test(line)) {
      const items = [];
      while (i < source.length && source[i].trim() && !opensBlock(source, i, quotes)) {
        const match = source[i].match(LIST_ITEM);
        if (match) items.push({ indent: match[1].length, ordered: /\d/.test(match[2]), start: parseInt(match[2], 10), text: match[3] });
        else if (items.length) items[items.length - 1].text += ` ${source[i].trim()}`;
        i++;
      }
      const stack = [{ indent: -1, list: null, item: null }];
      for (const item of items) {
        while (stack.length > 1 && item.indent < stack[stack.length - 1].indent) stack.pop();
        let top = stack[stack.length - 1];
        if (item.indent > top.indent) {
          const list = el(item.ordered ? 'ol' : 'ul');
          if (item.ordered && item.start > 1) list.start = item.start;
          (top.item ?? root).append(list);
          top = { indent: item.indent, list, item: null };
          stack.push(top);
        }
        const entry = el('li');
        const task = item.text.match(/^\[([ xX])\] (.*)$/);
        if (task) entry.append(task[1] === ' ' ? '☐ ' : '☑ ');
        inline(task ? task[2] : item.text, entry);
        top.list.append(entry);
        top.item = entry;
      }
      continue;
    }
    const paragraph = el('p');
    do {
      if (paragraph.childNodes.length) paragraph.append(el('br'));
      inline(source[i].trim(), paragraph);
      i++;
    } while (i < source.length && source[i].trim() && !LIST_ITEM.test(source[i]) && !opensBlock(source, i, quotes));
    root.append(paragraph);
  }
  return root;
}

function bubble(kind, ...content) {
  $('#chat-empty').hidden = true;
  const node = el('div', { class: `bubble ${kind}` }, ...content);
  $('#history').append(node);
  node.scrollIntoView({ block: 'end', behavior: 'smooth' });
  return node;
}

function answerBubble(result) {
  const row = { resource: result.resource, status: result.status, audit: { ...result, question: result.request, proposed: result.answer } };
  const question = el('p', { class: 'hint', textContent: result.identified_question ? `${t('Identified question')}: ${t(result.identified_question.text)}` : t('No question identified') });
  const sources = el('p', { class: 'hint', textContent: `${t('Available sources')}: ${(result.available_sources ?? []).map(sourceTitle).join(', ')}\n${t('Sources selected to answer')}: ${(result.selected_sources ?? []).map(sourceTitle).join(', ')}` });
  const review = result.resource && !result.identified_question && ['no_matching_question', 'question_matching_unavailable'].includes(result.reason) ? questionAssociation(row, q => { question.textContent = `${t('Identified question')}: ${t(q.text)}`; }) : null;
  if (!result.answer) return bubble('bot none', `${t('No answer')}: ${reasonText(result.reason) || result.status}`, question, sources, review);
  const meta = el('div', { class: 'meta' });
  for (const id of result.used_sources ?? []) meta.append(badge(id, 'active'));
  if (result.partial) meta.append(badge(t('Partial coverage'), 'warn'));
  for (const warning of result.warnings ?? []) meta.append(badge(warning, 'warn'));
  let refs = null;
  if (result.references?.length) {
    refs = el('div', { class: 'refs' }, el('strong', { textContent: t('Verified references') }),
      el('ul', {}, ...result.references.map(ref => el('li', {}, link(ref.label || ref.id, ref.url)))));
  }
  return bubble('bot', markdown(result.answer), question, sources, meta.childNodes.length ? meta : null, refs, result.resource ? feedbackBlock(row) : null);
}

function resizeComposer() {
  const area = $('#message');
  area.style.height = 'auto';
  area.style.height = `${Math.min(area.scrollHeight + 2, 192)}px`;
}

// ---------------------------------------------------------------- wiring

$('#machines-refresh').onclick = () => busy($('#machines-refresh'), loadMachines);
$('#machine-save').onclick = () => busy($('#machine-save'), async () => {
  await invoke('machine_save', { name: $('#machine-name').value, roots: machineRoots });
  await loadMachines(); done(t('Machine settings saved.'));
});
$('#machine-roots-choose').onclick = () => chooseDirectories({ trigger: $('#machine-roots-choose'), title: t('Allowed directory roots'), selected: machineRoots, allowEmpty: true,
  browse: path => invoke('machine_browse', { path }), apply: paths => { machineRoots = paths; renderMachineRoots(); } });
$('#repo-path-choose').onclick = () => chooseDirectories({ trigger: $('#repo-path-choose'), title: t('Checkout path'), selected: lines($('#repo-path').value), multiple: false,
  browse: path => invoke('machine_browse', { path }), apply: paths => { field('#repo-path', paths[0]); $('#repo-path-value').textContent = paths[0]; } });
// The modal dialog shows progress; loadMachines then decides whether Connect stays available.

/** Browser-assisted sign-in: the host keeps the verifier; this dialog only sees the public link and code.
 *  Resolves true once this computer is connected to the account at `url`. */
async function machineLogin(url) {
  const dialog = $('#machine-login-dialog'), status = $('#machine-login-status'), link = $('#machine-login-link');
  if (dialog.open) return false;
  let cancelled = false, connected = false;
  $('#machine-login-code').textContent = '';
  $('#machine-login-copy').disabled = true;
  link.hidden = true; link.removeAttribute('href');
  status.textContent = t('Starting sign-in…');
  dialog.showModal();
  const cancel = event => { event?.preventDefault(); cancelled = true; };
  dialog.oncancel = cancel;
  $('#machine-login-cancel').onclick = cancel;
  try {
    const login = await invoke('machine_login', { url, open: true });
    if (cancelled) return false;
    $('#machine-login-code').textContent = login.code;
    link.href = login.approval_url; link.hidden = false;
    link.onclick = event => { event.preventDefault(); invoke('machine_login', { url: login.coordinator, open: true }).catch(fail); };
    $('#machine-login-copy').disabled = false;
    $('#machine-login-copy').onclick = () => busy($('#machine-login-copy'), async () => {
      if (await copy(login.approval_url)) done(t('Link copied.'));
    });
    status.textContent = login.opened
      ? t('Waiting for you to trust this computer in the browser…')
      : t('Open the link on any device, sign in and choose Trust this machine.');
    link.focus();
    while (!cancelled && Date.now() < login.expires_at * 1000) {
      const result = await rawInvoke('command', { request: { method: 'machine_login_finish', args: {}, revision: null, contract: 1 } });
      if (cancelled) return false;
      if (result.ok) {
        connected = true;
        done(result.data.username ? t('This computer is connected to {account}.', { account: result.data.username }) : t('This computer is connected to your account.'));
        return true;
      }
      if (result.code !== 'authorization_pending') throw new Error(tx(result.message || result.code));
      await sleep(2000);
    }
    if (!cancelled) throw new Error(t('The sign-in expired. Nothing was trusted; select Sign in to try again.'));
    return false;
  } finally {
    if (!connected) await rawInvoke('command', { request: { method: 'machine_login_cancel', args: {}, revision: null, contract: 1 } }).catch(() => {});
    dialog.close();
    link.removeAttribute('href'); $('#machine-login-code').textContent = '';
    await loadMachines().catch(fail);
    if (connected) leaveAccount();
  }
}
$('#machine-disconnect').onclick = () => busy($('#machine-disconnect'), async () => {
  await invoke('machine_disconnect');
  leaveAccount();
  await loadMachines();
  await Promise.all([loadVariables(), loadAccountConfig(), loadReadiness()]);
});
$('#todo-provider').onchange = () => {
  $('#todo-state').replaceChildren(el('option', { value: '', textContent: t('All states') }));
  loadTodo();
};
$('#todo-source').onchange = () => {
  $('#todo-state').replaceChildren(el('option', { value: '', textContent: t('All states') }));
  loadTodo();
};
$('#todo-kind').onchange = () => {
  $('#todo-state').replaceChildren(el('option', { value: '', textContent: t('All states') }));
  loadTodo();
};
$('#todo-github-kind').onchange = () => loadTodo();
$('#todo-back').onclick = () => closeTodoDetail();
$('#todo-detail-refresh').onclick = () => loadTodoDetail();
$('#todo-state').onchange = () => loadTodo();
$('#todo-limit').onchange = () => loadTodo();
$('#todo-create').onclick = openRequest;
$('#todo-agent-history').onclick = () => showTab('agent-history');
$('#agent-history-back').onclick = () => showTab('todo');
$('#agent-history-refresh').onclick = () => busy($('#agent-history-refresh'), loadAgentHistory);
$('#todo-refresh').onclick = () => busy($('#todo-refresh'), loadTodo);

// After a section change, focus its heading so a screen reader starts there (no visible ring after a click).
document.querySelectorAll('section > .page-head h1').forEach(heading => { heading.tabIndex = -1; });
document.querySelectorAll('nav button[data-tab]').forEach(button => {
  button.onclick = () => { showTab(button.dataset.tab); $(`#${button.dataset.tab} h1`)?.focus({ preventScroll: true }); };
});
$('#nav-more').onclick = () => setNavOverflow($('#nav-more').getAttribute('aria-expanded') !== 'true');
document.addEventListener('click', event => { if (!event.target.closest('nav')) setNavOverflow(false); });
document.addEventListener('keydown', event => {
  if (event.key === 'Escape' && $('#nav-more').getAttribute('aria-expanded') === 'true') {
    setNavOverflow(false);
    $('#nav-more').focus();
  }
});
for (const list of document.querySelectorAll('[role=tablist]')) {
  const tabs = [...list.querySelectorAll('[role=tab]')];
  for (const button of tabs) {
    button.onclick = () => selectTab(list, button.dataset.group);
    // Arrow keys, Home and End move between tabs, as in any tab list.
    button.onkeydown = event => {
      const at = tabs.indexOf(button);
      const next = { ArrowRight: at + 1, ArrowLeft: at - 1, Home: 0, End: tabs.length - 1 }[event.key];
      if (next === undefined) return;
      event.preventDefault();
      selectTab(list, tabs[(next + tabs.length) % tabs.length].dataset.group, true);
    };
  }
}
document.querySelectorAll('.cfg').forEach(input => {
  input.addEventListener('input', markDirty);
  input.addEventListener('change', markDirty);
});
$('#discover-chats').addEventListener('change', renderChatList);
document.querySelectorAll('#theme-choice button').forEach(button => {
  button.onclick = () => {
    const choice = button.dataset.themeChoice;
    try { if (choice === 'system') localStorage.removeItem(THEME_KEY); else localStorage.setItem(THEME_KEY, choice); }
    catch { /* applies to this page only */ }
    applyTheme(choice);
  };
});
$('#registration-schedule').addEventListener('change', renderScheduleFields);
$('#notice-close').onclick = () => { $('#notice').hidden = true; };
$('#save').onclick = () => busy($('#save'), async () => { await save(); done(t('Settings saved.')); });
$('#discard').onclick = () => busy($('#discard'), async () => { await reload(); notify(t('Changes discarded.')); });
$('#start').onclick = () => startAssistant($('#start'));
$('#restart').onclick = () => busy($('#restart'), async () => {
  if (dirty) await save();
  await invoke('restart');
  await refreshLive();
  done(t('Assistant restarted with the saved settings.'));
});
$('#stop').onclick = () => busy($('#stop'), async () => { await invoke('stop_assistant'); await refreshLive(); done(t('Assistant stopped.')); });
$('#offer-start').onclick = () => busy($('#offer-start'), async () => {
  await invoke('start_assistant'); await refreshLive(); done(t('The assistant keeps running with the new version.'));
});
$('#offer-dismiss').onclick = () => busy($('#offer-dismiss'), async () => {
  await invoke('dismiss_restart_offer'); await refreshLive(); notify(t('The assistant stays stopped. You can start it whenever you want.'));
});
$('#activity-all').onclick = () => showTab('messages');
$('#activity-notice-open').onclick = () => busy($('#activity-notice-open'), () => invoke('activity_open'));
$('#activity-settings').onclick = () => showTab('settings', 'panel-registration');
$('#activity-refresh').onclick = () => busy($('#activity-refresh'), loadRegistration);
$('#activity-log-from').onchange = $('#activity-log-to').onchange = () => loadRegistration().catch(fail);
$('#activity-run').onclick = () => busy($('#activity-run'), async () => {
  if (dirty) await save();
  await invoke('activity_run', { day: $('#activity-day').value || null });
  activityRunning = true;
  await loadRegistration();
});
$('#activity-days-back').onclick = () => {
  ++activityDayRequest; ++activityDetailRequest;
  activityDay = null; activityTask = null;
  showActivityView('master');
  activityDayFocus?.focus({ preventScroll: true }); window.scrollTo(0, activityDaysScroll);
};
$('#activity-task-back').onclick = () => {
  if (backRelatedItem('activity')) return;
  ++activityDetailRequest; activityTask = null;
  showActivityView('day-view');
  activityTaskFocus?.focus({ preventScroll: true }); window.scrollTo(0, activityTasksScroll);
};
$('#activity-detail-refresh').onclick = () => loadActivityTask();
setInterval(() => { if (current && (tab === 'registration' || activityRunning)) loadRegistration().catch(() => {}); }, 5_000);

if (window.__TAURI__?.event?.listen) {
  // Pushed on every start/stop, including from the tray and `pta`.
  window.__TAURI__.event.listen('assistant-phase', event => {
    transition = transitions[event.payload] ? event.payload : null;
    // Once settled, render from a fresh snapshot rather than the one from before the change.
    if (transition || !current) renderLive();
    else refreshLive().catch(() => renderLive());
  });
  window.__TAURI__.event.listen('assistant-error', event => fail(event.payload));
  window.__TAURI__.event.listen('activity-registration', () => loadRegistration().catch(fail));
  window.__TAURI__.event.listen('activity-open', event => {
    showTab('registration');
  });
}
$('#messages-refresh').onclick = () => busy($('#messages-refresh'), loadMessages);
document.querySelectorAll('#messages-filters button').forEach(button => { button.onclick = () => setFilter(button.dataset.filter); });
$('#messages-hide-ineligible').onchange = renderMessages;
$('#messages-search').oninput = renderMessages;
setInterval(() => {
  // Not while context is typed for an answer: a rebuilt list would move it under the user.
  if (tab === 'messages' && $('#messages-auto').checked && !document.activeElement?.classList?.contains('answer-context')) loadMessages().catch(() => {});
}, 10_000);
$('#add-llm').onclick = () => {
  const info = llmCatalog.providers.find(provider => provider.id === $('#add-llm-provider').value);
  if (!info || llmRows.length >= 16) return;
  const model = info.models[0];
  if (info.id === 'opencode' && !model) return;
  llmRows.push({ provider: info.id, model: model?.id ?? '', effort: model?.default_effort ?? info.efforts[0], enabled: info.available });
  markDirty({ target: $('#llm-chain') });
  renderChain();
  $('#llm-chain li:last-child .field input, #llm-chain li:last-child .field select')?.focus();
};
$('#detect-llm').onclick = () => busy($('#detect-llm'), async () => {
  llmCatalog = await invoke('llm_providers'); renderChain(); done(t('CLIs detected again.'));
});
$('#teams-login').onclick = () => busy($('#teams-login'), () => applying(async () => {
  await invoke('connect_microsoft'); done(t('Microsoft account connected.'));
}));
$('#import').onclick = () => busy($('#import'), async () => {
  await invoke('import_existing', { path: $('#legacy-path').value.trim() }); await reload(); done(t('Settings imported.'));
});
$('#self-chat-enable').onclick = () => busy($('#self-chat-enable'), () => applying(async () => {
  await invoke('self_chat_enable', { id: $('#self-chat-id').value.trim() || null });
  done(t('Personal chat validated and enabled. Source audiences are kept.'));
}));
$('#self-chat-disable').onclick = () => busy($('#self-chat-disable'), () => applying(async () => {
  await invoke('self_chat_disable'); done(t('Personal chat disabled.'));
}));
$('#self-chat-test').onclick = () => busy($('#self-chat-test'), async () => {
  const result = await invoke('test_self_chat');
  if (result.account_scope_verified) done(t('Personal chat validated. Write a new question in Teams to check reception and the answer.'));
  else notify(t('Incomplete test.'));
});
$('#add-repo').onclick = () => {
  const alias = $('#repo-alias').value.trim();
  const path = $('#repo-path').value.trim();
  const description = $('#repo-description').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(alias) || !path) return fail(t('Give a valid alias (lowercase letters, digits, - or _) and the checkout path.'));
  if (current.map.repositories[alias]) return fail(t('That alias already exists.'));
  current.map.repositories[alias] = path;
  field('#repo-alias', ''); field('#repo-path', ''); $('#repo-path-value').textContent = ''; field('#repo-description', '');
  markDirty({ target: $('#add-repo') });
  if (description) addRepositorySource(alias, description);
  else renderRepositories();
};
$('#github-connect').onclick = () => busy($('#github-connect'), async () => {
  const prompt = await invoke('begin_github_login', { clientId: $('#github-client-id').value.trim() });
  $('#github-code').textContent = prompt.user_code;
  $('#github-device').hidden = false;
  notify(t('Code ready. Authorize the GitHub App with the repositories you want.'));
});
$('#github-open').onclick = () => {
  if (window.ptaTransport.browser) window.open('https://github.com/login/device', '_blank', 'noopener,noreferrer');
  else invoke('open_github_login').catch(fail);
};
$('#github-finish').onclick = () => busy($('#github-finish'), async () => {
  await invoke('finish_github_login');
  $('#github-device').hidden = true;
  await refreshLive();
  await listGithub();
  done(t('GitHub connected.'));
});
$('#github-list').onclick = () => busy($('#github-list'), listGithub);
$('#github-disconnect').onclick = () => busy($('#github-disconnect'), async () => {
  await invoke('disconnect_github');
  $('#github-repositories').replaceChildren();
  await refreshLive();
  done(t('GitHub disconnected. Local checkouts stay.'));
});
$('#source-kind').onchange = () => { $('#source-path-label').hidden = $('#source-kind').value !== 'file'; };
$('#add-source').onclick = () => {
  const id = $('#source-id').value.trim();
  const repository = $('#source-repo').value;
  const whole = $('#source-kind').value === 'repository';
  const path = $('#source-path').value.trim();
  const description = $('#source-description').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || !repository || (!whole && !path) || !description) return fail(whole
    ? t('Fill in the ID (lowercase letters, digits, - or _), repository and description.')
    : t('Fill in the ID (lowercase letters, digits, - or _), repository, file and description.'));
  if (current.map.resources.some(r => r.id === id)) return fail(t('That ID already exists.'));
  current.map.resources.push({ id, description, topics: names($('#source-topics').value), enabled: false,
    external_processing: false, allowed_conversations: [], allowed_senders: [],
    ...(whole ? { kind: 'repository', repository } : { kind: 'file', repository, path }) });
  for (const name of ['#source-id', '#source-path', '#source-topics', '#source-description']) field(name, '');
  markDirty();
  renderResources();
  notify(t('Source {id} added, disabled. Save the changes to keep it.', { id }));
};
$('#add-url-source').onclick = () => {
  const id = $('#url-source-id').value.trim(), urls = [...new Set(lines($('#url-source-addresses').value))];
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || current.map.resources.some(r => r.id === id)) return fail(t('Choose a unique source ID using lowercase letters, digits, - or _.'));
  if (!urls.length || urls.length > 10) return fail(t('Add between one and ten HTTPS URLs.'));
  let parsed;
  try { parsed = urls.map(value => new URL(value)); } catch { return fail(t('Add between one and ten HTTPS URLs.')); }
  if (parsed.some(url => url.protocol !== 'https:' || url.username || url.password || url.hash || (url.port && url.port !== '443'))) return fail(t('Add between one and ten HTTPS URLs.'));
  const wiki = current.map.resources.find(r => r.id === $('#url-source-wiki').value);
  if (parsed.some(url => url.hostname === 'dev.azure.com') && !wiki) return fail(t('Select an Azure DevOps Wiki connection for these pages.'));
  if (wiki && parsed.some(url => url.hostname !== 'dev.azure.com' || !url.pathname.includes('/_wiki/wikis/'))) return fail(t('The Wiki connection reads only Azure DevOps Wiki page URLs.'));
  current.map.resources.push({ id, description: $('#url-source-description').value.trim() || id, topics: [], enabled: false, external_processing: false, allowed_conversations: [], allowed_senders: [],
    ...(wiki ? { kind: 'tool', tool: { ...structuredClone(wiki.tool), repository_docs: false, page_urls: urls } } : { kind: 'urls', urls }) });
  for (const selector of ['#url-source-id', '#url-source-description', '#url-source-addresses']) field(selector, '');
  $('#url-source-form').open = false; openSources.add(id);
  markDirty({ target: $('#add-url-source') }); renderResources(); renderQuestions();
  notify(t('Source {id} added, disabled. Enable it, authorize it, choose its conversations and save.', { id }));
};
$('#add-record').onclick = () => {
  if (current.map.resources.some(r => r.tool?.type === 'activity_record')) return;
  const source = recordSource();
  if (current.map.resources.some(r => r.id === source.id)) return fail(t('That ID already exists.'));
  current.map.resources.push(source);
  openSources.add(source.id);
  markDirty({ target: $('#add-record') });
  renderResources();
  notify(t('Source {id} added, disabled. Enable it, authorize it, choose its conversations and save.', { id: source.id }));
};
$('#add-person').onclick = () => {
  const name = $('#person-name').value.trim();
  const sender = $('#person-sender').value.trim();
  if (!name) return fail(t('Write the person\'s name.'));
  const people = current.map.people ??= [];
  const base = folded(name).replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').replace(/^[^a-z]+/, '') || 'person';
  let id = base.slice(0, 60);
  for (let n = 2; people.some(p => p.id === id); n++) id = `${base.slice(0, 56)}-${n}`;
  people.push({ id, name, senders: sender ? [sender] : [], aliases: [], address: $('#person-address').value.trim(), notes: '' });
  openPeople.add(id);
  for (const name of ['#person-name', '#person-sender', '#person-address']) field(name, '');
  $('#person-form').open = false;
  markDirty({ target: $('#add-person') });
  renderPeople();
  notify(t('{name} added. Save the changes to keep it.', { name }));
};
$('#add-question').onclick = () => {
  const text = $('#question-text').value.trim();
  if (!text || !newQuestionSources.size) return fail(t('Write a question and select at least one source.'));
  const question = { id: `question-${crypto.randomUUID().replaceAll('-', '')}`, text, sources: [...newQuestionSources], examples: [] };
  (current.map.questions ??= []).push(question);
  openQuestions.add(question.id);
  field('#question-text', ''); newQuestionSources.clear();
  $('#question-form').open = false;
  markDirty({ target: $('#add-question') }); renderQuestions();
  notify(t('Question added. Save the changes to keep it.'));
};
$('#add-context').onclick = () => {
  const id = $('#context-id').value.trim();
  const name = $('#context-name').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || !name) return fail(t('Give the context an ID (lowercase letters, digits, - or _) and a name.'));
  const contexts = current.map.contexts ??= [];
  if (contexts.some(c => c.id === id)) return fail(t('That ID already exists.'));
  contexts.push({ id, name, description: '', keywords: names($('#context-keywords').value), sources: [], instructions: '' });
  openContexts.add(id);
  for (const name of ['#context-id', '#context-name', '#context-keywords']) field(name, '');
  markDirty({ target: $('#add-context') });
  renderContexts(); renderNoteContexts();
  notify(t('Context {id} added. Choose its sources and save the changes.', { id }));
};
$('#add-notes-source').onclick = () => {
  if (current.map.resources.some(r => r.kind === 'notes')) return;
  const id = current.map.resources.some(r => r.id === 'notes') ? 'verified-notes' : 'notes';
  current.map.resources.push({ id, description: t('Facts I verified: procedures, decisions and how things are done here.'),
    topics: [], enabled: false, external_processing: false, allowed_conversations: [], allowed_senders: [], kind: 'notes' });
  openSources.add(id);
  markDirty({ target: $('#add-notes-source') });
  renderResources(); renderNotes();
  selectTab($('#knowledge-tabs'), 'sources');
  notify(t('Source {id} added, disabled. Enable it, authorize it, choose its conversations and save.', { id }));
};
// A note started from an answer stops being that answer's once the form closes.
$('#note-form').ontoggle = () => { if (!$('#note-form').open) noteOrigin = null; };
let notesSearch;
$('#notes-query').oninput = () => { clearTimeout(notesSearch); notesSearch = setTimeout(() => loadNotes().catch(fail), 300); };
$('#add-note').onclick = () => busy($('#add-note'), async () => {
  const note = { title: $('#note-title').value.trim(), text: $('#note-text').value.trim(),
    keywords: names($('#note-keywords').value), contexts: newNoteContexts, ...(noteOrigin ?? {}) };
  if (!note.title || !note.text) return fail(t('A note needs a title and a text.'));
  await invoke('notes_save', { note });
  for (const name of ['#note-title', '#note-text', '#note-keywords']) field(name, '');
  newNoteContexts = []; renderNoteContexts();
  noteOrigin = null;
  $('#note-form').open = false;
  done(t('Note saved.'));
  await loadNotes();
});
$('#chat-all').onclick = () => {
  const boxes = [...$('#chat-sources').querySelectorAll('input')];
  const all = boxes.every(box => box.checked);
  for (const box of boxes) { box.checked = !all; if (box.checked) chosen.add(box.value); else chosen.delete(box.value); }
};
$('#chat-reset').onclick = () => {
  session = crypto.randomUUID();
  $('#history').replaceChildren($('#chat-empty'));
  $('#chat-empty').hidden = false;
};
$('#message').addEventListener('input', resizeComposer);
$('#message').addEventListener('keydown', event => {
  if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) { event.preventDefault(); $('#send').click(); }
});
$('#chat-form').onsubmit = async event => {
  event.preventDefault();
  const text = $('#message').value.trim();
  const sources = [...chosen];
  if (!text) return;
  if (!sources.length) return fail(t('Select at least one source for the test.'));
  const send = $('#send');
  send.disabled = true;
  bubble('you', text);
  field('#message', '');
  resizeComposer();
  const elapsed = el('span', { textContent: t('Writing…') });
  const pending = bubble('bot', el('span', { class: 'typing' }, el('i'), el('i'), el('i'), elapsed));
  const started = Date.now();
  const timer = setInterval(() => { elapsed.textContent = t('Writing… {n} s', { n: Math.round((Date.now() - started) / 1000) }); }, 1000);
  try {
    const result = await invoke('chat', { input: { session, text, group: false, mentioned: false, sources } });
    pending.remove();
    answerBubble({ ...result, request: text });
  } catch (error) {
    pending.remove();
    bubble('bot none', t('The test could not complete: {error}', { error: tx(String(error?.message ?? error)) }));
  } finally {
    clearInterval(timer);
    send.disabled = false;
  }
};

reload().then(async () => { await loadActivity(); await loadRegistration(); if (activityWindow) showTab('registration'); }).catch(fail);

function browserMicrosoftLogin(url) {
  const dialog = $('#web-microsoft-dialog');
  const address = new URL(url);
  if (address.protocol !== 'https:' || address.hostname !== 'login.microsoftonline.com') throw new Error(t('Invalid Microsoft sign-in address.'));
  $('#web-microsoft-link').href = address.href;
  $('#web-microsoft-redirect').value = '';
  $('#web-microsoft-status').textContent = '';
  dialog.showModal();
  return new Promise((resolve, reject) => {
    let cancelled = false;
    const close = () => { $('#web-microsoft-redirect').value = ''; $('#web-microsoft-link').removeAttribute('href'); dialog.close(); };
    const cancel = async event => {
      event?.preventDefault(); cancelled = true;
      await rawInvoke('command', { request: { method: 'cancel_microsoft', args: {}, contract: 1 } }).catch(() => {});
      close(); reject(new Error(t('Microsoft sign-in cancelled.')));
    };
    dialog.oncancel = cancel;
    $('#web-microsoft-cancel').onclick = cancel;
    $('#web-microsoft-form').onsubmit = async event => {
      event.preventDefault();
      const button = $('#web-microsoft-finish');
      button.disabled = true;
      $('#web-microsoft-status').textContent = t('Finishing sign-in…');
      try {
        let args = { redirect_url: $('#web-microsoft-redirect').value.trim() };
        $('#web-microsoft-redirect').value = '';
        const deadline = Date.now() + 600_000;
        let result;
        do {
          result = await rawInvoke('command', { request: { method: 'finish_microsoft', args, revision: null, contract: 1 } });
          args = {};
          if (result.code === 'authorization_pending') await new Promise(done => setTimeout(done, 2000));
        } while (!cancelled && result.code === 'authorization_pending' && Date.now() < deadline);
        if (cancelled) return;
        if (!result.ok) throw new Error(result.message || result.code);
        close(); resolve(result);
      } catch (error) { $('#web-microsoft-status').textContent = tx(error.message || String(error)); }
      finally { button.disabled = false; }
    };
  });
}

/** The one explicit trust decision for a computer's pending sign-in, in an authenticated browser session. */
async function machineTrust(transaction) {
  const dialog = $('#machine-trust-dialog'), status = $('#machine-trust-status');
  const request = action => window.ptaTransport.request('/api/enrollment', { action, transaction });
  history.replaceState(null, '', '/');
  const detail = await request('inspect');
  if (!detail.ok) throw new Error(tx(detail.message || detail.code));
  const { machine, code, username } = detail.data;
  $('#machine-trust-name').textContent = machine.name;
  $('#machine-trust-os').textContent = { linux: 'Linux', macos: 'macOS', windows: 'Windows' }[machine.os] || machine.os;
  $('#machine-trust-code').textContent = code;
  $('#machine-trust-account').textContent = username;
  status.textContent = '';
  const approve = $('#machine-trust-approve'), deny = $('#machine-trust-deny'), close = $('#machine-trust-close');
  approve.hidden = deny.hidden = false; close.hidden = true;
  const decided = message => {
    status.textContent = message;
    approve.hidden = deny.hidden = true; close.hidden = false; close.focus();
  };
  const decide = (button, action) => busy(button, async () => {
    const result = await request(action);
    if (!result.ok) throw new Error(tx(result.message || result.code));
    decided(action === 'trust'
      ? t('Trusted. Return to {name}: it finishes connecting by itself.', { name: machine.name })
      : t('Denied. That computer was not added to your account.'));
  });
  approve.onclick = () => decide(approve, 'trust');
  deny.onclick = () => decide(deny, 'deny');
  close.onclick = () => dialog.close();
  dialog.showModal();
  deny.focus();
}

if (window.ptaTransport.browser) {
  window.ptaTransport.ready.then(identity => {
    const enroll = new URLSearchParams(window.location.search).get('enroll');
    if (enroll) machineTrust(enroll).catch(fail);
    $('#web-account').hidden = $('#panel-web-account').hidden = $('#web-hosting-note').hidden = false;
    $('#web-username').textContent = identity.username;
    for (const id of ['#public-url', '#bind', '#tunnel-config', '#cloudflare-tunnel']) $(id).disabled = true;
    for (const button of [$('#web-logout'), $('#web-logout-settings')]) {
      button.onclick = () => busy(button, async () => {
        leaveAccount();
        const result = await window.ptaTransport.request('/api/logout', {});
        window.location.replace(result.logout_url);
      });
    }
  }).catch(fail);
}
