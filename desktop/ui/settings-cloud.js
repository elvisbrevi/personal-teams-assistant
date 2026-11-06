// Settings › Advanced › Cloud storage: the account store link.
import { t, tn, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el, rowText } from './dom.js';
import { busy, done } from './notices.js';
import { ago } from './messages.js';

// ---------------------------------------------------------------- account store link

/** This computer's link to the account store (never credentials). */
let cloudStatus = null;

const CLOUD_STATES = {
  live: ['Linked', 'ok'], offline: ['Offline', 'warn'], revoked: ['Credentials refused', 'error'],
  unconfigured: ['Store not set up', 'warn'], not_enrolled: ['Not connected yet', 'muted'], disconnected: ['Not connected yet', 'muted'],
};

/** The account's computers in the account store, read while this one is live there. */
let cloudInstallations = [];
/** The installation whose revocation waits for a second tap. */
let cloudRevokeArmed = null;
/** The installation whose «Lost or stolen» waits for a second tap. */
let cloudLostArmed = null;
/** The service token and, on the coordinator, which computers hold the current one. */
let cloudToken = null;
let cloudTokenArmed = false;
/** This computer's backups and the last restore; the backup whose restore waits for a second tap. */
let cloudBackups = null;
let cloudRestoreArmed = null;

async function loadCloud() {
  try { cloudStatus = await invoke('cloud_status'); }
  catch { cloudStatus = null; }
  cloudInstallations = [];
  cloudToken = null;
  if (cloudStatus?.standing === 'live') {
    try { cloudInstallations = (await invoke('cloud_installations')).installations ?? []; }
    catch { cloudInstallations = []; }
  }
  if (cloudStatus?.enrolled) {
    try { cloudToken = await invoke('cloud_token'); }
    catch { cloudToken = null; }
  }
  cloudConflicts = [];
  if (cloudStatus?.mode === 'primary' && cloudStatus?.mirror?.primary?.conflicts) {
    try { cloudConflicts = (await invoke('cloud_conflicts')).conflicts ?? []; }
    catch { cloudConflicts = []; }
  }
  cloudBackups = null;
  if (cloudStatus && cloudStatus.enabled !== false) {
    try { cloudBackups = await invoke('cloud_backups'); }
    catch { cloudBackups = null; }
  }
  renderCloud();
}

const CLOUD_TOKEN_STATES = {
  current: ['Has the current token', 'ok'], previous: ['Waiting for the new token', 'warn'],
  none: ['Waiting for the new token', 'warn'], unknown: ['Not reported yet', 'muted'],
};

/** Replacing the service token: which computers hold it, and the approved launcher that does it. */
function renderCloudToken() {
  const token = cloudToken;
  const box = $('#cloud-token');
  box.hidden = !token;
  if (!token) return;
  const lines = [];
  if (token.coordinator === false) {
    const delivery = token.delivery ?? {};
    lines.push(delivery.receives
      ? t('When the coordinator replaces the service token or rotates the content key, this computer receives them automatically.')
      : t('This computer was trusted before it could receive the account store\'s new credentials: trust it again with the account store.'));
    if (delivery.installed_at) lines.push(t('Last received {when}.', { when: ago(delivery.installed_at * 1000) }));
    if (delivery.problem) lines.push(tx(delivery.problem));
  } else {
    const launcher = token.replace?.launcher ?? {};
    lines.push(token.replace?.where === 'operator'
      ? t('The portal operator replaces this account\'s service token; your computers then receive it automatically.')
      : launcher.approved
        ? t('Replacing the service token rotates its secret in Cloudflare Access: the previous one stops working at once, and the account\'s other computers receive the new one automatically.')
        : t('To replace the service token, define and approve the launcher cloud-token-rotate (configure-cloud-access.py production --rotate) on this computer.'));
    if (token.computers?.length) {
      lines.push(token.pending
        ? tn(token.pending, '{n} computer does not have the current token or content key yet.', '{n} computers do not have the current token or content key yet.')
        : t('Every computer of the account has the current token and content key.'));
    }
  }
  $('#cloud-token-summary').textContent = lines.join(' ');
  $('#cloud-token-list').replaceChildren(...(token.computers ?? []).map(entry => {
    const [label, tone] = !entry.receives ? ['Trust it again with the account store', 'warn']
      : entry.token === 'current' && entry.key === 'previous' ? ['Waiting for the new content key', 'warn']
      : CLOUD_TOKEN_STATES[entry.token] ?? CLOUD_TOKEN_STATES.unknown;
    const seen = entry.reported_at ? t('Reported {when}.', { when: ago(entry.reported_at * 1000) }) : t('It has not asked for the account store\'s credentials yet.');
    return el('div', { class: 'row variable-row' },
      rowText(el('span', { class: 'cred-name', textContent: entry.name, translate: 'no' }), seen, badge(t(label), tone)));
  }));
  // The recorded expiry is this computer's own reminder; a portal account's comes from its operator.
  const expiry = !cloudStatus?.granted_endpoint && cloudStatus?.standing === 'live';
  $('#cloud-expiry').hidden = !expiry;
  $('#cloud-expiry-save').hidden = !expiry;
  const date = $('#cloud-expiry-date');
  if (expiry && document.activeElement !== date) {
    const at = cloudStatus?.service_token?.expires_at;
    date.value = at ? new Date(at * 1000).toISOString().slice(0, 10) : '';
  }
  const rotate = $('#cloud-token-rotate');
  rotate.hidden = !(token.replace?.runs_here && token.replace?.launcher?.approved);
  rotate.textContent = cloudTokenArmed ? t('Tap again to replace the token') : t('Replace the service token');
}

