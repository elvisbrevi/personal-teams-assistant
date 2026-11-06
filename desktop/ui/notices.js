// Global notices (`notify`, `done`, `fail`) and the busy state of a button running an action.
import { tx } from './i18n.js';
import { $ } from './dom.js';
import { requestForm } from './todo-request.js';
import { implementationForm, implementationReady } from './implementation.js';
import { renderLive } from './home.js';
import { activityRunning } from './activities.js';

let noticeTimer;
function notify(text, tone = 'info') {
  const box = $('#notice');
  $('#notice-text').textContent = tx(String(text?.message ?? text));
  box.className = `toast ${tone}`;
  // Errors interrupt a screen reader; confirmations and progress wait their turn.
  box.setAttribute('role', tone === 'error' ? 'alert' : 'status');
  box.hidden = false;
  clearTimeout(noticeTimer);
  if (tone !== 'error') noticeTimer = setTimeout(() => { box.hidden = true; }, 6000);
}
const done = text => notify(text, 'ok');
const fail = error => notify(error, 'error');

/** Runs one operation with the button showing progress; errors become a notice. */
async function busy(button, task) {
  if (button) { button.disabled = true; button.classList.add('busy'); }
  try { return await task(); }
  catch (error) { fail(error); }
  finally {
    if (button) { button.disabled = false; button.classList.remove('busy'); }
    renderLive();
    if (button?.id === 'activity-run') button.disabled = activityRunning;
    if (implementationForm) implementationReady(implementationForm);
    if (requestForm) implementationReady(requestForm);
  }
}

export { notify, done, fail, busy };
