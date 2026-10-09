// The GUI has one command contract in both Tauri and the browser. Session tokens
// stay in HttpOnly cookies; only the anti-CSRF token is available to JavaScript.
(() => {
  const native = window.__TAURI__?.core?.invoke;
  if (native) {
    window.ptaTransport = { browser: false, invoke: native };
    return;
  }
  let identity;
  async function response(path, options = {}) {
    const reply = await fetch(path, { credentials: 'same-origin', cache: 'no-store', ...options });
    if (reply.status === 401) {
      window.location.replace('/login');
      throw new Error('Your session has expired. Sign in again.');
    }
    if (!reply.ok) throw new Error(path === '/api/password' && reply.status === 400
      ? 'Check your current password and use at least 12 characters for the new password.'
      : reply.status === 429
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
    browser: true, ready, request,
    invoke: (_command, { request: value }) => request('/api/control', value),
  };
})();