const BACKUP_REASONS = { migrate: 'Before migrating', promote: 'Before making the store the source', rollback: 'Before bringing everything back' };

/** Backups are listed by time and reason; restoring restarts the application. */
function renderCloudBackups() {
  const data = cloudBackups;
  const box = $('#cloud-backups');
  box.hidden = !data || (!data.backups?.length && !data.last_restore);
  if (box.hidden) return;
  const lines = [];
  const outcome = data.last_restore;
  if (data.pending) lines.push(t('A restore is waiting for the application to restart.'));
  else if (outcome?.ok) lines.push(t('Last restore {when}: {n} files put back; the data from just before is kept in {copy}.', { when: ago(outcome.finished_at * 1000), n: outcome.files, copy: outcome.before }));
  else if (outcome) lines.push(t('The last restore did not complete: {problem}', { problem: tx(outcome.error) }));
  if (data.backups?.length && !data.restores_here) lines.push(t('Restore from this computer\'s own local page or with pta cloud restore NAME --confirmed.'));
  $('#cloud-backups-summary').textContent = lines.join(' ');
  $('#cloud-backups-summary').hidden = !lines.length;
  $('#cloud-backup-list').replaceChildren(...(data.backups ?? []).map(entry => {
    const when = entry.created_at ? new Date(entry.created_at * 1000).toLocaleString(uiLocale()) : entry.name;
    const detail = entry.restorable
      ? t('{reason}, version {version}, {size} KB.', { reason: t(BACKUP_REASONS[entry.reason] ?? 'Backup'), version: entry.host_version, size: Math.max(1, Math.round(entry.bytes / 1024)) })
      : tx(entry.problem ?? '');
    const actions = [];
    if (entry.restorable && data.restores_here && !data.pending) {
      const armed = cloudRestoreArmed === entry.name;
      const restore = el('button', { type: 'button', class: 'secondary', textContent: armed ? t('Tap again to restore') : t('Restore'), 'aria-label': t('Restore the backup of {when}', { when }) });
      restore.onclick = () => {
        if (!armed) {
          cloudRestoreArmed = entry.name;
          renderCloudBackups();
          cloudDisarmLater(`restore:${entry.name}`, () => { if (cloudRestoreArmed === entry.name) { cloudRestoreArmed = null; renderCloudBackups(); } });
          return;
        }
        cloudRestoreArmed = null;
        busy(restore, async () => {
          await invoke('cloud_restore', { backup: entry.name });
          done(t('The application is restarting to restore the backup. The data from just before is kept.'));
        });
      };
      actions.push(restore);
    }
    return el('div', { class: 'row variable-row' },
      rowText(el('span', { class: 'cred-name', textContent: when }), detail, entry.restorable ? null : badge(t('Cannot be restored'), 'warn')),
      actions.length ? el('div', { class: 'variable-actions' }, actions) : null);
  }));
}

