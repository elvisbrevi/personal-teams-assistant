// Settings › Advanced › updates of the installed version.
import { t, tx } from './i18n.js';
import { invoke } from './control.js';
import { $, el } from './dom.js';
import { current } from './state.js';
import { busy } from './notices.js';

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
    const mb = bytes => (bytes / 1048576).toFixed(1);
    const received = stage?.progress?.received ?? 0, total = stage?.progress?.total;
    $('#update-progress').textContent = stage?.phase === 'verifying'
      ? t('Verifying the new application and pta pair…')
      : stage?.phase === 'downloading'
        ? total
          ? t('Downloading version {version} ({received} of {total} MB)…', { version: stage?.version ?? '', received: mb(received), total: mb(total) })
          : t('Downloading version {version}…', { version: stage?.version ?? '' })
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
      : u.install?.kind === 'archive'
        ? t('Version {available} is available; this computer runs {running}. Preparing it downloads its release archive, checks it and puts it next to the installed one without changing anything.', { available: u.available, running: u.running })
        : t('Version {available} is available; this computer runs {running}. Preparing it builds it next to the installed one without changing anything.', { available: u.available, running: u.running });
    $('#update-offer-stage').hidden = prepared;
    $('#update-offer-apply').hidden = !canApply;
    $('#update-offer-apply').textContent = updateInstallArmed ? t('Tap again to install and restart') : t('Install update');
  }
  $('#update-offer-stage').disabled = !u.can_stage;
  renderUnattended(u, managed);
}

/** The per-computer choice of unattended updating: only on this computer's own page. */
function renderUnattended(u, managed) {
  const choice = u.unattended;
  const shown = !managed && Boolean(choice) && (choice.available || choice.enabled);
  $('#update-unattended').hidden = !shown;
  if (!shown) return;
  $('#update-unattended-toggle').checked = Boolean(choice.enabled);
  $('#update-unattended-toggle').disabled = !choice.available && !choice.enabled;
  const skipped = choice.skipped;
  const note = choice.waiting ? sentences(choice.waiting)
    : skipped?.reason === 'failed' ? t('Version {version} is not installed by itself again: its installation was rolled back. Install it yourself, or wait for a newer release.', { version: skipped.version })
      : skipped?.reason === 'cancelled' ? t('Version {version} is not installed by itself because you cancelled its wait. Install it yourself, or wait for a newer release.', { version: skipped.version })
        : '';
  $('#update-unattended-note').hidden = !note || !choice.enabled;
  $('#update-unattended-note').textContent = note;
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
$('#update-unattended-toggle').onchange = event => {
  const toggle = /** @type {HTMLInputElement} */ (event.currentTarget);
  busy(toggle, async () => {
    updates = await invoke('crate_update_unattended', { enabled: toggle.checked });
  }).finally(() => loadUpdates());
};
$('#update-offer-dismiss').onclick = e => updating(e, dismissUpdate);
setTimeout(loadUpdates, 1500);
setInterval(() => { if (current) loadUpdates(); }, 5 * 60_000);
setInterval(() => { if (['staging', 'applying', 'deferred'].includes(updates?.state)) loadUpdates(); }, 3_000);

export { sentences, loadUpdates };
