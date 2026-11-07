// Native agent sessions: where a run's session opens (app, web or this computer) and the session sheet.
import { transport } from './transport.js';
import { t, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { badge, el, icon } from './dom.js';
import { tab } from './state.js';
import { busy, fail, notify } from './notices.js';
import { copy } from './markdown.js';
import { loadAgentHistory } from './todo-request.js';
import { providerNames } from './home.js';

// ---------------------------------------------------------------- implementation
function sessionDestination(session, mobile, native = false) {
  if (!session.session_id) return { reason: t('The agent has not reported its session ID yet.') };
  if (['codex', 'claude'].includes(session.provider) && !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(session.session_id)) return { reason: t('This session ID does not have a supported app link.') };
  if (mobile && ['codex', 'claude'].includes(session.provider)) return { connect: true };
  if (session.provider === 'codex') return { url: `codex://threads/${encodeURIComponent(session.session_id)}` };
  if (session.provider === 'claude') {
    if (native) return ['running', 'planning', 'generating', 'sending'].includes(session.state) ? { reason: t('Claude Desktop can open this local session after the CLI finishes. This session is still running.') } : { native: true };
    return { connect: true };
  }
  return { reason: t('This agent does not provide a verified app link for the recorded local session. Its session ID is available in implementation runs.') };
}
function sessionIsMobile() {
  return /Android|iPhone|iPad|iPod/i.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
}
function mobileSessionLink(label, url) {
  return el('a', { class: 'button secondary', href: url, target: '_blank', rel: 'noopener noreferrer', textContent: label });
}

function showMobileSession(session, trigger) {
  const dialog = el('dialog', { class: 'directory-picker', 'aria-label': t('Open agent session') });
  const close = el('button', { type: 'button', class: 'icon-button secondary', 'aria-label': t('Close') }, icon('close'));
  const body = el('div', { class: 'directory-picker-body mobile-session-body' });
  const connect = el('button', { type: 'button', class: 'secondary', textContent: t('Connect mobile session') });
  const operation = action => invoke('implementation_mobile_session', { machine: session.machine, id: session.id, action, confirmed: true });
  const render = result => {
    if (!dialog.isConnected) return;
    session.remote = result.remote;
    if (session.provider === 'claude') {
      const url = result.remote?.url;
      if (!/^https:\/\/claude\.ai\/code\/session_[a-zA-Z0-9_]+$/.test(url || '')) throw new Error(t('The provider did not return a supported remote session URL.'));
      const nativeUrl = `claude://code/${encodeURIComponent(url.split('/').pop())}`;
      const stop = el('button', { type: 'button', class: 'secondary', textContent: t('Disconnect mobile session') });
      stop.onclick = () => busy(stop, async () => { await operation('close'); dialog.close(); });
      body.replaceChildren(...[el('p', { class: 'hint', textContent: t('This session is available in your Claude account while the execution machine stays online. The connection lasts up to two hours; reconnect here when needed.') }),
        el('div', { class: 'actions' }, mobileSessionLink(t('Open in app'), nativeUrl), mobileSessionLink(t('Open on web'), url)),
        el('p', { class: 'hint', textContent: t('If the app is missing or does not open, use the web link. Sign in with the same provider account as on the execution machine.') }), ['running', 'planning', 'generating'].includes(session.state) ? null : stop].filter(Boolean));
    } else {
      const pair = el('button', { type: 'button', class: 'secondary', textContent: t('Pair this phone with Codex') });
      const pairing = el('div', { role: 'status' });
      pair.onclick = () => busy(pair, async () => {
        const result = await operation('pair');
        if (!dialog.isConnected) return;
        const value = result.pairing;
        const code = el('code', { textContent: value.manual_code });
        const copyCode = el('button', { type: 'button', class: 'secondary small', textContent: t('Copy pairing code') });
        copyCode.onclick = () => busy(copyCode, async () => { if (!await copy(value.manual_code)) fail(t('Could not copy the pairing code.')); });
        pairing.replaceChildren(el('p', { class: 'hint', textContent: t('In the ChatGPT app, open Codex, choose to add a computer and enter this code. It expires at {time}.', { time: new Date(value.expires_at * 1000).toLocaleTimeString(uiLocale()) }) }), el('div', { class: 'actions' }, code, copyCode));
      });
      const remoteId = result.remote?.session_id || session.session_id;
      const copySession = el('button', { type: 'button', class: 'secondary small', textContent: t('Copy session ID') });
      copySession.onclick = () => busy(copySession, async () => { if (!await copy(remoteId)) fail(t('Could not copy the session ID.')); });
      body.replaceChildren(...[el('p', { textContent: t('Codex Remote Control is connected to {machine}.', { machine: result.remote?.server_name || session.machine_name || session.machine }) }),
        el('p', { class: 'hint', textContent: t('Open Codex in the ChatGPT mobile app, select this computer, then select the session. A direct mobile link to this local session is not available.') }),
        remoteId !== session.session_id ? el('p', { class: 'hint', textContent: t('This older CLI session has a mobile copy with its conversation history. Continue in that copy; the original session is kept.') }) : null,
        el('p', {}, el('strong', { textContent: t('Mobile session ID') }), ' ', el('code', { textContent: remoteId })), copySession,
        el('details', { class: 'fold' }, el('summary', { textContent: t('This computer is not listed in Codex') }), el('p', { class: 'hint', textContent: t('Your phone only needs this once: if the computer already appears in Codex, ignore this.') }), pair, pairing)].filter(Boolean));

    }
  };
  connect.onclick = () => busy(connect, async () => render(await operation('connect')));
  close.onclick = () => dialog.close();
  dialog.onclose = () => { document.body.classList.remove('directory-picker-open'); dialog.remove();
    const focus = trigger.isConnected ? trigger : [...document.querySelectorAll('[data-run-id]')].find(card => card.dataset.runId === session.id)?.querySelector('button');
    focus?.focus({ preventScroll: true }); };
  dialog.append(el('div', { class: 'directory-picker-head' }, el('h2', { textContent: t('Open agent session') }), close), body);
  body.append(el('p', { class: 'hint', textContent: t('Connecting the recorded session through its execution machine…') }), connect);
  document.body.append(dialog); document.body.classList.add('directory-picker-open'); dialog.showModal();
  connect.onclick();
}
function openAgentSession(session, button) {
  const destination = sessionDestination(session, sessionIsMobile(), !transport.browser);
  if (destination.connect) return showMobileSession(session, button);
  if (destination.native) return busy(button, () => invoke('implementation_open_session', { machine: session.machine, id: session.id }));
  if (!destination.url) { notify(destination.reason, 'info'); return; }
  if (!transport.browser) return busy(button, () => invoke('implementation_open_session', { machine: session.machine, id: session.id }));
  window.location.assign(destination.url);
}

let workItemSessionsRequest = 0;
async function loadWorkItemSessions() {
  const buttons = [...document.querySelectorAll('[data-session-for]')];
  const urls = [...new Set(buttons.map(button => button.dataset.sessionFor))].slice(0, 100);
  if (!urls.length) return;
  const request = ++workItemSessionsRequest;
  const result = await invoke('implementation_sessions', { urls });
  if (request !== workItemSessionsRequest) return;
  for (const button of buttons) {
    const sessions = (result.sessions || []).filter(session => session.item.url === button.dataset.sessionFor && session.session_id);
    const session = sessions.find(session => ['running', 'planning', 'generating', 'sending'].includes(session.state)) || sessions[0];
    button.hidden = !session;
    if (session) {
      button.title = t('Open {provider} session', { provider: providerNames[session.provider] || session.provider });
      button.setAttribute('aria-label', button.title);
      button.onclick = () => openAgentSession(session, button);
    }
  }
  for (const section of document.querySelectorAll('[data-sessions-for]')) {
    const sessions = (result.sessions || []).filter(session => session.item.url === section.dataset.sessionsFor);
    section.hidden = !sessions.length;
    section.replaceChildren(el('h2', { textContent: t('Agent sessions') }), ...sessions.map(session => {
      const open = el('button', { type: 'button', class: 'secondary small', textContent: t('Open agent session') });
      open.onclick = () => openAgentSession(session, open);
      return el('article', { class: 'implementation-run', 'data-run-id': session.id }, el('strong', { textContent: `${providerNames[session.provider] || session.provider} · ${session.model}` }),
        badge(session.online === false ? t('Machine offline. Last reported state: {state}', { state: tx(session.state) }) : tx(session.state), session.state === 'done' ? 'ok' : session.state === 'permission_prompt' ? 'warn' : 'info'),
        el('code', { textContent: session.session_id || t('The agent has not reported its session ID yet.') }),
        el('dl', { class: 'todo-fields' }, ...[[t('Started at'), session.started_at], [t('Finished at'), session.finished_at]].map(([label, value]) => el('div', {}, el('dt', { textContent: label }), el('dd', { textContent: value ? new Date(value).toLocaleString(uiLocale()) : t('Not recorded') })))), open);
    }));
  }
}
setInterval(() => { if (tab === 'agent-history') loadAgentHistory(); }, 10000);
setInterval(() => { if (['todo', 'registration'].includes(tab)) loadWorkItemSessions().catch(fail); }, 10000);

export { sessionDestination, sessionIsMobile, showMobileSession, openAgentSession, loadWorkItemSessions };