$('#cloud-token-rotate').onclick = event => {
  if (!cloudTokenArmed) {
    cloudTokenArmed = true;
    renderCloudToken();
    cloudDisarmLater('#cloud-token-rotate', () => { cloudTokenArmed = false; renderCloudToken(); });
    return;
  }
  cloudTokenArmed = false;
  busy(event.currentTarget, async () => {
    const reply = await invoke('variable_launcher_run', { id: cloudToken.replace.launcher.id });
    await loadCloud();
    if (reply.result?.status !== 'succeeded') throw new Error(t('The launcher did not finish; nothing reports a new token yet.'));
    done(t('The service token was replaced. The account\'s other computers receive it within minutes.'));
  });
};

$('#cloud-expiry-save').onclick = event => busy(event.currentTarget, async () => {
  const date = $('#cloud-expiry-date').value;
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) throw t('Give the date the service token expires, as Cloudflare Access shows it.');
  // Connecting again with the same token only records the new date.
  const reply = await invoke('cloud_connect', { endpoint: cloudStatus.endpoint, token_expires: date });
  await loadCloud();
  done(reply.changed ? t('The service token\'s expiry date was saved.') : t('That date was already saved.'));
});

/** Plain text only: labels are whatever each computer called itself. */
function renderCloudInstallations() {
  const list = $('#cloud-installation-list');
  list.replaceChildren(...cloudInstallations.map(entry => {
    const state = entry.current ? badge(t('This computer'), 'ok') : entry.revoked_at ? badge(t('Access revoked'), 'muted') : badge(t('Connected'), 'ok');
    const seen = entry.revoked_at
      ? t('Revoked {when}.', { when: ago(entry.revoked_at * 1000) })
      : t('Version {version}, connected {enrolled}, last seen {seen}.', { version: entry.host_version, enrolled: ago(entry.enrolled_at * 1000), seen: ago(entry.last_seen_at * 1000) });
    const actions = [];
    if (!entry.current && !entry.revoked_at) {
      const armed = cloudRevokeArmed === entry.id;
      const revoke = el('button', { type: 'button', class: 'danger', textContent: armed ? t('Tap again to revoke') : t('Revoke'), 'aria-label': t('Revoke {name}', { name: entry.label }) });
      revoke.onclick = () => {
        if (!armed) {
          cloudRevokeArmed = entry.id;
          renderCloudInstallations();
          cloudDisarmLater(`revoke:${entry.id}`, () => { if (cloudRevokeArmed === entry.id) { cloudRevokeArmed = null; renderCloudInstallations(); } });
          return;
        }
        cloudRevokeArmed = null;
        busy(revoke, async () => {
          await invoke('cloud_revoke', { installation: entry.id });
          await loadCloud();
          done(t('{name} can no longer reach the account store. It keeps its own data.', { name: entry.label }));
        });
      };
      actions.push(revoke);
    }
    // Only the coordinator secures the account after a lost computer, revoked or not yet.
    if (!entry.current && cloudToken?.coordinator) {
      const armed = cloudLostArmed === entry.id;
      const lost = el('button', { type: 'button', class: 'secondary', textContent: armed ? t('Tap again to secure the account') : t('Lost or stolen'), 'aria-label': t('Lost or stolen: {name}', { name: entry.label }) });
      lost.onclick = () => {
        if (!armed) {
          cloudLostArmed = entry.id;
          renderCloudInstallations();
          cloudDisarmLater(`lost:${entry.id}`, () => { if (cloudLostArmed === entry.id) { cloudLostArmed = null; renderCloudInstallations(); } });
          return;
        }
        cloudLostArmed = null;
        busy(lost, () => cloudLost({ installation: entry.id }));
      };
      actions.push(lost);
    }
    return el('div', { class: 'row variable-row' },
      rowText(el('span', { class: 'cred-name', textContent: entry.label, translate: 'no' }), seen, state),
      actions.length ? el('div', { class: 'variable-actions' }, actions) : null);
  }));
  $('#cloud-installations').hidden = cloudInstallations.length < 2;
}

const CLOUD_LOST_STEPS = {
  machine: () => t('Revoked on this coordinator'),
  store: () => t('Revoked in the account store'),
  token: () => t('Service token replaced'),
  key: () => t('Content key rotated'),
};
const CLOUD_LOST_STATES = { done: ['Done', 'ok'], not_needed: ['Not needed', 'muted'], pending: ['Waiting for your action', 'warn'], failed: ['Failed', 'warn'] };

