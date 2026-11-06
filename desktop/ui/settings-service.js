// Settings › Advanced › Start at login: this computer's service-manager entry for the host.
import { t } from './i18n.js';
import { invoke } from './control.js';
import { $, el } from './dom.js';
import { busy } from './notices.js';
import { sentences } from './settings-updates.js';

// ---------------------------------------------------------------- start at login

/** This computer's service-manager entry for the host; null where the host does not offer it. */
let loginService = null;

async function loadService() {
  try { loginService = await invoke('service_status'); } catch { loginService = null; }
  renderService();
}

const SERVICE_MANAGERS = { launchd: 'macOS LaunchAgent', systemd: 'systemd user unit', task_scheduler: 'Windows scheduled task' };

function renderService() {
  const s = loginService;
  $('#panel-service').hidden = !s;
  if (!s) return;
  const installed = Boolean(s.installed);
  const state = !s.supported ? t('Not available') : installed ? (s.running ? t('Running') : t('On')) : t('Off');
  $('#service-state').textContent = state;
  $('#service-state').className = `badge ${installed ? 'ok' : 'muted'}`;
  const rows = s.supported ? [
    [t('Service manager'), t(SERVICE_MANAGERS[s.manager] ?? s.manager)],
    [t('Entry'), s.name],
  ] : [];
  $('#service-rows').replaceChildren(...rows.map(([label, value]) => el('div', { class: 'row' },
    el('span', { textContent: label }), el('code', { translate: 'no', textContent: value }))));
  $('#service-error').hidden = !s.last_error;
  $('#service-error').textContent = s.last_error ? sentences(s.last_error) : '';
  $('#service-notes').replaceChildren(...(s.notes ?? []).map(note => el('li', { textContent: sentences(note) })));
  $('#service-install').hidden = !s.supported || (installed && s.current);
  $('#service-install').textContent = installed ? t('Update the entry') : t('Start at login');
  $('#service-uninstall').hidden = !installed;
}

async function changeService(method) {
  try {
    loginService = await invoke(method);
  } finally {
    await loadService();
  }
}
$('#service-install').onclick = e => busy(e.currentTarget, () => changeService('service_install')).then(renderService);
$('#service-uninstall').onclick = e => busy(e.currentTarget, () => changeService('service_uninstall')).then(renderService);

export { loadService };
