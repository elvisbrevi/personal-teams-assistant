// To-Do: the work item list, its filters and the in-section detail.
import { transport } from './transport.js';
import { t, tn, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el, icon } from './dom.js';
import { live } from './state.js';
import { busy, done, fail } from './notices.js';
import { copy, link, todoRich, todoSafeUrl } from './markdown.js';
import { loadWorkItemSessions } from './sessions.js';
import { openImplementation } from './implementation.js';
import { refreshActiveRuns, runLinkFor } from './workflow-runs.js';
import {
  activityDetailRequest, activityTask, loadActivityTask, setActivityDetailRequest, setActivityTask,
} from './activities.js';

// ---------------------------------------------------------------- To-Do

let todoRequest = 0;
/** How many items To-Do asks for: it starts at the first step and «Show more» takes the next one. */
const TODO_LIMITS = [10, 25, 50, 100];
let todoLimit = TODO_LIMITS[0];
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
  const open = transport.browser
    ? el('a', { class: 'icon-button secondary', href: item.url, target: '_blank', rel: 'noopener noreferrer', title: label, 'aria-label': label }, icon('link'))
    : el('button', { type: 'button', class: 'icon-button secondary', title: label, 'aria-label': label }, icon('link'));
  if (!transport.browser) open.onclick = () => busy(open, () => invoke('open_work_item', { url: item.url }));
  const copyButton = el('button', {
    type: 'button', class: 'icon-button secondary', title: t('Copy work item link'),
    'aria-label': t('Copy work item link'),
  }, icon('copy'));
  copyButton.onclick = () => busy(copyButton, async () => {
    if (await copy(item.url)) done(t('Link copied: {url}', { url: item.url }));
    else fail(t('Could not copy the work item link.'));
  });
  const session = el('button', { type: 'button', class: 'icon-button secondary', hidden: true, 'data-session-for': item.url, title: t('Open agent session'), 'aria-label': t('Open agent session') }, icon('chat'));
  return el('div', { class: 'todo-actions' }, runLinkFor(item), implement, open, copyButton, session);
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
/** The dates a card shows: the two recorded ones nearest to today, in order; without any, when it was created.
 *  The detail lists them all. */