/** Secure the account after a lost computer; repeating it for the same computer continues. */
async function cloudLost(target) {
  const reply = await invoke('cloud_lost', target);
  await loadCloud();
  if (!reply.complete) throw t('Some steps are not done yet: see Lost computer.');
  done(t('{name} can no longer use this account: the service token was replaced and the content key rotated. Back up the new key now with pta variables key backup.', { name: reply.name }));
}

/** The last lost computer secured from here: each step, and Continue while one is left. */
function renderCloudLost() {
  const record = cloudToken?.lost;
  const box = $('#cloud-lost');
  box.hidden = !record || record.complete;
  if (box.hidden) return;
  $('#cloud-lost-summary').textContent = t('Securing the account after “{name}” was lost. Do what each step that is not done says, then continue: finished steps are not repeated.', { name: record.name });
  $('#cloud-lost-steps').replaceChildren(...(record.steps ?? []).map(step => {
    const [label, tone] = CLOUD_LOST_STATES[step.state] ?? ['Unknown', 'muted'];
    const detail = ['pending', 'failed'].includes(step.state) ? tx(step.detail) : '';
    return el('div', { class: 'row variable-row' },
      rowText(el('span', { class: 'cred-name', textContent: CLOUD_LOST_STEPS[step.step]?.() ?? step.step }), detail, badge(t(label), tone)));
  }));
  const next = $('#cloud-lost-continue');
  next.hidden = false;
  next.onclick = () => busy(next, () => cloudLost(record.installation ? { installation: record.installation } : { machine: record.machine }));
}

function renderCloud() {
  const status = cloudStatus;
  const [label, tone] = CLOUD_STATES[status?.standing] ?? ['Unknown', 'muted'];
  const badge = $('#cloud-state');
  badge.textContent = t(label);
  badge.className = `badge ${tone}`;
  $('#cloud-summary').textContent = !status ? t('The link could not be read.')
    : !status.endpoint ? t('This computer is not connected to an account store.')
    : t('{endpoint}, checked {when}. Mode: {mode}.', { endpoint: status.endpoint, when: ago(status.checked_at * 1000), mode: status.mode === 'off' ? t('nothing is uploaded') : status.mode === 'mirror' ? t('copy kept up to date') : t('the account store is the source') });
  const problem = $('#cloud-problem');
  problem.textContent = status?.problem ? tx(status.problem) : '';
  problem.hidden = !problem.textContent;
  const details = $('#cloud-details');
  details.replaceChildren();
  if (status?.worker) {
    details.append(el('p', { class: 'hint', textContent: t('Account store {version} ({environment}), protocol {protocols}.', { version: status.worker.version, environment: status.worker.environment, protocols: (status.worker.protocols ?? []).join(', ') }) }));
  }
  if (status?.principal) {
    details.append(el('p', { class: 'hint', textContent: tn(status.principal.account_installations, 'This computer is “{label}”, connected {when}; the account has {n} connected computer.', 'This computer is “{label}”, connected {when}; the account has {n} connected computers.', { label: status.principal.label, when: ago(status.principal.enrolled_at * 1000) }) }));
  }
  if (status?.store?.usage) {
    const usage = status.store.usage;
    details.append(el('p', { class: 'hint', textContent: t('Today: {requests} of {limit} requests and {rows} of {rows_limit} records written.', { requests: usage.requests, limit: usage.limits.requests, rows: usage.rows_written, rows_limit: usage.limits.rows_written }) }));
  }
  if (status?.store?.plan) {
    const plan = status.store.plan;
    details.append(el('p', { class: 'hint', textContent: tn(plan.accounts, 'That is this account\'s daily share. The plan allows {requests} requests and {rows} records written a day, shared by {n} account.', 'That is this account\'s daily share. The plan allows {requests} requests and {rows} records written a day, shared by {n} accounts.', { requests: plan.requests, rows: plan.rows_written }) }));
    if (plan.oversubscribed) details.append(el('p', { class: 'callout warn', textContent: t('If every account used its whole share, the plan\'s daily quota would run out first. Ask whoever runs the account store to lower the shares.') }));
  }
  if (status?.service_token?.expires_at) {
    const when = new Date(status.service_token.expires_at * 1000).toLocaleDateString(uiLocale(), { timeZone: 'UTC' });
    details.append(el('p', { class: status.service_token.renew ? 'callout warn' : 'hint', textContent: status.service_token.renew
      ? t('The service token expires on {date}. Ask whoever runs the account store to renew it, then save the new date under Service token.', { date: when })
      : t('The service token expires on {date}.', { date: when }) }));
  }
  if (status?.latency_ms?.store !== undefined) {
    details.append(el('p', { class: 'hint', textContent: t('Round trip to the account store: {ms} ms.', { ms: status.latency_ms.store }) }));
  }
  details.hidden = !details.childElementCount;
  if (status?.endpoint && !$('#cloud-endpoint').value) $('#cloud-endpoint').value = status.endpoint;
  // A portal account connects only to the store its operator enabled, with the operator's token.
  const granted = status?.granted_endpoint;
  // Once this computer is connected and the store answers, connecting again changes nothing: the
  // form stays out of the way until the link breaks (revoked, a refused token, not enrolled).
  $('#cloud-connect-fold').hidden = status?.enabled === false || (status?.enrolled && status?.standing === 'live');
  $('#cloud-connect-own').hidden = Boolean(granted);
  $('#cloud-connect-granted').hidden = !granted;
  $('#cloud-client-id-field').hidden = Boolean(granted);
  $('#cloud-client-secret-field').hidden = Boolean(granted);
  $('#cloud-expires-field').hidden = Boolean(granted);
  if (granted) $('#cloud-endpoint').value = granted;
  $('#cloud-endpoint').readOnly = Boolean(granted);
  renderCloudInstallations();
  renderCloudLost();
  renderCloudToken();
  renderCloudBackups();
  renderCloudMirror();
}

