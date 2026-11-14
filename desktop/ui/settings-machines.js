// Settings › Machines: this computer, the account's machines and the computer's sign-in.
import { transport } from './transport.js';
import { t, tx, uiLocale } from './i18n.js';
import { invoke, rawInvoke } from './control.js';
import { $, badge, el, field, sleep } from './dom.js';
import { live, tab } from './state.js';
import { busy, done, fail, notify } from './notices.js';
import { copy } from './markdown.js';
import { implementationForm, implementationRuns } from './implementation.js';
import { chooseDirectories } from './directory-sheet.js';
import { accountEpoch, leaveAccount } from './settings-account.js';

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
/** What a machine last reported about its version and updates; older hosts report nothing. */
function updateSummary(machine) {
  const version = machine.version ? t('Version {version}', { version: machine.version }) : t('Version unknown');
  const update = machine.update;
  if (!update) return version;
  const parts = [version];
  if (update.failed) parts.push(t('update {version} failed and was rolled back', { version: update.failed }));
  else if (update.state === 'applying') parts.push(t('installing an update'));
  else if (update.state === 'staging') parts.push(t('preparing an update'));
  else if (update.state === 'deferred' && update.pending) parts.push(t('update {version} waits for work in progress', { version: update.pending }));
  else if (update.pending) parts.push(t('update {version} pending', { version: update.pending }));
  parts.push(update.unattended ? t('updates itself') : t('updates when asked'));
  return parts.join(' · ');
}
/** @param {string | null | undefined} a @param {string | null | undefined} b */
function newer(a, b) {
  const parse = (/** @type {string | null | undefined} */ v) => (v || '').split('.').map(Number);
  const [x, y] = [parse(a), parse(b)];
  if (x.length !== 3 || y.length !== 3 || [...x, ...y].some(Number.isNaN)) return false;
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] > y[i];
  return false;
}
/** The newest release any machine of the account runs or was offered: what an older one can reach. */
function newestKnown(machines) {
  let newest = null;
  for (const machine of machines) for (const version of [machine.version, machine.update?.pending]) {
    if (version && (!newest || newer(version, newest))) newest = version;
  }
  return newest;
}
/** Update the machine to the newest release: a second press confirms, the machine does the rest. */
function upgradeButton(machine, newest) {
  const state = machine.update?.state;
  const behind = Boolean(machine.update?.pending) || newer(newest, machine.version);
  if (!machine.capabilities?.upgrade || machine.trust === 'revoked' || !behind || state === 'applying') return null;
  const target = machine.update?.pending || newest;
  const button = el('button', { type: 'button', class: 'secondary small', textContent: target ? t('Update to {version}', { version: target }) : t('Update'),
    'aria-label': target ? t('Update to {version}: {machine}', { version: target, machine: machine.name }) : t('Update: {machine}', { machine: machine.name }),
    disabled: !machine.online || state === 'staging' || state === 'deferred' });
  let confirmed = false;
  button.onclick = () => busy(button, async () => {
    if (!confirmed) {
      confirmed = true; button.textContent = t('Confirm update');
      button.setAttribute('aria-label', t('Confirm update: {machine}', { machine: machine.name }));
      return;
    }
    for (const other of document.querySelectorAll('.machine-choice button')) other.disabled = true;
    try {
      const result = await invoke('machine_upgrade', { machine: machine.id });
      const name = machine.name;
      if (result?.waiting_for_archive) {
        done(t('{machine} waits for the archive of version {version}; it is prepared and installed by itself when the archive appears.', { machine: name, version: result.target }));
        return;
      }
      done({
        current: t('{machine} already runs the newest release.', { machine: name }),
        staging: t('Preparing the update on {machine}; it installs itself when ready and restarts.', { machine: name }),
        deferred: t('The update on {machine} waits for work in progress to finish.', { machine: name }),
      }[result?.outcome] || t('Installing the update on {machine}; it restarts.', { machine: name }));
    } finally { renderTeamsMachines(await invoke('machine_list')); }
  });
  return button;
}
function renderTeamsMachines(fleet) {
  const newest = newestKnown(fleet.machines);
  for (const id of ['machine-list']) {
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
        el('span', { class: 'hint', textContent: machine.online ? phaseText : t('Last reported assistant state: {state}', { state: phaseText }) }),
        el('span', { class: 'hint', textContent: updateSummary(machine) }), el('div', { class: 'machine-actions' }, control, ...[upgradeButton(machine, newest)].filter(Boolean), manage));
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
      el('p', { class: 'hint', textContent: updateSummary(detail) }),
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
  $('#machine-connect-hint').hidden = transport.browser || Boolean(settings.coordinator);
  $('#machine-disconnect').disabled = !settings.coordinator;
  renderTeamsMachines(fleet);
  await loadJoinCommand();
}

/** Settings › Machines › Connect this computer (portal only): the portal's own install command.
 *  @type {{commands?: Record<string, string>} | null} */
let joinCommands = null;
function visitorSystem() {
  const platform = /** @type {any} */ (navigator).userAgentData?.platform || navigator.userAgent;
  return /win/i.test(platform) ? 'windows' : /mac|iphone|ipad/i.test(platform) ? 'macos' : 'linux';
}
/** @param {string} system */
function showJoinCommand(system) {
  for (const button of document.querySelectorAll('#machine-join-system button')) {
    button.setAttribute('aria-pressed', String(/** @type {HTMLElement} */ (button).dataset.system === system));
  }
  $('#machine-join-command').textContent = joinCommands?.commands?.[system] ?? '';
}
async function loadJoinCommand() {
  joinCommands = transport.browser ? await invoke('machine_join_command').catch(() => null) : null;
  $('#machine-join').hidden = !joinCommands?.commands;
  if (!joinCommands?.commands) return;
  const pressed = /** @type {HTMLElement | null} */ (document.querySelector('#machine-join-system [aria-pressed="true"]'));
  showJoinCommand($('#machine-join').dataset.chosen && pressed?.dataset.system ? pressed.dataset.system : visitorSystem());
}
for (const button of document.querySelectorAll('#machine-join-system button')) {
  /** @type {HTMLElement} */ (button).onclick = () => {
    $('#machine-join').dataset.chosen = 'true';
    showJoinCommand(/** @type {HTMLElement} */ (button).dataset.system ?? 'linux');
  };
}
$('#machine-join-copy').onclick = async () => {
  const copied = await copy($('#machine-join-command').textContent ?? '');
  if (copied) notify(t('Command copied. Paste it into a terminal on the computer to add.'), 'ok');
  else fail(t('The command could not be copied. Select it and copy it by hand.'));
};
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
        const store = result.data.store;
        const joined = result.data.username ? t('This computer is connected to {account}.', { account: result.data.username }) : t('This computer is connected to your account.');
        done(!store ? joined
          : store.installed && store.connected ? `${joined} ${t('It also received the account store and is connected to it; nothing was uploaded.')}`
          : `${joined} ${t('The account store was not set up here: {problem}', { problem: tx(store.problem || '') })}`);
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

/** @param {any} value */
function setMachineRoots(value) { machineRoots = value; }

export { machineRoots, renderMachineRoots, loadMachines, loadJoinCommand, machineLogin, setMachineRoots };
