// The GUI has one command contract in Tauri, the owner's local page and the portal. Session
// tokens stay in HttpOnly cookies; only the anti-CSRF token is available to JavaScript.
// `browser` means the portal: the person's device is not the host's computer. The local page
// (`local`) is this computer's GUI in the system browser, with the window's behavior.
(() => {
  const native = window.__TAURI__?.core?.invoke;
  if (native) {
    window.ptaTransport = { browser: false, local: false, invoke: native };
    return;
  }
  const local = document.querySelector('meta[name="pta-transport"]')?.content === 'local';
  let identity;
  async function response(path, options = {}) {
    const reply = await fetch(path, { credentials: 'same-origin', cache: 'no-store', ...options });
    if (reply.status === 401 && local) {
      // Only the host opens a new sign-in (pta app open); the page explains it.
      window.location.replace('/');
      throw new Error('Your session has expired. Sign in again.');
    }
    if (reply.status === 401) {
      // Keep a pending machine approval across a new sign-in.
      const enroll = new URLSearchParams(window.location.search).get('enroll');
      window.location.replace(/^[0-9a-f]{64}$/.test(enroll || '') ? `/login?enroll=${enroll}` : '/login');
      throw new Error('Your session has expired. Sign in again.');
    }
    if (!reply.ok) throw new Error(reply.status === 429
      ? 'Too many requests. Wait a minute and try again.'
      : 'The request could not complete. Refresh the page and check the service.');
    return reply.json();
  }
  const ready = response('/api/session').then(value => { identity = value; return value; });
  async function request(path, value) {
    await ready;
    return response(path, { method: 'POST', headers: {
      'Content-Type': 'application/json', 'X-PTA-CSRF': identity.csrf_token
    }, body: JSON.stringify(value) });
  }
  window.ptaTransport = {
    browser: !local, local, ready, request,
    invoke: (_command, { request: value }) => request('/api/control', value),
  };
})();