/**
 * How long the cloud panel's first tap stays armed. Its actions back up, upload or reseal the whole
 * account, so the second tap is deliberate and often comes after reading the button again; a
 * short window expired before it on slow pointers and screen readers.
 */
const CLOUD_CONFIRM_MS = 12000;
const cloudDisarmTimers = new Map();
/** Disarms `key` after the confirm window, replacing a pending disarm so a re-arm gets the whole window. */
function cloudDisarmLater(key, disarm) {
  clearTimeout(cloudDisarmTimers.get(key));
  cloudDisarmTimers.delete(key);
  if (disarm) cloudDisarmTimers.set(key, setTimeout(() => { cloudDisarmTimers.delete(key); disarm(); }, CLOUD_CONFIRM_MS));
}

let cloudMigrateArmed = false;
let cloudRollbackArmed = false;
let cloudRollbackLocalArmed = false;
let cloudEraseArmed = false;
let cloudDisconnectArmed = false;
let cloudRotateArmed = false;
let cloudAdoptArmed = false;
let cloudPromoteArmed = false;
let cloudRepairArmed = false;
/** Records changed both here and in the store, with the fields that differ; the choice that waits for a second tap. */
let cloudConflicts = [];
let cloudResolveArmed = null;

function renderCloudMirror() {
  const status = cloudStatus;
  const mirror = status?.mirror ?? {};
  $('#cloud-mirror').hidden = !status?.enrolled;
  const outbox = mirror.outbox;
  const primary = status?.mode === 'primary';
  const conflicts = mirror.primary?.conflicts ?? 0;
  const lines = [];
  if (primary) {
    lines.push(t('The account store is the source of this account\'s configuration: changes are saved there first, and while it does not answer the configuration cannot be changed.'));
    if (conflicts) lines.push(tn(conflicts, '{n} record was changed both here and in the account store; it cannot be edited until you replace it with the account store\'s.', '{n} records were changed both here and in the account store; they cannot be edited until you replace them with the account store\'s.'));
    if (mirror.primary?.last_pull?.at) lines.push(t('Last synchronized {when}.', { when: ago(mirror.primary.last_pull.at * 1000) }));
    if (outbox?.pending) lines.push(tn(outbox.pending, '{n} change waits to be copied.', '{n} changes wait to be copied.'));
  } else if (status?.mode === 'mirror') {
    lines.push(outbox?.pending
      ? tn(outbox.pending, '{n} change waits to be copied.', '{n} changes wait to be copied.')
      : t('The copy is up to date.'));
    if (outbox?.blocked) lines.push(tn(outbox.blocked, '{n} record is too large for the account store and is not copied.', '{n} records are too large for the account store and are not copied.'));
    if (mirror.last_success_at) lines.push(t('Last copied {when}.', { when: ago(mirror.last_success_at * 1000) }));
  } else {
    lines.push(mirror.rolled_back_at ? t('This computer no longer uses the account store; the copy stays there until you erase it.')
      : mirror.erased_at ? t('The account store\'s copy was erased. Migrating again starts a new copy with a new content key.')
      : t('Nothing is copied yet. Migrating backs up this computer first, uploads every record, verifies the copy and then keeps it up to date.'));
  }
  if (mirror.key?.present === false && status?.mode !== 'mirror') lines.push(t('Migrating creates the account content key on this computer. Back it up right after with pta variables key backup.'));
  if (mirror.backups?.latest?.created_at) lines.push(t('Last backup of this computer {when}.', { when: ago(mirror.backups.latest.created_at * 1000) }));
  if (mirror.last_verify) lines.push(mirror.last_verify.ok ? t('Last verification {when}: the copy matches.', { when: ago(mirror.last_verify.at * 1000) }) : t('Last verification {when}: the copy differs.', { when: ago(mirror.last_verify.at * 1000) }));
  const adoptable = status?.adopt === true && status?.mode !== 'mirror' && !primary;
  if (adoptable) lines.push(t('This account already has a copy in the account store from another computer. Adopting backs this computer up, replaces its configuration with the account\'s (its own settings stay) and uploads only its history.'));
  $('#cloud-mirror-summary').textContent = lines.join(' ');
  const migrate = $('#cloud-migrate');
  // Another computer of the account migrated first: this one adopts the account's configuration.
  const adopt = $('#cloud-adopt');
  adopt.hidden = !adoptable;
  adopt.textContent = cloudAdoptArmed ? t('Tap again to adopt and migrate') : t('Adopt the account\'s configuration');
  migrate.hidden = status?.mode === 'mirror' || primary || !adopt.hidden;
  migrate.textContent = cloudMigrateArmed ? t('Tap again to migrate') : t('Migrate to the account store');
  const rollback = $('#cloud-rollback');
  rollback.hidden = status?.mode !== 'mirror' && !primary;
  rollback.textContent = cloudRollbackArmed ? t('Tap again to bring it back') : t('Bring everything back to this computer');
  const keepLocal = $('#cloud-rollback-local');
  keepLocal.hidden = !primary || (!conflicts && status?.standing === 'live');
  keepLocal.textContent = cloudRollbackLocalArmed ? t('Tap again to keep this computer\'s') : t('Bring it back keeping this computer\'s');
  $('#cloud-erase-box').hidden = status?.mode !== 'off' || !mirror.rolled_back_at;
  $('#cloud-erase').textContent = cloudEraseArmed ? t('Tap again to erase the copy') : t('Erase the account store\'s copy');
  const promote = $('#cloud-promote');
  promote.hidden = status?.mode !== 'mirror' || !mirror.initial_verified_at;
  promote.textContent = cloudPromoteArmed ? t('Tap again to make it the source') : t('Make the account store the source');
  renderCloudConflicts();
  $('#cloud-demote').hidden = !primary;
  $('#cloud-verify').hidden = !mirror.key?.present;
  // A verification that found differences is repaired by sending again only what differs.
  const repair = $('#cloud-repair');
  repair.hidden = !(status?.mode === 'mirror' || primary) || mirror.last_verify?.ok !== false;
  repair.textContent = cloudRepairArmed ? t('Tap again to repair the copy') : t('Repair the copy');
  $('#cloud-disconnect').textContent = cloudDisconnectArmed ? t('Tap again to disconnect') : t('Disconnect this computer');
  const rotate = $('#cloud-rotate');
  rotate.hidden = status?.mode !== 'mirror' && !primary;
  rotate.textContent = cloudRotateArmed ? t('Tap again to rotate the key') : t('Rotate the content key');
  const maintenance = $('#cloud-maintenance');
  maintenance.hidden = ![...maintenance.querySelectorAll('.actions button')].some(button => !button.closest('[hidden]'));
}

