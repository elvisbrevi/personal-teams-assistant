const rawInvoke = window.ptaTransport.invoke;
// Also the answer operations: they do not depend on the profile files, so they neither check
// nor advance the form's revision.
const READS = ['snapshot', 'chat', 'github_repositories', 'self_chat_status', 'llm_providers', 'audit', 'activity_status', 'activity_history', 'activity_pending', 'answer_refine', 'answer_send', 'list_azure_hus', 'list_github_epics', 'get_azure_hu', 'get_azure_task', 'list_azure_tasks', 'get_github_epic', 'open_work_item'];
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
    method, args, revision: READS.includes(method) ? null : current?.revision ?? null, contract: 1
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
  if (result.revision && current && !READS.includes(method)) current.revision = result.revision;
  if (!result.ok) throw result.message || result.code;
  return result.data;
}

const $ = (selector) => document.querySelector(selector);
/** Snapshot being edited by the forms (saved with «Save changes»). */
let current;
/** Latest persisted snapshot: drives status, overview and the test chat (which reads the saved map). */
let live;
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
  }
}

function showTab(name, anchor) {
  tab = name;
  setNavOverflow(false);
  for (const section of ['overview', 'todo', 'settings', 'messages', 'knowledge', 'chat', 'registration']) $('#' + section).hidden = section !== name;
  for (const button of document.querySelectorAll('nav button[data-tab]')) {
    button.setAttribute('aria-current', button.dataset.tab === name ? 'page' : 'false');
  }
  $('#nav-more').setAttribute('aria-current', ['knowledge', 'chat'].includes(name) ? 'page' : 'false');
  if (name === 'messages') loadMessages().catch(fail);
  if (name === 'overview') loadActivity();
  if (name === 'registration') loadRegistration().catch(fail);
  if (name === 'knowledge') loadKnowledge().catch(fail);
  if (name === 'todo') loadTodo();
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
function todoOriginal(item) {
  const label = t('Open #{id} in a new tab', { id: item.id });
  const open = window.ptaTransport.browser
    ? el('a', { class: 'icon-button secondary', href: item.url, target: '_blank', rel: 'noopener noreferrer', title: label, 'aria-label': label }, icon('link'))
    : el('button', { type: 'button', class: 'icon-button secondary', title: label, 'aria-label': label }, icon('link'));
  if (!window.ptaTransport.browser) open.onclick = () => busy(open, () => invoke('open_work_item', { url: item.url }));
  return open;
}
let todoDetailRequest = 0;
let todoSelection = null;
let todoListScroll = 0;
let todoReturnFocus = null;
function todoItem(item, onSelect = null) {
  const select = el('button', { type: 'button', class: 'todo-select', textContent: `#${item.id} · ${item.title}` });
  select.onclick = () => {
    if (onSelect) return onSelect(select);
    todoListScroll = window.scrollY;
    todoReturnFocus = select;
    todoSelection = { provider: $('#todo-provider').value, source: $('#todo-source').value, item };
    loadTodoDetail();
  };
  return el('article', { class: 'todo-item' },
    el('div', { class: 'todo-heading' }, el('h3', {}, select), todoOriginal(item)),
    el('div', { class: 'todo-context' }, badge(tx(item.state), 'info'), badge(tx(item.kind || 'HU'), 'muted'), el('span', { textContent: item.project })),
    el('dl', { class: 'todo-dates' }, todoDate(t('Created'), item.created_at), ...item.dates.map(date => todoDate(tx(date.name), date.value))));
}
function closeTodoDetail(restore = true) {
  ++todoDetailRequest;
  todoSelection = null;
  $('#todo-detail').hidden = true;
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
/** Render the core's safe rich-text tree; provider HTML never enters the browser's parser. */
function todoRich(text, format, base, content) {
  if (format !== 'html') return format === 'markdown' ? markdown(text) : el('div', { class: 'todo-text', textContent: text });
  if (!Array.isArray(content)) return el('div', { class: 'todo-text', textContent: text });
  const root = el('div', { class: 'md' });
  const allowed = new Set(['p', 'div', 'span', 'strong', 'b', 'em', 'i', 'u', 's', 'del', 'br', 'ul', 'ol', 'li', 'pre', 'code', 'blockquote', 'hr', 'table', 'thead', 'tbody', 'tr', 'th', 'td']);
  let remaining = 12000;
  let clipped = false;
  function rebuild(node, parent, depth = 0) {
    if (--remaining < 0 || depth > 40) { clipped = true; return; }
    if (node.tag === 'notice') { clipped = true; return; }
    if (node.tag === 'text') { parent.append(node.text || ''); return; }
    if (node.tag === 'a' || node.tag === 'img') {
      const url = node.url && todoSafeUrl(node.url, base);
      const label = node.tag === 'img' ? node.text || t('View image') : richLabel(node) || url || '';
      parent.append(url ? link(label, url) : label);
      return;
    }
    const tag = /^h[1-6]$/.test(node.tag) ? 'h3' : allowed.has(node.tag) ? node.tag : null;
    if (!tag) return;
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
    el('div', { class: 'todo-context' }, badge(tx(item.kind), 'muted'), badge(tx(item.state), 'info'), el('span', { textContent: item.project })),
    el('div', { class: 'todo-heading' }, el('h2', { id: `${prefix}-detail-title`, tabIndex: -1, textContent: `#${item.id} · ${item.title}` }), todoOriginal(item)),
    todoFields(shown, base),
    el('dl', { class: 'todo-dates' }, todoDate(t('Created'), item.created_at), ...item.dates.map(date => todoDate(tx(date.name), date.value))));
  const panels = content.map(field => el('section', { class: 'panel' }, el('h2', { textContent: tx(field.name) }), todoValue(field, base)));
  const relations = detail.relations.map(relation => el('div', { class: 'todo-relation' }, badge(tx(relation.name), 'muted'), relation.url && todoSafeUrl(relation.url, base) ? link(relation.detail || relation.url, relation.url) : el('span', { textContent: relation.detail })));
  const comments = detail.comments.map(comment => el('article', { class: 'todo-comment' },
    el('div', { class: 'todo-context' }, el('strong', { textContent: comment.author || t('Unknown author') }), comment.created_at ? el('time', { dateTime: comment.created_at, textContent: new Date(comment.created_at).toLocaleString(uiLocale(), { dateStyle: 'medium', timeStyle: 'short' }) }) : null,
      comment.modified_at && comment.modified_at !== comment.created_at ? el('span', { textContent: t('Edited') }) : null), todoRich(comment.text, comment.format, base, comment.content)));
  const history = detail.history.map(change => el('article', { class: 'todo-comment' }, el('div', { class: 'todo-context' }, el('strong', { textContent: change.author || t('Unknown author') }), change.at ? el('time', { dateTime: change.at, textContent: new Date(change.at).toLocaleString(uiLocale(), { dateStyle: 'medium', timeStyle: 'short' }) }) : null),
    todoFields(change.fields.map(field => ({ ...field, kind: 'string', value: typeof field.value === 'object' && field.value !== null && ('newValue' in field.value || 'oldValue' in field.value) ? `${todoHistoryValue(field.value.oldValue)} → ${todoHistoryValue(field.value.newValue)}` : field.value })), base)));
  $(`#${prefix}-detail-content`).replaceChildren(header,
    ...(detail.partial ? [ el('div', { class: 'callout warn', role: 'status' }, ...detail.warnings.map(warning => el('p', { textContent: tx(warning) })))] : []),
    ...panels,
    el('section', { class: 'panel' }, el('div', { class: 'panel-head' }, el('h2', { textContent: t('Comments') }), el('span', { class: 'count', textContent: comments.length })), ...(comments.length ? comments : [el('p', { class: 'hint', textContent: t('No comments to show.') })])),
    el('section', { class: 'panel' }, el('div', { class: 'panel-head' }, el('h2', { textContent: t('Links and evidence') }), el('span', { class: 'count', textContent: relations.length })), ...(relations.length ? relations : [el('p', { class: 'hint', textContent: t('No linked items to show.') })])),
    todoFold(t('All fields'), [todoFields(detail.fields, base)], detail.fields.length),
    todoFold(t('Change history'), history.length ? history : [el('p', { class: 'hint', textContent: t('No history to show.') })], history.length));
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
    const detail = await invoke(selected.provider === 'azure' ? 'get_azure_hu' : 'get_github_epic', args);
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
  $('#todo-title').textContent = azure ? t('My HUs and bugs') : t('My epics');
  $('#todo-kind-field').hidden = !azure;
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
    const result = await invoke(azure ? 'list_azure_hus' : 'list_github_epics', args);
    if (request !== todoRequest) return;
    const selected = $('#todo-state').value;
    $('#todo-state').replaceChildren(el('option', { value: '', textContent: t('All states') }), ...result.states.map(state => el('option', { value: state, textContent: tx(state) })));
    $('#todo-state').value = selected;
    $('#todo-assignment').textContent = azure
      ? t('Your assignment fields: {fields}', { fields: result.assignment_fields.join(', ') })
      : t('Issues of type Epic assigned to your connected GitHub account. GitHub provides open and closed states; unavailable dates stay empty.');
    $('#todo-list').replaceChildren(...result.items.map(todoItem));
    if (!result.items.length) $('#todo-list').append(el('p', { class: 'empty', textContent: t('No items match these filters. Try another state.') }));
    $('#todo-progress').textContent = result.has_more
      ? t('Showing the {n} newest items. Increase the limit to see more.', { n: result.items.length })
      : tn('{n} item', '{n} items', result.items.length);
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

// ---------------------------------------------------------------- settings sections

/** Settings sections with edits not saved yet, so a hidden section never hides them. */
const dirtyGroups = new Set();

/** Selects the tab `name` of a tab list and shows only its panel. */
function selectTab(list, name, focus = false) {
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
    { done: !missing.length, title: t('Credentials'), text: missing.length ? t('Missing: {names}.', { names: missing.join(', ') }) : t('The required credentials are stored.'), go: ['settings', 'panel-credentials'] },
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
    credentials: missingCredentials(s.credentials).length > 0,
    teams: !s.microsoft_connected || !entraReady(s.config) || !urlReady(s.config),
    models: !enabledChain(s.config).length,
  });
}

/** Refreshes persisted state without touching unsaved form edits. */
async function refreshLive() {
  live = await invoke('snapshot');
  if (current) { current.credentials = live.credentials; current.github_connected = live.github_connected; }
  renderLive();
  renderCredentials();
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
  renderCredentials(); renderRepositories(); renderResources(); renderContexts(); renderCases(); renderPeople(); renderNoteContexts(); renderChatSources();
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

function renderCredentials() {
  const container = $('#credentials');
  container.replaceChildren();
  const origins = { system: t('System store'), environment: t('Environment variable'), file: t('Private file') };
  for (const [name, source] of Object.entries(live.credentials)) {
    const generated = source === 'missing' && GENERATED.includes(name);
    const retired = source === 'retired';
    const empty = source === 'missing' || source === 'unused';
    const state = generated ? badge(t('Generated on start'), 'info')
      : source === 'missing' ? badge(t('Missing'), 'warn')
      : source === 'unused' ? badge(t('Unused'), 'muted')
      : retired ? badge(t('No longer used'), 'muted')
      : badge(t('Set'), 'ok');
    const detail = generated ? t('The assistant creates it on its first start.')
      : source === 'unused' ? t('The current model chain does not use it.')
      : retired ? t('An earlier version needed it; nothing reads it now. You can remove it.')
      : empty ? t('Needed to start the assistant.')
      : origins[source] || t('Set');
    const title = el('span', { class: 'cred-name', textContent: name });
    const input = el('input', { type: 'password', autocomplete: 'new-password', spellcheck: false, hidden: retired,
      placeholder: empty ? t('Enter the credential') : t('Type only to replace it'), 'aria-label': t('New value of {name}', { name }) });
    // Storing a missing credential is the next step; replacing a stored one is not.
    const save = el('button', { class: empty ? null : 'secondary', textContent: empty ? t('Save') : t('Replace'), disabled: true, hidden: retired });
    input.oninput = () => { save.disabled = !input.value.trim(); };
    input.onkeydown = event => { if (event.key === 'Enter' && !save.disabled) save.click(); };
    save.onclick = () => busy(save, async () => {
      await invoke('set_credential', { name, value: input.value });
      input.value = '';
      await refreshLive();
      done(t('{name} stored in the system.', { name }));
    });
    const remove = el('button', { class: 'danger', textContent: t('Remove'), disabled: empty, 'aria-label': t('Remove {name}', { name }) });
    remove.onclick = () => busy(remove, async () => {
      await invoke('delete_credential', { name });
      await refreshLive();
      done(t('{name} removed.', { name }));
    });
    container.append(el('div', { class: 'row' }, rowText(title, detail, state), input, save, remove));
  }
}

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
  return source && source !== 'missing' && source !== 'unused' ? t('API · {name} set', { name: info.credential }) : t('API · {name} missing in Credentials', { name: info.credential });
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
      });
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
const rateable = row => !!(row.audit.sent || row.audit.proposed)?.trim() && !row.resource.startsWith('simulation:')
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
    exampleFields.hidden = draft.rating !== 'good';
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
    await loadMessages();
  });
  const asNote = el('button', { type: 'button', class: 'secondary', textContent: t('Save as a note') });
  asNote.onclick = () => noteFromAnswer(row);
  sync();
  return el('div', { class: 'field-block feedback' },
    el('strong', { textContent: t('Your rating') }),
    ratings, exampleFields,
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
  if (resource.kind === 'url') return 'URL';
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
  if (resource.kind === 'repository') return `${t('Repository')}: ${resource.repository} · ${indexText(resource)}`;
  if (resource.kind === 'notes') return resource.contexts?.length
    ? t('Your verified notes of {contexts} and the general ones', { contexts: resource.contexts.join(', ') })
    : `${t('Your verified notes')} · ${indexText(resource)}`;
  const tool = resource.tool ?? {};
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
    const input = el('input', { value: path, spellcheck: false, 'aria-label': t('Path of {alias}', { alias }) });
    input.oninput = () => { current.map.repositories[alias] = input.value.trim(); markDirty(); };
    const update = el('button', { class: 'secondary', textContent: t('Update') });
    update.onclick = () => busy(update, async () => { await invoke('update_github_repository', { alias }); done(t('{alias} updated.', { alias })); });
    const remove = el('button', { class: 'danger', textContent: t('Remove') });
    remove.onclick = () => {
      const tools = current.map.resources.filter(r => r.kind === 'tool' && r.tool?.repository === alias);
      if (tools.length) return fail(t('{alias} cannot be removed: {ids} use it. Remove those sources first.', { alias, ids: tools.map(r => r.id).join(', ') }));
      const files = current.map.resources.filter(r => r.repository === alias);
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
      rowText(alias, source ? t('Git checkout · source {id}', { id: source.id }) : t('Git checkout')), input,
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

// ---------------------------------------------------------------- cases

/** What each built-in case is, in the interface language (the host sends its defaults). */
const caseTexts = {
  general: { name: t('Every answer'), summary: t('How every answer sounds, and how the model recognizes a question.') },
  status_round: { name: t('Status round'), summary: t('Progress, commitments, risks or blockers of your work («avance», «compromiso», «bloqueos», a named HU).'),
    sources: t('As built it reads Azure DevOps activity, your own recent Wiki pages and the activity record.') },
  recap: { name: t('What you did'), summary: t('What you did on a day or period («¿qué hice ayer?», «la semana pasada»), from the activity record alone.') },
  report_tasks: { name: t('Report tasks'), summary: t('Report registered tasks or answer a reminder to log hours, from the activity record alone.') },
  activity_review: { name: t('Activity review'), summary: t('Your work that no task records yet, compared with your work items.'),
    sources: t('As built it reads Azure DevOps activity, Wiki edits, your Teams messages and the activity record.') },
  documentation: { name: t('Documentation'), summary: t('How something works or is used («según la wiki», endpoints, procedures), from the Wiki.'),
    sources: t('As built it reads every authorized Wiki.') },
  personal_request: { name: t('Request for you'), summary: t('A call, a meeting or your availability: left for you to answer, outside your personal chat.') },
  statement: { name: t('Informational message'), summary: t('A message that asks for no answer: left unanswered, and replied to only when you ask for an answer in Messages.') },
};
/** Cases whose edit fold is open, kept across re-renders. */
const openCases = new Set();

/** Keeps only what differs from the built-in case; an unchanged case leaves the map. */
function setCase(id, change) {
  const clean = { id };
  if (change.enabled === false) clean.enabled = false;
  for (const key of ['description', 'instructions']) if (change[key]?.trim()) clean[key] = change[key].trim();
  for (const key of ['keywords', 'sources']) if (change[key]?.length) clean[key] = change[key];
  current.map.cases = (current.map.cases ?? []).filter(c => c.id !== id);
  if (Object.keys(clean).length > 1) current.map.cases.push(clean);
}

function renderCases() {
  const container = $('#cases');
  container.replaceChildren();
  const changed = (current.map.cases ?? []).length;
  $('#cases-summary').textContent = changed ? tn(changed, '1 changed', '{n} changed') : t('As built');
  for (const built of current.cases ?? []) {
    const texts = caseTexts[built.id] ?? { name: built.name, summary: built.summary };
    const change = structuredClone((current.map.cases ?? []).find(c => c.id === built.id) ?? { id: built.id });
    const badges = el('span', { class: 'case-badges' });
    const renderBadges = () => badges.replaceChildren(
      ...(change.enabled === false ? [badge(t('Off'), 'warn')] : []),
      ...((current.map.cases ?? []).some(c => c.id === built.id) ? [badge(t('Changed'), 'active')] : [badge(t('As built'), 'muted')]));
    renderBadges();
    const keep = () => { setCase(built.id, change); markDirty({ target: container }); renderBadges(); summary(); };
    const summary = () => {
      const count = (current.map.cases ?? []).length;
      $('#cases-summary').textContent = count ? tn(count, '1 changed', '{n} changed') : t('As built');
    };
    const area = (label, value, fallback, onchange) => {
      const element = el('textarea', { value: value || fallback, rows: 6 });
      element.oninput = () => { onchange(element.value.trim() === fallback.trim() ? '' : element.value); keep(); };
      return el('label', {}, label, element);
    };
    const parts = [];
    if (built.optional) {
      const box = el('input', { type: 'checkbox', checked: change.enabled !== false });
      box.onchange = () => { change.enabled = box.checked; keep(); };
      parts.push(el('div', { class: 'resource-toggles' }, el('label', { class: 'check' }, box, t('Use this case'))));
    }
    if (built.description) parts.push(area(t('How the model recognizes it'), change.description, built.description, value => { change.description = value; }));
    if (built.keywords) {
      const words = el('input', { value: (change.keywords ?? []).join(', '), placeholder: t('Comma separated'), spellcheck: false });
      words.oninput = () => { change.keywords = names(words.value); keep(); };
      parts.push(el('label', {}, t('Extra words that identify it'), el('small', { textContent: t('Besides the built-in ones; matched whole, ignoring case and accents.') }), words));
    }
    if (built.sources) {
      const choices = el('fieldset', { class: 'choices' }, el('legend', { textContent: t('Sources it reads') }),
        el('p', { class: 'hint', textContent: `${t('With none chosen, as built.')} ${texts.sources ?? ''}` }));
      for (const resource of current.map.resources) {
        const box = el('input', { type: 'checkbox', value: resource.id, checked: (change.sources ?? []).includes(resource.id) });
        box.onchange = () => {
          change.sources = box.checked ? [...(change.sources ?? []), resource.id] : (change.sources ?? []).filter(id => id !== resource.id);
          keep();
        };
        choices.append(el('label', { class: 'check' }, box, el('span', {}, el('strong', { textContent: resource.id }), el('small', { textContent: kindName(resource) }))));
      }
      parts.push(choices);
    }
    if (built.instructions) parts.push(area(t('How to answer it'), change.instructions, built.instructions, value => { change.instructions = value; }));
    const reset = el('button', { type: 'button', class: 'secondary small', textContent: t('Restore defaults') });
    reset.onclick = () => {
      current.map.cases = (current.map.cases ?? []).filter(c => c.id !== built.id);
      markDirty({ target: container });
      renderCases();
    };
    const edit = el('details', { class: 'fold resource-edit', open: openCases.has(built.id) },
      el('summary', { textContent: t('Edit case') }), ...parts, el('div', { class: 'actions' }, reset));
    edit.ontoggle = () => { if (edit.open) openCases.add(built.id); else openCases.delete(built.id); };
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: texts.name }), el('span', { class: 'grow' }), badges),
      el('p', { class: 'resource-facts' }, el('span', { textContent: texts.summary })),
      edit));
  }
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
].map(part => part.source).join('|'), 'g');

