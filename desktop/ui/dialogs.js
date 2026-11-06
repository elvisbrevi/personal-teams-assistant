// The browser's Microsoft sign-in and a computer's trust decision.
import { transport } from './transport.js';
import { t, tx } from './i18n.js';
import { rawInvoke } from './control.js';
import { $ } from './dom.js';
import { busy } from './notices.js';

function browserMicrosoftLogin(url) {
  const dialog = $('#web-microsoft-dialog');
  const address = new URL(url);
  if (address.protocol !== 'https:' || address.hostname !== 'login.microsoftonline.com') throw new Error(t('Invalid Microsoft sign-in address.'));
  $('#web-microsoft-link').href = address.href;
  $('#web-microsoft-redirect').value = '';
  $('#web-microsoft-status').textContent = '';
  dialog.showModal();
  return new Promise((resolve, reject) => {
    let cancelled = false;
    const close = () => { $('#web-microsoft-redirect').value = ''; $('#web-microsoft-link').removeAttribute('href'); dialog.close(); };
    const cancel = async event => {
      event?.preventDefault(); cancelled = true;
      await rawInvoke('command', { request: { method: 'cancel_microsoft', args: {}, contract: 1 } }).catch(() => {});
      close(); reject(new Error(t('Microsoft sign-in cancelled.')));
    };
    dialog.oncancel = cancel;
    $('#web-microsoft-cancel').onclick = cancel;
    $('#web-microsoft-form').onsubmit = async event => {
      event.preventDefault();
      const button = $('#web-microsoft-finish');
      button.disabled = true;
      $('#web-microsoft-status').textContent = t('Finishing sign-in…');
      try {
        /** @type {{ redirect_url?: string }} */
        let args = { redirect_url: $('#web-microsoft-redirect').value.trim() };
        $('#web-microsoft-redirect').value = '';
        const deadline = Date.now() + 600_000;
        let result;
        do {
          result = await rawInvoke('command', { request: { method: 'finish_microsoft', args, revision: null, contract: 1 } });
          args = {};
          if (result.code === 'authorization_pending') await new Promise(done => setTimeout(done, 2000));
        } while (!cancelled && result.code === 'authorization_pending' && Date.now() < deadline);
        if (cancelled) return;
        if (!result.ok) throw new Error(result.message || result.code);
        close(); resolve(result);
      } catch (error) { $('#web-microsoft-status').textContent = tx(error.message || String(error)); }
      finally { button.disabled = false; }
    };
  });
}

/** The one explicit trust decision for a computer's pending sign-in, in an authenticated browser session. */
async function machineTrust(transaction) {
  const dialog = $('#machine-trust-dialog'), status = $('#machine-trust-status');
  const request = action => transport.request('/api/enrollment', { action, transaction });
  history.replaceState(null, '', '/');
  const detail = await request('inspect');
  if (!detail.ok) throw new Error(tx(detail.message || detail.code));
  const { machine, code, username, store_available: storeAvailable } = detail.data;
  // The account store goes to this computer only when its owner ticks it here, in this session.
  $('#machine-trust-store-field').hidden = !storeAvailable;
  $('#machine-trust-store').checked = false;
  $('#machine-trust-name').textContent = machine.name;
  $('#machine-trust-os').textContent = { linux: 'Linux', macos: 'macOS', windows: 'Windows' }[machine.os] || machine.os;
  $('#machine-trust-code').textContent = code;
  $('#machine-trust-account').textContent = username;
  status.textContent = '';
  const approve = $('#machine-trust-approve'), deny = $('#machine-trust-deny'), close = $('#machine-trust-close');
  approve.hidden = deny.hidden = false; close.hidden = true;
  const decided = message => {
    status.textContent = message;
    approve.hidden = deny.hidden = true; close.hidden = false; close.focus();
  };
  const decide = (button, action) => busy(button, async () => {
    const result = await transport.request('/api/enrollment', { action, transaction, store: action === 'trust' && storeAvailable && $('#machine-trust-store').checked });
    if (!result.ok) throw new Error(tx(result.message || result.code));
    decided(action === 'trust'
      ? (result.data?.store
        ? t('Trusted, with the account store. Return to {name}: it finishes connecting by itself.', { name: machine.name })
        : t('Trusted. Return to {name}: it finishes connecting by itself.', { name: machine.name }))
      : t('Denied. That computer was not added to your account.'));
  });
  approve.onclick = () => decide(approve, 'trust');
  deny.onclick = () => decide(deny, 'deny');
  close.onclick = () => dialog.close();
  dialog.showModal();
  deny.focus();
}

export { browserMicrosoftLogin, machineTrust };