cloudTwoTaps('#cloud-adopt', () => cloudAdoptArmed, v => { cloudAdoptArmed = v; }, async () => {
  await invoke('cloud_migrate', { action: 'start', adopt: true });
  await loadCloud();
  done(t('This computer adopted the account\'s configuration and keeps its history in the account store.'));
});

// For a lost computer, after revoking it: a new key, and the whole copy sealed again with it.
cloudTwoTaps('#cloud-rotate', () => cloudRotateArmed, v => { cloudRotateArmed = v; }, async () => {
  const reply = await invoke('cloud_rotate_key', {});
  await loadCloud();
  done(tn(reply.resealing, 'A new content key is active; {n} record is being sealed again with it. Back up the key now with pta variables key backup; your other computers trusted with the account store receive it within minutes.', 'A new content key is active; {n} records are being sealed again with it. Back up the key now with pta variables key backup; your other computers trusted with the account store receive it within minutes.'));
});

cloudTwoTaps('#cloud-disconnect', () => cloudDisconnectArmed, v => { cloudDisconnectArmed = v; }, async () => {
  const reply = await invoke('cloud_disconnect', {});
  await loadCloud();
  done(reply.store_notified === false
    ? t('This computer is disconnected. The account store did not answer, so it may still list this computer until another one revokes it. The copy and the key were kept.')
    : t('This computer is disconnected from the account store. The copy and the key were kept.'));
});