function inline(text, parent) {
  const wrap = (tag, body, props) => { const node = el(tag, props); inline(body, node); return node; };
  let last = 0;
  for (const match of text.matchAll(INLINE)) {
    if (match.index > last) parent.append(text.slice(last, match.index));
    const [, escaped, code, both, bold, underBold, strike, italic, underItalic, tone, toned, label, url] = match;
    if (escaped !== undefined) parent.append(escaped);
    else if (code !== undefined) parent.append(el('code', { textContent: code }));
    else if (both !== undefined) parent.append(el('strong', {}, wrap('em', both)));
    else if ((bold ?? underBold) !== undefined) parent.append(wrap('strong', bold ?? underBold));
    else if (strike !== undefined) parent.append(wrap('s', strike));
    else if ((italic ?? underItalic) !== undefined) parent.append(wrap('em', italic ?? underItalic));
    else if (tone !== undefined) parent.append(wrap('span', toned.trim(), { class: `tone-${tone}` }));
    else parent.append(link(label, url));
    last = match.index + match[0].length;
  }
  if (last < text.length) parent.append(text.slice(last));
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
  if (!result.answer) return bubble('bot none', `${t('No answer')}: ${reasonText(result.reason) || result.status}`);
  const meta = el('div', { class: 'meta' });
  for (const id of result.used_sources ?? []) meta.append(badge(id, 'active'));
  if (result.partial) meta.append(badge(t('Partial coverage'), 'warn'));
  for (const warning of result.warnings ?? []) meta.append(badge(warning, 'warn'));
  let refs = null;
  if (result.references?.length) {
    refs = el('div', { class: 'refs' }, el('strong', { textContent: t('Verified references') }),
      el('ul', {}, ...result.references.map(ref => el('li', {}, link(ref.label || ref.id, ref.url)))));
  }
  return bubble('bot', markdown(result.answer), meta.childNodes.length ? meta : null, refs);
}

function resizeComposer() {
  const area = $('#message');
  area.style.height = 'auto';
  area.style.height = `${Math.min(area.scrollHeight + 2, 192)}px`;
}

// ---------------------------------------------------------------- wiring

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
$('#todo-back').onclick = () => closeTodoDetail();
$('#todo-detail-refresh').onclick = () => loadTodoDetail();
$('#todo-state').onchange = () => loadTodo();
$('#todo-limit').onchange = () => loadTodo();
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
  field('#repo-alias', ''); field('#repo-path', ''); field('#repo-description', '');
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
    answerBubble(result);
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

if (window.ptaTransport.browser) {
  window.ptaTransport.ready.then(identity => {
    $('#web-account').hidden = $('#panel-web-account').hidden = $('#web-hosting-note').hidden = false;
    $('#web-username').textContent = identity.username;
    for (const id of ['#public-url', '#bind', '#tunnel-config', '#cloudflare-tunnel']) $(id).disabled = true;
    for (const button of [$('#web-logout'), $('#web-logout-settings')]) {
      button.onclick = () => busy(button, async () => {
        const result = await window.ptaTransport.request('/api/logout', {});
        window.location.replace(result.logout_url);
      });
    }
  }).catch(fail);
}
