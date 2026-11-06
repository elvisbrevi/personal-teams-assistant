// Settings › Advanced › credential migration.
import { t, tx } from './i18n.js';
import { invoke, rawInvoke } from './control.js';
import { $, badge, el, rowText } from './dom.js';
import { busy, done } from './notices.js';
import { accountEpoch, adoptAccount } from './settings-account.js';

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

/** @param {any} value */
function setMigrationState(value) { migrationState = value; }

export { migrationState, loadMigration, renderMigration, setMigrationState };