function todoCardDates(item) {
  const now = Date.now();
  const recorded = item.dates.filter(date => date.value && !Number.isNaN(new Date(date.value).valueOf()));
  const nearest = [...recorded].sort((a, b) => Math.abs(new Date(a.value).valueOf() - now) - Math.abs(new Date(b.value).valueOf() - now)).slice(0, 2)
    .sort((a, b) => new Date(a.value).valueOf() - new Date(b.value).valueOf());
  return nearest.length ? nearest.map(date => todoDate(tx(date.name), date.value)) : [todoDate(t('Created'), item.created_at)];
}
function todoItem(item, onSelect = null, selection = null) {
  const stateStyle = todoStateStyle(item.state);
  const select = el('button', { type: 'button', class: 'todo-select', textContent: `#${item.id} · ${item.title}` });
  select.onclick = () => {
    if (onSelect) return onSelect(select);
    todoListScroll = window.scrollY;
    todoReturnFocus = select;
    todoSelection = { provider: $('#todo-provider').value, source: $('#todo-source').value, item, task: item.category === 'task' };
    loadTodoDetail();
  };
  return el('article', { class: `todo-item ${stateStyle.class}`.trim() },
    el('div', { class: 'todo-heading' }, el('h3', {}, select), todoOriginal(item, selection)),
    el('div', { class: 'todo-context' }, badge(tx(item.state), stateStyle.tone), badge(tx(item.kind || 'HU'), 'muted'), el('span', { textContent: item.project })),
    el('dl', { class: 'todo-dates' }, ...todoCardDates(item)));
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
    setActivityTask({ ...activityTask, item: relation.item, task: relation.task });
    loadActivityTask();
  }
}
function backRelatedItem(prefix) {
  const previous = detailTrails[prefix].pop();
  if (!previous) return false;
  if (prefix === 'todo') { ++todoDetailRequest; todoSelection = previous.selection; $('#todo-detail-problem').hidden = true; }
  else { setActivityDetailRequest(activityDetailRequest + 1); setActivityTask(previous.selection); }
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
/** The Azure types the last list returned, each with its group (`hu`, `bug` or `task`). */
let todoTypes = [];
/** The type selector holds the groups, then each exact type the process defines (`type:NAME`). */
function todoTypeOptions(types) {
  todoTypes = types;
  const select = $('#todo-kind');
  const selected = select.value;
  select.querySelector('optgroup')?.remove();
  // The process's own names: a translated «User Story» would repeat the HU group.
  const names = [...new Set(types.map(type => type.name))].sort((a, b) => a.localeCompare(b, uiLocale()));
  if (names.length) select.append(el('optgroup', { label: t('Work item types') }, ...names.map(name => el('option', { value: `type:${name}`, textContent: name }))));
  select.value = selected;
}
function todoTitle() {
  const kind = $('#todo-kind').value;
  return kind === 'task' ? t('My tasks') : kind.startsWith('type:') ? t('My work items') : t('My HUs and bugs');
}
/** Loads the list. A change of filters starts again at the first step; `more` keeps what is shown while the longer list loads. */
async function loadTodo({ more = false, keep = more } = {}) {
  closeTodoDetail(false);
  if (!keep) todoLimit = TODO_LIMITS[0];
  const request = ++todoRequest;
  const azure = $('#todo-provider').value === 'azure';
  const sources = todoSources();
  $('#todo-title').textContent = azure ? todoTitle() : $('#todo-github-kind').value === 'all' ? t('My issues') : t('My epics');
  $('#todo-kind-field').hidden = !azure;
  $('#todo-github-kind-field').hidden = azure;
  if (!more) $('#todo-list').replaceChildren();
  $('#todo-list').setAttribute('aria-busy', 'true');
  $('#todo-more').hidden = !more;
  if (!more) $('#todo-progress').textContent = t('Loading…');
  $('#todo-assignment').textContent = '';
  $('#todo-problem').hidden = true;
  $('#todo-warnings').hidden = true;
  $('#todo-refresh').disabled = true;
  try {
    if (azure && !sources.length) throw t('Configure an Azure DevOps activity source in Knowledge to see your HUs.');
    const args = { limit: todoLimit };
    if (azure) {
      args.source = $('#todo-source').value;
      const kind = $('#todo-kind').value;
      if (kind.startsWith('type:')) args.type = kind.slice(5);
      else if (kind) args.kind = kind;
    }
    if ($('#todo-state').value) args.state = $('#todo-state').value;
    const [result] = await Promise.all([invoke(azure ? 'list_azure_hus' : $('#todo-github-kind').value === 'all' ? 'list_github_issues' : 'list_github_epics', args), azure ? refreshActiveRuns() : Promise.resolve()]);
    if (request !== todoRequest) return;
    const selected = $('#todo-state').value;
    $('#todo-state').replaceChildren(el('option', { value: '', textContent: t('All states') }), ...result.states.map(state => el('option', { value: state, textContent: tx(state) })));
    $('#todo-state').value = selected;
    if (azure) todoTypeOptions(result.types || []);
    $('#todo-assignment').textContent = azure
      ? t('Your assignment fields: {fields}', { fields: result.assignment_fields.join(', ') })
      : $('#todo-github-kind').value === 'all' ? t('Issues assigned to your connected GitHub account.') : t('Issues of type Epic assigned to your connected GitHub account. GitHub provides open and closed states; unavailable dates stay empty.');
    $('#todo-list').replaceChildren(...result.items.map(item => todoItem(item)));
    loadWorkItemSessions().catch(fail);
    if (!result.items.length) $('#todo-list').append(el('p', { class: 'empty', textContent: t('No items match these filters. Try another state.') }));
    $('#todo-progress').textContent = result.has_more
      ? t('Showing the {n} newest items.', { n: result.items.length })
      : tn(result.items.length, '{n} item', '{n} items');
    $('#todo-more').hidden = !result.has_more || todoLimit >= TODO_LIMITS.at(-1);
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

/** @param {any} value */
function setTodoLimit(value) { todoLimit = value; }

export {
  TODO_LIMITS, todoLimit, todoOriginal, detailTrails, todoItem, closeTodoDetail, backRelatedItem,
  renderTodoDetail, loadTodoDetail, todoTypes, loadTodo, setTodoLimit,
};
