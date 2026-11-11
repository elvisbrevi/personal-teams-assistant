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
  $('#registration-effort-types').replaceChildren(...Object.entries(s.effort_fields ?? {}).map(([kind, name]) => effortTypeRow(kind, name)));
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
/** One work item type and the field that holds its actual hours; edits mark the section changed. */
function effortTypeRow(kind = '', name = '') {
  const input = (label, value, placeholder, data) => {
    const box = el('input', { value, placeholder, spellcheck: false, translate: 'no' });
    box.dataset[data] = '';
    box.oninput = markDirty;
    return el('label', {}, label, box);
  };
  const row = el('div', { class: 'grid effort-type' },
    input(t('Work item type'), kind, 'Bug', 'kind'),
    input(t('Field'), name, 'Custom.EsfuerzoReal', 'field'));
  const remove = el('button', { type: 'button', class: 'secondary small', textContent: t('Remove'),
    'aria-label': t('Remove the effort field of {type}', { type: kind || t('this type') }) });
  remove.onclick = () => {
    // The row leaves the page, so the section is marked from the list that held it.
    const list = $('#registration-effort-types');
    row.remove();
    markDirty({ target: list });
  };
  row.append(el('div', { class: 'actions' }, remove));
  return row;
}
function addEffortType() {
  const row = effortTypeRow();
  $('#registration-effort-types').append(row);
  markDirty({ target: row });
  row.querySelector('input').focus();
}
/** The field names the host accepts for actual hours. */
function effortField(name) {
  return /^[A-Za-z0-9][A-Za-z0-9._-]{0,149}$/.test(name) && !name.startsWith('System.') && name !== 'Microsoft.VSTS.Scheduling.RemainingWork';
}
function collectEffortTypes() {
  const fields = {};
  for (const row of document.querySelectorAll('#registration-effort-types .effort-type')) {
    const kind = row.querySelector('[data-kind]').value.trim();
    const name = row.querySelector('[data-field]').value.trim();
    if (!kind && !name) continue;
    if (!kind || !effortField(name)) {
      showTab('settings', 'panel-registration');
      throw t('Each type needs a field that holds actual hours, such as Custom.EsfuerzoReal; not a System field or remaining work.');
    }
    if (Object.keys(fields).some(known => known.toLowerCase() === kind.toLowerCase())) {
      showTab('settings', 'panel-registration');
      throw t('{type} has more than one effort field.', { type: kind });
    }
    fields[kind] = name;
  }
  return fields;
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
    effort_fields: collectEffortTypes(),
    sources: [...document.querySelectorAll('#registration-sources input:checked')].map(c => c.dataset.source) });
}

export { renderRegistrationSettings, renderScheduleFields, collectRegistrationSettings, addEffortType };
