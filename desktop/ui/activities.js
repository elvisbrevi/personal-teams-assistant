// Activities: recorded days, their tasks and the task detail.
import { num, t, tn, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el, field, icon } from './dom.js';
import { done, fail } from './notices.js';
import { detailTrails, renderTodoDetail, todoItem, todoOriginal } from './todo.js';
import { link } from './markdown.js';

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

/** @param {any} value */
function setActivityTask(value) { activityTask = value; }
/** @param {any} value */
function setActivityDetailRequest(value) { activityDetailRequest = value; }
/** @param {any} value */
function setActivityRunning(value) { activityRunning = value; }
/** @param {any} value */
function setActivityDayRequest(value) { activityDayRequest = value; }
/** @param {any} value */
function setActivityDay(value) { activityDay = value; }

export {
  activityWindow, activityRunning, activityDay, activityTask, activityDayRequest, activityDetailRequest,
  activityDayFocus, activityTaskFocus, activityDaysScroll, activityTasksScroll, renderActivityNotice,
  showActivityView, loadActivityTask, loadRegistration, setActivityDay, setActivityDayRequest,
  setActivityDetailRequest, setActivityRunning, setActivityTask,
};
