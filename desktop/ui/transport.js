// The GUI has one command contract on the owner's local page and in the portal. Session tokens
// stay in HttpOnly cookies; only the anti-CSRF token is available to JavaScript. `browser` means
// the portal: the person's device is not the host's computer. The local page (`local`) is this
// computer's GUI in the system browser, with the owner's authority.
(() => {
  const local = document.querySelector('meta[name="pta-transport"]')?.content === 'local';
  // The local page's session key arrives once, in the fragment of the address the sign-in page moves to: kept in
  // this origin's sessionStorage (another server on 127.0.0.1 gets the cookie, never this) and
  // removed from the address. A new tab has no key and asks for a new sign-in.
  let key = null;
  if (local) {
    const given = /^#k=([0-9a-f]{64})$/.exec(window.location.hash)?.[1];
    if (given) window.history.replaceState(null, '', window.location.pathname + window.location.search);
    try {
      if (given) sessionStorage.setItem('pta-local-key', given);
      key = sessionStorage.getItem('pta-local-key') || given || null;
    } catch { key = given || null; }
  }
  let identity;
  async function response(path, options = {}) {
    const headers = { ...(options.headers || {}), ...(key ? { 'X-PTA-Key': key } : {}) };
    const reply = await fetch(path, { credentials: 'same-origin', cache: 'no-store', ...options, headers });
    if (reply.status === 401 && local) {
      // Only the host opens a new sign-in (pta app open); this page explains it.
      window.location.replace('/signed-out');
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
