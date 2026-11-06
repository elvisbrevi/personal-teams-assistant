// Settings › Daily record: the daily task record and its schedule.
import { t } from './i18n.js';
import { $, el, field } from './dom.js';
import { current } from './state.js';
import { markDirty, showTab } from './nav.js';

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

export { renderRegistrationSettings, renderScheduleFields, collectRegistrationSettings };
