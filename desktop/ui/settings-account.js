// The account: standing, configuration, readiness, sign-in and synchronization, and Connect an agent.
import { transport } from './transport.js';
import { t, tn, tx } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el, icon } from './dom.js';
import { current, dirty, live, readiness, setReadiness, tab } from './state.js';
import { busy, done, fail, notify } from './notices.js';
import { showTab } from './nav.js';
import { copy } from './markdown.js';
import { machineLogin } from './settings-machines.js';
import { renderSteps } from './home.js';
import { reload } from './profile.js';
import {
  clearTypedVariableValues, consumerChoice, consumerNote, consumerState, loadVariables, renderConsumers,
  renderVariables, setConsumerState, setVariableAttempt, setVariableState, variableAttempt, variableState,
} from './settings-variables.js';
import { migrationState, renderMigration, setMigrationState } from './settings-migration.js';
import { cloudStatus, renderCloud, setCloudStatus } from './settings-cloud.js';
import { ago } from './messages.js';

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
  setVariableState({ variables: [], pending: [], error: '', sync: null });
  setVariableAttempt(null);
  setConsumerState({ consumers: [], launchers: [], launchersHere: false, error: '' });
  setMigrationState({ review: null, error: '', choices: {}, result: null });
  for (const key of Object.keys(consumerChoice)) delete consumerChoice[key];
  for (const key of Object.keys(consumerNote)) delete consumerNote[key];
  accountConfig = null;
  accountStanding = null;
  setReadiness(null);
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

/**
 * Settings › Advanced › Connect an agent. The remote MCP endpoint lives on the account's portal:
 * this page's own origin in the browser, or the coordinator this computer is signed in to.
 */
async function agentEndpoint() {
  if (transport.browser) return `${window.location.origin}/mcp`;
  const settings = await invoke('machine_settings').catch(() => null);
  const origin = settings?.coordinator_origin || settings?.coordinator || '';
  try {
    const url = new URL(origin);
    return url.protocol === 'https:' ? `${url.origin}/mcp` : '';
  } catch { return ''; }
}
function agentCommand(text) {
  return el('code', { textContent: text, translate: 'no' });
}
async function loadAgentEndpoint() {
  const address = await agentEndpoint();
  $('#agent-endpoint').hidden = !address;
  $('#agent-endpoint-url').textContent = address;
  const missing = $('#agent-endpoint-missing');
  missing.hidden = Boolean(address);
  missing.replaceChildren(t('This computer is not signed in to a portal, so there is no address yet. Agents on this computer use the local server: run pta mcp config to see how to add it.'));
  $('#agent-endpoint-help').hidden = !address;
  $('#agent-step-claude').replaceChildren(t('Claude Code:'), ' ', agentCommand(`claude mcp add --transport http personal-teams-assistant ${address}`));
  $('#agent-step-other').replaceChildren(t('Other agents: add a remote MCP server (Streamable HTTP) with this address. The agent opens the browser for you to sign in; no key is copied.'));
  $('#agent-step-local').replaceChildren(t('On a computer where this app is installed, the local server (pta mcp) needs no browser and keeps that computer\'s own permissions.'));
}
$('#agent-endpoint-copy').onclick = async () => {
  const copied = await copy($('#agent-endpoint-url').textContent);
  if (copied) notify(t('Address copied.'), 'ok');
  else fail(t('The address could not be copied. Select it and copy it by hand.'));
};

$('#cloud-connect').onclick = event => busy(event.currentTarget, async () => {
  const endpoint = $('#cloud-endpoint').value.trim();
  if (!/^https:\/\/[^/?#\s]+\.[^/?#\s]+\/?$/.test(endpoint)) throw t('Give the account store address, like https://store.example.com.');
  const id = $('#cloud-client-id').value.trim();
  const secret = $('#cloud-client-secret').value.trim();
  if (Boolean(id) !== Boolean(secret)) throw t('Give both the client ID and the client secret, or neither.');
  const args = { endpoint };
  const label = $('#cloud-label').value.trim();
  if (label) args.label = label;
  if (id) { args.client_id = id; args.client_secret = secret; }
  const expires = $('#cloud-expires').value;
  if (expires && !$('#cloud-expires-field').hidden) args.token_expires = expires;
  try {
    const reply = await invoke('cloud_connect', args);
    setCloudStatus(reply.status ?? cloudStatus);
  } finally {
    $('#cloud-client-secret').value = '';
  }
  $('#cloud-client-id').value = '';
  renderCloud();
  done(t('This computer is connected to the account store. Nothing was uploaded.'));
});

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
  account_store: ['panel-cloud', () => t('Encrypted copy in the account store')],
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
    setReadiness(report);
  } catch {
    if (epoch !== accountEpoch) return;
    setReadiness(null);
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
  return Boolean(report) && !transport.browser && report.local_owner
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
  chip.hidden = !report || transport.browser || !report.local_owner || report.mode === 'portal_account'
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
    const [state, stateTone] = sync?.mode === 'replica' ? chipState(sync.state)
      : sync?.mode === 'account_store' ? [t('Configuration from the account store'), 'ok'] : ['', 'ok'];
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

export { accountEpoch, leaveAccount, adoptAccount, loadAccountConfig, loadAgentEndpoint, loadReadiness };
