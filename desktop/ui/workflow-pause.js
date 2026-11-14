// Workflows › Runs: the pauses of the workflow runs above the list. Each pause in force says its
// scope (every project or one), what it did to the sessions running then and since when, with
// its unpause (`workflow_runs_unpause`); the form pauses every project or one, with the person's
// explicit choice for the sessions running now (`workflow_runs_pause`). Each acts on a second
// press, and `reload` reads the list again.
import { t, tn, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, el } from './dom.js';
import { current } from './state.js';
import { busy, done, notify } from './notices.js';

/** A button that acts only on a second press, saying so in between. */
function twice(label, confirm, act) {
  const button = el('button', { type: 'button', class: 'secondary', textContent: label });
  let armed = false;
  button.onclick = () => busy(button, async () => {
    if (!armed) { armed = true; button.textContent = confirm; return; }
    try { await act(); } finally { armed = false; button.textContent = label; }
  });
  return button;
}
/** Unix seconds in the device's locale. */
function when(seconds) {
  return new Date(seconds * 1000).toLocaleString(uiLocale());
}
/** The account's workflows as the profile holds them now. */
function workflowsNow() {
  return current?.map?.workflows ?? [];
}

/** The pauses in force, each with its unpause; the pause control stays below them. */
function renderPauses(pauses, reload) {
  $('#runs-pauses').replaceChildren(...pauses.map(pause => {
    const scope = pause.project ? t('The runs of project {project} are paused.', { project: pause.project }) : t('Every workflow run is paused.');
    const sessions = pause.sessions === 'stop' ? t('The sessions running then were stopped.') : t('The sessions running then were left to finish.');
    const lift = twice(t('Unpause'), t('Confirm unpause'), async () => {
      await invoke('workflow_runs_unpause', { ...(pause.project ? { project: pause.project } : {}), confirmed: true });
      done(t('Unpaused; the runs continue in their order.'));
      await reload();
    });
    return el('div', { class: 'callout warn run-pause', 'data-project': pause.project ?? '' },
      el('p', {}, el('strong', { textContent: scope }), ' ', sessions, ' ', t('Nothing new starts until you unpause, and the runs keep their order.'), ' ', el('small', { textContent: t('Since {time}', { time: when(pause.paused_at) }) })),
      el('div', { class: 'actions' }, lift));
  }));
}
/** The pause form: every project or one, and what happens to the sessions running now, chosen explicitly. */
function showPause(reload) {
  const form = $('#runs-pause-form');
  const projects = [...new Set(workflowsNow().map(w => w.project))].sort((a, b) => a.localeCompare(b));
  const scope = el('select', { id: 'runs-pause-scope' }, el('option', { value: '', textContent: t('Every project') }), ...projects.map(p => el('option', { value: p, textContent: p })));
  const pause = twice(t('Pause'), t('Confirm pause'), async () => {
    const picked = /** @type {HTMLInputElement | null} */ (form.querySelector('input[name="runs-pause-sessions"]:checked'));
    if (!picked) return;
    const reply = await invoke('workflow_runs_pause', { ...(scope.value ? { project: scope.value } : {}), sessions: picked.value, confirmed: true });
    const stopped = reply.sessions_stopped?.length ?? 0;
    if (reply.warning) notify(tx(reply.warning), 'warn');
    else done(stopped ? tn(stopped, 'Paused; 1 session was stopped.', 'Paused; {n} sessions were stopped.') : t('Paused.'));
    form.replaceChildren();
    await reload();
  });
  pause.id = 'runs-pause-confirm';
  // No default: the person chooses what happens to the sessions running now.
  pause.disabled = true;
  const choice = (value, label, detail) => {
    const radio = el('input', { type: 'radio', name: 'runs-pause-sessions', value });
    radio.onchange = () => { pause.disabled = false; };
    return el('label', { class: 'check' }, radio, el('span', {}, el('strong', { textContent: label }), ' ', el('small', { class: 'hint', textContent: detail })));
  };
  const close = el('button', { type: 'button', class: 'link-button', textContent: t('Close') });
  close.onclick = () => form.replaceChildren();
  form.replaceChildren(el('div', { class: 'panel', id: 'runs-pause-panel' },
    el('h3', { textContent: t('Pause workflow runs') }),
    el('p', { class: 'hint', textContent: t('While paused, no run takes a slot, asks the director, starts a planning or coding run or runs a block, and the watcher starts no run. What already runs is still followed.') }),
    el('label', {}, t('Runs'), scope),
    el('fieldset', {}, el('legend', { textContent: t('The sessions running now') }),
      choice('stop', t('Stop them now'), t('Their runs stop as interrupted, to restart with a note or resume. The file changes stay.')),
      choice('finish', t('Let them finish'), t('Their runs record how they end, and nothing new starts.'))),
    el('div', { class: 'actions' }, pause, close)));
  scope.focus();
}
export { renderPauses, showPause };