const CLOUD_COLLECTIONS = {
  settings: () => t('Settings'), organization: () => t('Organization'), notes: () => t('Verified notes'),
  evaluations: () => t('Answer evaluations'), examples: () => t('Approved examples'), audits: () => t('Messages'),
  activity_snapshots: () => t('Daily task snapshots'), activity_summaries: () => t('Daily summaries'), activity_runs: () => t('Activity runs'),
  implementation_runs: () => t('Implementation runs'), implementation_requests: () => t('Request runs'),
};

function renderCloudVerify(report) {
  const box = $('#cloud-verify-result');
  box.replaceChildren();
  for (const c of report?.collections ?? []) {
    if (c.ok && !c.pending) continue;
    box.append(el('p', { class: 'hint', textContent: c.ok
      ? tn(c.pending, '{name}: {n} change not copied yet.', '{name}: {n} changes not copied yet.', { name: CLOUD_COLLECTIONS[c.name]?.() ?? c.name })
      : t('{name}: {local} here, {remote} in the account store; the copy differs.', { name: CLOUD_COLLECTIONS[c.name]?.() ?? c.name, local: c.local, remote: c.remote }) }));
  }
  box.hidden = !box.childElementCount;
}

/** A two-tap action of the cloud panel: the first tap arms it for CLOUD_CONFIRM_MS. */
function cloudTwoTaps(id, armed, setArmed, run) {
  $(id).onclick = event => {
    if (!armed()) {
      setArmed(true);
      renderCloudMirror();
      cloudDisarmLater(id, () => { setArmed(false); renderCloudMirror(); });
      return;
    }
    setArmed(false);
    cloudDisarmLater(id, null);
    busy(event.currentTarget, run);
  };
}

cloudTwoTaps('#cloud-migrate', () => cloudMigrateArmed, v => { cloudMigrateArmed = v; }, async () => {
  const reply = await invoke('cloud_migrate', { action: 'start' });
  renderCloudVerify(reply.verify);
  await loadCloud();
  if (!reply.complete) throw t('The copy is not complete yet. Migrate again to continue where it stopped.');
  done(reply.key_created
    ? t('The encrypted copy is up to date, and a backup of this computer was kept first. Back up the new account content key now with pta variables key backup.')
    : t('The encrypted copy is up to date, and a backup of this computer was kept first.'));
});

async function cloudRollback(keepLocal) {
  const reply = await invoke('cloud_rollback', { keep_local: keepLocal });
  renderCloudVerify(reply.verify);
  await loadCloud();
  done(t('This computer is the only source again. Nothing was deleted, and the account store keeps its copy until you erase it.'));
}

cloudTwoTaps('#cloud-rollback', () => cloudRollbackArmed, v => { cloudRollbackArmed = v; }, () => cloudRollback(false));
cloudTwoTaps('#cloud-rollback-local', () => cloudRollbackLocalArmed, v => { cloudRollbackLocalArmed = v; }, () => cloudRollback(true));

cloudTwoTaps('#cloud-erase', () => cloudEraseArmed, v => { cloudEraseArmed = v; }, async () => {
  await invoke('cloud_erase', {});
  await loadCloud();
  done(t('The account store\'s copy was erased. This computer keeps all of its data; migrating again uses a new content key.'));
});

$('#cloud-verify').onclick = event => busy(event.currentTarget, async () => {
  const reply = await invoke('cloud_verify');
  renderCloudVerify(reply.verify);
  await loadCloud();
  if (!reply.ok) throw t('The copy differs from this computer. Repair it to send again what differs.');
  done(t('The copy matches this computer.'));
});

cloudTwoTaps('#cloud-repair', () => cloudRepairArmed, v => { cloudRepairArmed = v; }, async () => {
  const reply = await invoke('cloud_repair', {});
  renderCloudVerify(reply.verify);
  await loadCloud();
  if (!reply.ok) throw tn(reply.requeued, '{n} record was sent again, but the copy still differs.', '{n} records were sent again, but the copy still differs.');
  done(t('The copy matches this computer again.'));
});

cloudTwoTaps('#cloud-promote', () => cloudPromoteArmed, v => { cloudPromoteArmed = v; }, async () => {
  const reply = await invoke('cloud_promote', { action: 'promote' });
  renderCloudVerify(reply.verify);
  await loadCloud();
  done(t('The account store is now the source of this account\'s configuration.'));
});

/** A field's value as plain text: removed, cut short, or as it is. */
function conflictValue(value) {
  if (value === null || value === undefined) return t('(not there)');
  if (value?.truncated) return `${value.preview}…`;
  return typeof value === 'string' ? value : JSON.stringify(value);
}

/** Each conflicting record with its differing fields and the two choices, two taps each. */
function renderCloudConflicts() {
  const box = $('#cloud-conflicts');
  box.hidden = !cloudConflicts.length;
  box.replaceChildren(...cloudConflicts.map(entry => {
    const fields = entry.fields.map(field => el('li', {},
      el('span', { class: 'cred-name', textContent: field.path || t('The whole record'), translate: 'no' }),
      el('span', { class: 'hint', textContent: ` ${t('Here:')} ${conflictValue(field.here)} · ${t('Account store:')} ${conflictValue(field.store)}` })));
    if (entry.more_fields) fields.push(el('li', { class: 'hint', textContent: tn(entry.more_fields, 'And {n} more field.', 'And {n} more fields.') }));
    const choice = (keep, label, armedLabel, message) => {
      const key = `${entry.collection}/${entry.record}/${keep}`;
      const armed = cloudResolveArmed === key;
      const button = el('button', { type: 'button', class: keep === 'store' ? '' : 'secondary', textContent: armed ? t(armedLabel) : t(label), 'aria-label': `${t(label)}: ${tx(entry.label)}` });
      button.onclick = () => {
        if (!armed) {
          cloudResolveArmed = key;
          renderCloudConflicts();
          cloudDisarmLater(`resolve:${key}`, () => { if (cloudResolveArmed === key) { cloudResolveArmed = null; renderCloudConflicts(); } });
          return;
        }
        cloudResolveArmed = null;
        busy(button, async () => {
          await invoke('cloud_resolve', { collection: entry.collection, record: entry.record, keep });
          await loadCloud();
          done(t(message));
        });
      };
      return button;
    };
    return el('div', { class: 'row variable-row' },
      rowText(el('span', { class: 'cred-name', textContent: tx(entry.label) }), tn(entry.fields.length + (entry.more_fields ?? 0), '{n} field differs.', '{n} fields differ.'), badge(t('Conflict'), 'warn')),
      el('ul', { class: 'conflict-fields' }, fields),
      el('div', { class: 'variable-actions' },
        choice('store', 'Keep the account store\'s', 'Tap again to keep the store\'s', 'This computer now has the account store\'s version of that record. The replaced one was kept in a private backup.'),
        choice('here', 'Keep this computer\'s', 'Tap again to keep this computer\'s', 'The account store now has this computer\'s version of that record. Its previous one was kept in a private backup.')));
  }));
}

$('#cloud-demote').onclick = event => busy(event.currentTarget, async () => {
  await invoke('cloud_promote', { action: 'demote' });
  await loadCloud();
  done(t('This computer\'s configuration is the source again; the account store keeps its copy up to date.'));
});

$('#cloud-refresh').onclick = event => busy(event.currentTarget, loadCloud);

/** @param {any} value */
function setCloudStatus(value) { cloudStatus = value; }

export { cloudStatus, loadCloud, renderCloud, setCloudStatus };
