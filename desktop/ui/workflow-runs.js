// Workflows › Runs: the runs of the account's workflows, newest first and those waiting on the
// person before the rest, and one run's detail: its stage timeline, children with their coding
// runs, agent sessions and pull requests, the outcomes of its acting blocks, and what it waits
// for. The actions are the host's operations and nothing more: the plan-review gate (the existing
// child review, confirmed with `implementation_create`), assigning a workflow to an unassigned
// run, resume, cancel, restart with a handover note (`workflow_run_restart`), a progress summary
// by the director (`workflow_summary`), sending one summary to Teams chats
// (`workflow_summary_send`, one confirmed send per chat); the pauses above the list are
// `workflow-pause.js`.
import { t, tn, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el, icon } from './dom.js';
import { current } from './state.js';
import { busy, done, notify } from './notices.js';
import { selectTab, showTab } from './nav.js';
import { chooseFromList } from './directory-sheet.js';
import { link, markdown } from './markdown.js';
import { allowNames, failureText, runStateLabel } from './run-states.js';
import { openAgentSession, sessionLifeBadge } from './sessions.js';
import { planChildrenReview } from './implementation.js';
import { STAGE_NAMES } from './workflow-blocks.js';
import { renderPauses } from './workflow-pause.js';

/** The states in which a run waits on the person (src/app/workflows/record.rs `waits_on_user`). */
const WAITING = ['plan_review', 'unassigned', 'needs_attention', 'interrupted', 'failed'];
const FINAL = ['done', 'cancelled'];
const RESUMABLE = ['needs_attention', 'interrupted', 'failed'];
const POLL_MS = 15_000;
/** The longest handover note (src/app/workflows/record.rs `MAX_HANDOVER`). */
const MAX_HANDOVER = 4000;

const runsData = {
  /** @type {any[]} */ runs: [],
  /** The open run's ID, or null while the list shows. @type {string | null} */
  open: null,
  /** The open run's last record, as `workflow_run_status` returned it. @type {any} */
  record: null,
  /** The stored summary chosen for sending (the latest when unset). @type {string | null} */
  chosenSummary: null,
  /** Each list read and each detail read has its own counter, so one never discards the other. */
  listGeneration: 0,
  detailGeneration: 0,
  /** The open run's agent sessions, as last read. @type {any[]} */
  sessions: [],
  /** @type {ReturnType<typeof setInterval> | null} */
  timer: null,
  /** A send in progress or a confirmation open: the detail is not re-rendered meanwhile. */
  holding: false,
};
/** The active run of each work item, by its link (the To-Do card's «workflow run» link). @type {Map<string, any>} */
const activeRuns = new Map();

/** The tone of a run's state: finished well, waiting on the person, over, or on its way. */
function tone(state) {
  if (state === 'done') return 'ok';
  if (WAITING.includes(state)) return 'warn';
  if (state === 'cancelled') return 'muted';
  return 'info';
}
/** Unix seconds in the device's locale. */
function when(seconds) {
  return seconds ? new Date(seconds * 1000).toLocaleString(uiLocale()) : t('Not recorded');
}
function message(error) {
  return tx(String(error?.message ?? error)).replace(/^\[[a-z_]+\] /, '');
}
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
function workflowName(id) {
  return workflowsNow().find(w => w.id === id)?.name ?? id ?? t('No workflow');
}
/** The account's workflows as the profile holds them now. */
function workflowsNow() {
  return current?.map?.workflows ?? [];
}
/** An Azure DevOps organization's name from its URL (`dev.azure.com/NAME` or `NAME.visualstudio.com`), lowercased. */
function organizationName(url) {
  try {
    const parsed = new URL(String(url ?? ''));
    if (parsed.hostname === 'dev.azure.com') return decodeURIComponent(parsed.pathname.split('/').filter(Boolean)[0] ?? '').toLowerCase();
    if (parsed.hostname.endsWith('.visualstudio.com')) return parsed.hostname.slice(0, -'.visualstudio.com'.length).toLowerCase();
  } catch { /* not a URL */ }
  return String(url ?? '').toLowerCase();
}

// ---- The list ----

/** Reads the runs (filtered as the selects say) and the workflows their names come from. */
async function loadWorkflowRuns() {
  const generation = ++runsData.listGeneration;
  const problem = $('#runs-problem'); problem.hidden = true;
  const state = /** @type {HTMLSelectElement} */ ($('#runs-state')).value;
  const workflow = /** @type {HTMLSelectElement} */ ($('#runs-workflow')).value;
  try {
    const list = await invoke('workflow_runs', { ...(state ? { state } : {}), ...(workflow ? { workflow } : {}), limit: 100 });
    if (generation !== runsData.listGeneration) return;
    fillWorkflowFilter(workflow);
    renderPauses(list.paused ?? [], loadWorkflowRuns);
    const runs = (list.runs ?? []).slice().sort((a, b) => Number(WAITING.includes(b.state)) - Number(WAITING.includes(a.state)) || b.updated_at - a.updated_at);
    runsData.runs = runs;
    rememberActive(runs);
    const waiting = runs.filter(run => WAITING.includes(run.state)).length;
    $('#runs-summary').textContent = runs.length ? (waiting ? tn(waiting, '1 run waits for you', '{n} runs wait for you') : tn(runs.length, '1 run', '{n} runs')) : '';
    $('#run-rows').replaceChildren(...(runs.length ? runs.map(runRow) : [el('p', { class: 'hint', textContent: state || workflow ? t('No run matches these filters.') : t('No workflow run yet. Start one from a To-Do item\'s Implement, or let the watcher start them for items in a ready state.') })]));
    if (list.partial) $('#run-rows').append(el('p', { class: 'hint', textContent: t('Showing the most recent runs.') }));
  } catch (error) {
    if (generation !== runsData.listGeneration) return;
    problem.hidden = false; problem.textContent = message(error);
  }
}
function fillWorkflowFilter(selected) {
  const select = /** @type {HTMLSelectElement} */ ($('#runs-workflow'));
  select.replaceChildren(el('option', { value: '', textContent: t('Every workflow') }),
    ...workflowsNow().map(w => el('option', { value: w.id, textContent: w.name, selected: w.id === selected })));
}
/** The active runs by item link, for the To-Do cards. */
function rememberActive(runs) {
  for (const run of runs) {
    const active = !FINAL.includes(run.state);
    if (active) activeRuns.set(run.item.url, run);
    else if (activeRuns.get(run.item.url)?.id === run.id) activeRuns.delete(run.item.url);
  }
}
/** Reads the active runs once, so To-Do can mark the items that have one; silent without the engine. */
async function refreshActiveRuns() {
  try {
    const list = await invoke('workflow_runs', { limit: 100 });
    activeRuns.clear();
    rememberActive(list.runs ?? []);
  } catch { /* an older host, or a computer without the engine: no link is shown */ }
}
function activeRunFor(url) {
  return activeRuns.get(url) ?? null;
}
/** The To-Do card's small link to the item's active run. */
function runLinkFor(item) {
  const run = activeRunFor(item.url);
  if (!run) return null;
  const button = el('button', { type: 'button', class: 'icon-button secondary', 'data-run': run.id, title: t('Workflow run: {state}', { state: runStateLabel(run.state) }), 'aria-label': t('Workflow run: {state}', { state: runStateLabel(run.state) }) }, icon('flow'));
  button.onclick = () => openWorkflowRun(run.id, button);
  return button;
}

/** A run in a stage that takes one of the `concurrency` slots, without one: its place in the line (1 is next). */
function slotBadge(run) {
  return run.waiting_for_slot ? badge(t('Waiting for a slot, position {n}', { n: run.queue_position }), 'muted') : null;
}

/** A run a pause holds: nothing new starts for it until the pause ends. */
function pausedBadge(run) {
  return run.paused ? badge(t('Paused'), 'muted') : null;
}

/** A run as a card, the same layout for every state: its badges, the item's number and title, its type, why it waits, then the facts. */
function runRow(run) {
  const waits = WAITING.includes(run.state);
  const row = el('button', { type: 'button', class: `card run-row${waits ? ' waits' : ''}`, 'data-run': run.id, 'data-state': run.state },
    el('span', { class: 'card-main' },
      el('span', { class: 'card-meta' }, badge(runStateLabel(run.state), tone(run.state)), waits ? badge(t('Waits for you'), 'warn') : null, slotBadge(run), pausedBadge(run)),
      el('strong', { class: 'run-row-title', textContent: `#${run.item.id} · ${run.item.title}` }),
      el('span', { class: 'run-row-kind', textContent: tx(run.item.kind) }),
      run.attention ? el('span', { class: 'card-text', textContent: tx(run.attention).slice(0, 160) }) : null,
      el('small', { class: 'run-row-facts', textContent: [workflowName(run.workflow), tn(run.children ?? 0, '1 child', '{n} children'), t('Updated {time}', { time: when(run.updated_at) })].join(' · ') })),
    icon('arrow'));
  row.onclick = () => openWorkflowRun(run.id, row);
  return row;
}

// ---- The detail ----

/** Opens one run in the Runs view (from a row, Home, Implement or a To-Do card). */
async function openWorkflowRun(id, trigger = null) {
  if ($('#workflows').hidden) showTab('workflows', 'workflows-runs');
  selectTab($('#workflows-tabs'), 'runs');
  runsData.open = id; runsData.record = null; runsData.sessions = []; runsData.chosenSummary = null; runsData.holding = false;
  $('#runs-list-panel').hidden = true;
  const detail = $('#workflow-run-detail'); detail.hidden = false;
  detail.dataset.trigger = trigger?.dataset.run ?? '';
  $('#run-detail-content').replaceChildren(el('p', { class: 'hint', textContent: t('Loading…') }));
  $('#run-detail-title').textContent = t('Workflow run');
  $('#run-detail-title').focus({ preventScroll: true });
  await loadRunDetail();
  if (runsData.timer) clearInterval(runsData.timer);
  runsData.timer = setInterval(() => {
    // Only while the run's detail is on screen: leaving the section stops the reads.
    if (!detailShown()) { if (runsData.timer) clearInterval(runsData.timer); runsData.timer = null; return; }
    if (document.visibilityState === 'visible') loadRunDetail().catch(() => {});
  }, POLL_MS);
}
/** Whether a run's detail is on screen now. */
function detailShown() {
  return Boolean(runsData.open) && !$('#workflows').hidden && !$('#workflows-runs').hidden && !$('#workflow-run-detail').hidden;
}
/** The Runs view's list (from Home's «Open Workflows»): any open detail closes, and the list is read once. */
function showRunsList() {
  if (runsData.timer) { clearInterval(runsData.timer); runsData.timer = null; }
  runsData.open = null; runsData.record = null; runsData.holding = false;
  $('#workflow-run-detail').hidden = true;
  $('#runs-list-panel').hidden = false;
  if ($('#workflows').hidden) showTab('workflows', 'workflows-runs');
  selectTab($('#workflows-tabs'), 'runs');
  return loadWorkflowRuns();
}
/** Back to the list; the row that opened the run gets the focus again. */
function closeWorkflowRun() {
  const id = runsData.open;
  runsData.open = null; runsData.record = null;
  if (runsData.timer) { clearInterval(runsData.timer); runsData.timer = null; }
  $('#workflow-run-detail').hidden = true;
  $('#runs-list-panel').hidden = false;
  loadWorkflowRuns().then(() => { /** @type {HTMLElement | null} */ (document.querySelector(`#run-rows [data-run="${id}"]`))?.focus(); });
}
/** Reads the open run again; a stale answer, or one that arrives while the person types or confirms, changes nothing. */
async function loadRunDetail() {
  const id = runsData.open;
  if (!id) return;
  const generation = ++runsData.detailGeneration;
  let reply;
  try {
    reply = await invoke('workflow_run_status', { run: id });
  } catch (error) {
    if (generation !== runsData.detailGeneration || runsData.open !== id) return;
    $('#run-detail-content').replaceChildren(el('p', { class: 'callout error', role: 'alert', textContent: message(error) }));
    return;
  }
  if (generation !== runsData.detailGeneration || runsData.open !== id) return;
  const run = reply.run;
  const urls = [run.item.url, ...run.children.map(c => c.url)];
  let sessions = [];
  try { sessions = (await invoke('implementation_sessions', { urls })).sessions ?? []; } catch { /* shown without sessions */ }
  if (generation !== runsData.detailGeneration || runsData.open !== id) return;
  const detail = $('#run-detail-content');
  const typing = detail.contains(document.activeElement) && ['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement?.tagName ?? '');
  if (runsData.record && (typing || runsData.holding)) return;
  const same = runsData.record && JSON.stringify([runsData.record, runsData.sessions]) === JSON.stringify([run, sessions]);
  runsData.record = run; runsData.sessions = sessions;
  if (same) return;
  rememberActive([{ ...run, item: run.item }]);
  renderRunDetail(run, sessions);
}

function renderRunDetail(run, sessions) {
  const detail = $('#run-detail-content');
  const focused = /** @type {HTMLElement | null} */ (detail.contains(document.activeElement) ? document.activeElement : null);
  const focusId = focused?.id;
  $('#run-detail-title').textContent = `#${run.item.id} · ${run.item.title}`;
  const session = coding => sessions.find(s => s.id === coding?.id) ?? null;
  const sessionButton = (coding) => {
    const found = session(coding);
    if (!found) return null;
    const open = el('button', { type: 'button', class: 'secondary small', textContent: t('Open agent session') });
    open.onclick = () => openAgentSession({ ...found, machine: found.machine ?? coding.machine }, open);
    return open;
  };
  const head = el('div', { class: 'run-head' },
    badge(runStateLabel(run.state), tone(run.state)), slotBadge(run), pausedBadge(run), badge(workflowName(run.workflow), 'muted'), el('span', { class: 'grow' }), link(t('Open the work item'), run.item.url));
  const facts = el('dl', { class: 'todo-fields' },
    ...[[t('Type'), tx(run.item.kind)], [t('Project'), run.item.project], [t('Started at'), when(run.created_at)], [t('Last change'), when(run.updated_at)],
      ...(run.finished_at ? [[t('Finished at'), when(run.finished_at)]] : []),
      ...(run.auto_start ? [[t('Started by'), t('The watcher (a ready state)')]] : []),
      ...(run.allow?.length ? [[t('Also allowed'), allowNames(run.allow).join(', ')]] : [])]
      .map(([label, value]) => el('div', {}, el('dt', { textContent: label }), el('dd', { textContent: value }))),
    // The run it restarted and the one that restarted it, each opened in place.
    ...[[t('Continues run'), run.continues, 'run-continues'], [t('Restarted as'), run.restarted_as, 'run-restarted-as']].filter(([, id]) => id).map(([label, id, key]) => {
      const open = el('button', { type: 'button', class: 'link-button', id: key, textContent: String(id).slice(0, 8) });
      open.onclick = () => openWorkflowRun(String(id), open);
      return el('div', {}, el('dt', { textContent: label }), el('dd', {}, open));
    }));
  const notes = [];
  if (run.attention) notes.push(el('div', { class: `callout ${WAITING.includes(run.state) ? 'warn' : 'info'}`, id: 'run-attention' }, el('p', { textContent: tx(run.attention) })));
  if (run.error) notes.push(el('div', { class: 'callout error', role: 'alert' }, el('p', { textContent: tx(run.error) })));
  if (run.classification?.reason) notes.push(el('p', { class: 'hint', id: 'run-classification', textContent: t('The director said: {reason}', { reason: run.classification.reason }) }));
  // Why the session it waits on did not end with its answer, from the host's fixed vocabulary.
  const stopped = WAITING.includes(run.state) ? (run.classification?.failure ?? run.planning?.failure ?? (run.children ?? []).map(c => c.coding?.failure).find(Boolean)) : null;
  if (stopped) notes.push(el('div', { class: 'callout warn', id: 'run-failure' }, el('p', {}, el('strong', { textContent: t('Why it stopped') }), ' ', failureText(stopped))));
  if (run.handover) notes.push(el('details', { class: 'fold', id: 'run-handover' }, el('summary', { textContent: t('Handover note from the previous run') }), el('p', { class: 'run-handover-text', textContent: run.handover })));
  detail.replaceChildren(head, facts, ...notes, runActions(run, sessionButton), timeline(run), children(run, sessionButton, sessions), blocks(run), summaries(run));
  if (focusId) /** @type {HTMLElement | null} */ (document.getElementById(focusId))?.focus({ preventScroll: true });
}

/** The actions the run's state allows, each the host's operation and nothing else. */
function runActions(run, sessionButton) {
  const actions = el('div', { class: 'actions run-actions' });
  const area = el('div', { id: 'run-action-area' });
  const refresh = async () => { runsData.holding = false; runsData.record = null; await loadRunDetail(); };
  if (run.state === 'plan_review' && run.planning) {
    const review = el('button', { type: 'button', textContent: t('Review plan'), id: 'run-review-plan' });
    review.onclick = () => busy(review, () => showPlanReview(run, area, refresh));
    actions.append(review);
  }
  if (run.state === 'unassigned') {
    const choose = el('button', { type: 'button', textContent: t('Choose workflow'), id: 'run-choose-workflow' });
    choose.onclick = () => showAssign(run, area, refresh);
    actions.append(choose);
  }
  if (RESUMABLE.includes(run.state)) {
    actions.append(twice(t('Resume'), t('Confirm resume'), async () => {
      const reply = await invoke('workflow_run_resume', { run: run.id, confirmed: true });
      done(reply.accepted ? t('Run resumed; what stopped it was accepted.') : t('Run resumed.'));
      await refresh();
    }));
  }
  if (!FINAL.includes(run.state) && run.workflow) {
    const restart = el('button', { type: 'button', class: 'secondary', textContent: t('Restart with a note'), id: 'run-restart-open' });
    restart.onclick = () => showRestart(run, area, refresh);
    actions.append(restart);
  }
  if (!FINAL.includes(run.state)) {
    const cancel = twice(t('Cancel run'), t('Confirm cancel'), async () => {
      const reply = await invoke('workflow_run_cancel', { run: run.id, confirmed: true });
      const stopped = reply.sessions_stopped?.length ?? 0;
      if (reply.warning) notify(tx(reply.warning), 'warn');
      else done(stopped ? tn(stopped, 'Run cancelled; 1 session still alive was stopped.', 'Run cancelled; {n} sessions still alive were stopped.') : t('Run cancelled.'));
      await refresh();
    });
    cancel.classList.add('danger');
    actions.append(cancel);
  }
  if (run.planning) { const open = sessionButton(run.planning); if (open) { open.textContent = t('Open planning session'); actions.append(open); } }
  const summary = el('button', { type: 'button', class: 'secondary', textContent: t('Summary'), id: 'run-summary' });
  summary.onclick = () => busy(summary, async () => {
    const reply = await invoke('workflow_summary', { run: run.id });
    runsData.chosenSummary = reply.summary?.id ?? null;
    done(t('Summary written.'));
    await refresh();
  });
  actions.append(summary);
  if (run.summaries?.length) {
    const send = el('button', { type: 'button', class: 'secondary', textContent: t('Send by Teams'), id: 'run-send' });
    send.onclick = () => busy(send, () => showSend(run, area, refresh));
    actions.append(send);
  }
  return el('div', {}, actions, area);
}

/** The plan-review gate: the planning run's children, edited and confirmed with the existing creation. */
async function showPlanReview(run, area, refresh) {
  area.replaceChildren(el('p', { class: 'hint', textContent: t('Loading the plan…') }));
  const { machine, id } = run.planning;
  const result = await invoke('implementation_runs', { machine, work_item_url: run.item.url });
  const planning = (result.runs ?? []).find(r => r.id === id);
  if (!planning?.plan?.children) {
    area.replaceChildren(el('p', { class: 'callout warn', textContent: t('The plan is not on its machine yet. Try again in a moment.') }));
    return;
  }
  runsData.holding = true;
  const selection = { provider: run.item.provider, id: run.item.id, project: run.item.project, ...(run.item.source ? { source: run.item.source } : {}), task: run.item.category === 'task' };
  const panel = el('div', { class: 'panel run-plan-review', id: 'run-plan-review' },
    el('div', { class: 'panel-head' }, el('h3', { textContent: t('Children to create') }), el('span', { class: 'hint', textContent: t('Nothing is written until you confirm. The run continues on its own afterwards.') })),
    planChildrenReview(planning, run.item.provider === 'azure', async chosen => {
      await invoke('implementation_create', { machine, id, item: selection, children: chosen, confirmed: true });
      done(t('Children requested; the run continues.'));
      area.replaceChildren();
      await refresh();
    }));
  const close = el('button', { type: 'button', class: 'secondary', textContent: t('Close') });
  close.onclick = () => { runsData.holding = false; area.replaceChildren(); refresh(); };
  panel.append(el('div', { class: 'actions' }, close));
  area.replaceChildren(panel);
}

/** A starting point for the handover note from what the record holds: the stages reached, each child's branch and pull requests. */
function handoverDraft(run) {
  const reached = [...new Set((run.timeline ?? []).map(s => s.stage).filter(s => !FINAL.includes(s) && !WAITING.includes(s)))].map(runStateLabel);
  const lines = [t('Done in the previous run, not to be repeated:')];
  if (reached.length) lines.push(`- ${t('Stages reached: {stages}', { stages: reached.join(', ') })}`);
  for (const child of run.children ?? []) {
    const facts = [child.branch ? t('Branch {name}', { name: child.branch }) : null,
      ...(child.pull_requests ?? []).map(pr => t('Pull request !{id} ({status})', { id: String(pr.id), status: tx(pr.status) }))].filter(Boolean);
    if (facts.length) lines.push(`- #${child.id} ${child.title}: ${facts.join(' · ')}`);
  }
  lines.push('', t('Left to do:'), '- ');
  return lines.join('\n');
}
/** Restart: this run ends and a new one of its workflow starts for its item, with the person's handover note for its coding runs. */
function showRestart(run, area, refresh) {
  runsData.holding = true;
  const note = el('textarea', { class: 'run-restart-note', id: 'run-restart-note', maxLength: MAX_HANDOVER, value: handoverDraft(run) });
  const count = el('small', { class: 'hint', id: 'run-restart-count' });
  const counted = () => { count.textContent = t('{n} of {max} characters', { n: String(note.value.length), max: String(MAX_HANDOVER) }); };
  note.oninput = counted; counted();
  const restart = twice(t('Restart'), t('Confirm restart'), async () => {
    const reply = await invoke('workflow_run_restart', { run: run.id, ...(note.value.trim() ? { note: note.value } : {}), confirmed: true });
    if (reply.warning) notify(tx(reply.warning), 'warn');
    else done(reply.paused ? t('Run restarted; the new run waits for the pause to end.') : t('Run restarted; the new run starts from its first stage.'));
    runsData.holding = false;
    await openWorkflowRun(reply.run.id);
  });
  restart.id = 'run-restart-confirm';
  const close = el('button', { type: 'button', class: 'link-button', textContent: t('Close') });
  close.onclick = () => { runsData.holding = false; area.replaceChildren(); refresh(); };
  area.replaceChildren(el('div', { class: 'panel', id: 'run-restart' },
    el('h3', { textContent: t('Restart with a handover note') }),
    el('p', { class: 'hint', textContent: t('This run is cancelled, its live sessions stop and the file changes stay; a new run of the same workflow starts for this work item from its first stage. Each of its coding runs gets your note, which is posted nowhere: not to Teams, not to the work item.') }),
    el('label', {}, t('Handover note'), note), count,
    el('div', { class: 'actions' }, restart, close)));
  note.focus();
}

/** An unassigned run: the enabled workflows of its project, with what the director said. */
function showAssign(run, area, refresh) {
  const same = w => w.enabled && w.project.toLowerCase() === run.item.project.toLowerCase() && organizationName(w.organization) === organizationName(run.item.organization);
  const options = workflowsNow().filter(same);
  runsData.holding = true;
  const select = el('select', { id: 'run-assign-workflow' }, ...options.map(w => el('option', { value: w.id, textContent: `${w.name}${w.description ? ` — ${w.description}` : ''}` })));
  const assign = twice(t('Assign'), t('Confirm assign'), async () => {
    await invoke('workflow_run_assign', { run: run.id, workflow: select.value, confirmed: true });
    done(t('Workflow assigned; the run continues.'));
    area.replaceChildren();
    await refresh();
  });
  const close = el('button', { type: 'button', class: 'link-button', textContent: t('Close') });
  close.onclick = () => { runsData.holding = false; area.replaceChildren(); refresh(); };
  area.replaceChildren(el('div', { class: 'panel', id: 'run-assign' },
    el('h3', { textContent: t('Choose the workflow for this run') }),
    run.classification?.reason ? el('p', { class: 'hint', textContent: t('The director said: {reason}', { reason: run.classification.reason }) }) : null,
    options.length ? el('label', {}, t('Workflow'), select) : el('p', { class: 'callout warn', textContent: t('No enabled workflow covers this project. Enable or create one first.') }),
    el('div', { class: 'actions' }, options.length ? assign : null, close)));
}

// ---- Timeline, children, blocks ----

function timeline(run) {
  const items = (run.timeline ?? []).map((stage, index, all) => {
    const current = index === all.length - 1 && !FINAL.includes(run.state);
    return el('li', { class: current ? 'current' : '' },
      el('i', { class: `dot ${/fail|needs|interrupt|cancel|uncertain|refused|timed|missing|abandon|permission/.test(stage.outcome ?? '') ? 'warn' : stage.finished_at ? 'ok' : 'info'}`, 'aria-hidden': 'true' }),
      el('div', {}, el('strong', { textContent: runStateLabel(stage.stage) }),
        el('small', { textContent: `${when(stage.started_at)}${stage.finished_at ? ` → ${when(stage.finished_at)}` : ''}${stage.outcome ? ` · ${tx(stage.outcome)}` : ''}` })));
  });
  return el('details', { class: 'fold', open: true, id: 'run-timeline' }, el('summary', { textContent: tn(items.length, 'Timeline (1 stage)', 'Timeline ({n} stages)') }),
    items.length ? el('ol', { class: 'run-timeline' }, ...items) : el('p', { class: 'hint', textContent: t('No stage yet.') }));
}

function children(run, sessionButton, sessions) {
  const rows = (run.children ?? []).map(child => {
    const life = sessionLifeBadge(sessions.find(s => s.id === child.coding?.id));
    const state = child.coding?.state ?? (child.coding ? t('Starting') : t('Waiting'));
    const prs = child.pull_requests ?? [];
    return el('div', { class: 'resource run-child', 'data-child': String(child.id) },
      el('div', { class: 'resource-head' }, badge(tx(state), state === 'done' ? 'ok' : ['failed', 'permission_prompt', 'interrupted', 'needs_attention', 'missing'].includes(state) ? 'warn' : 'info'),
        link(`#${child.id} ${child.title}`, child.url), el('span', { class: 'grow' }), life, sessionButton(child.coding)),
      el('p', { class: 'resource-facts' }, el('small', { textContent: [child.repository?.name, child.branch ? t('Branch {name}', { name: child.branch }) : null, child.outcome ? tx(child.outcome) : null].filter(Boolean).join(' · ') })),
      child.coding?.failure ? el('p', { class: 'hint run-child-failure', textContent: failureText(child.coding.failure) }) : null,
      prs.length ? el('ul', { class: 'run-pulls' }, ...prs.map(pr => el('li', {}, link(pr.title ? `!${pr.id} ${pr.title}` : pr.url, pr.url), ' ', badge(tx(pr.status), pr.status === 'completed' ? 'ok' : pr.status === 'abandoned' ? 'warn' : 'info')))) : null);
  });
  return el('details', { class: 'fold', open: true, id: 'run-children' }, el('summary', { textContent: tn(rows.length, 'Children (1)', 'Children ({n})') }),
    rows.length ? el('div', { class: 'list' }, ...rows) : el('p', { class: 'hint', textContent: run.state === 'plan_review' ? t('The children are created once you confirm the plan.') : t('No child yet.') }));
}

function blocks(run) {
  const names = Object.fromEntries((run.blocks ?? []).map(b => [b.id, b.name]));
  const rows = (run.block_outcomes ?? []).map(o => el('div', { class: 'resource run-block' },
    el('div', { class: 'resource-head' }, badge(tx(o.outcome), ['sent', 'succeeded'].includes(o.outcome) ? 'ok' : ['attempting', 'skipped'].includes(o.outcome) ? 'muted' : ['refused', 'failed', 'timed_out', 'uncertain'].includes(o.outcome) ? 'warn' : 'info'),
      el('strong', { textContent: names[o.block] ?? o.block }), o.stage ? badge(STAGE_NAMES[o.stage] ?? o.stage, 'muted') : null),
    el('p', { class: 'resource-facts' }, el('small', { textContent: [when(o.at), o.reason ? tx(o.reason) : null, o.exit_code != null ? t('Exit code {code}', { code: String(o.exit_code) }) : null, o.duration_ms != null ? t('{n} ms', { n: String(o.duration_ms) }) : null].filter(Boolean).join(' · ') }))));
  if (!rows.length) return el('div', {});
  return el('details', { class: 'fold', id: 'run-blocks' }, el('summary', { textContent: tn(rows.length, 'Block outcomes (1)', 'Block outcomes ({n})') }), el('div', { class: 'list' }, ...rows));
}

// ---- Summaries and sending ----

function summaries(run) {
  const all = run.summaries ?? [];
  if (!all.length) return el('p', { class: 'hint', id: 'run-no-summary', textContent: t('No progress summary yet. Summary asks the director for one, written from this run\'s record.') });
  const chosen = all.find(s => s.id === runsData.chosenSummary) ?? all[all.length - 1];
  runsData.chosenSummary = chosen.id;
  const latest = el('article', { class: 'panel run-summary', id: 'run-summary-text' },
    el('div', { class: 'panel-head' }, el('h3', { textContent: t('Progress summary') }), el('span', { class: 'hint', textContent: `${when(chosen.created_at)} · ${chosen.model ?? ''}` })),
    markdown(chosen.text));
  const older = all.filter(s => s.id !== chosen.id).reverse();
  const history = older.length ? el('details', { class: 'fold' }, el('summary', { textContent: tn(older.length, '1 earlier summary', '{n} earlier summaries') }),
    el('div', { class: 'list' }, ...older.map(s => {
      const use = el('button', { type: 'button', class: 'link-button', textContent: t('Show this one') });
      use.onclick = () => { runsData.chosenSummary = s.id; renderRunDetail(runsData.record ?? run, runsData.sessions); };
      return el('div', { class: 'resource' }, el('p', { class: 'resource-facts' }, el('small', { textContent: `${when(s.created_at)} · ${s.model ?? ''}` })), el('p', { textContent: s.text.slice(0, 240) }), use);
    }))) : null;
  const sends = (run.summary_sends ?? []).slice().reverse();
  const sent = sends.length ? el('details', { class: 'fold', id: 'run-sends' }, el('summary', { textContent: tn(sends.length, '1 send', '{n} sends') }),
    el('div', { class: 'list' }, ...sends.map(s => el('div', { class: 'resource' }, el('div', { class: 'resource-head' },
      badge(tx(s.outcome), s.outcome === 'sent' ? 'ok' : s.outcome === 'refused' ? 'warn' : 'muted'), el('strong', { textContent: s.chat?.topic || s.chat?.id || '' }), el('span', { class: 'grow' }), el('small', { textContent: when(s.at) })),
      el('p', { class: 'resource-facts' }, el('small', { textContent: [s.mentions?.length ? t('Mentions {names}', { names: s.mentions.join(', ') }) : null, s.edited ? t('Edited text') : null, s.reason ? tx(s.reason) : null].filter(Boolean).join(' · ') })))))) : null;
  return el('div', {}, latest, history, sent);
}

/** Send one summary to chats: the recipients sheet, then one confirmation naming every destination with the exact text. */
async function showSend(run, area, refresh) {
  const all = run.summaries ?? [];
  const chosen = all.find(s => s.id === runsData.chosenSummary) ?? all[all.length - 1];
  if (!chosen) return;
  const directory = (await invoke('teams_chats', {})).chats ?? [];
  const chats = directory.filter(c => c.id !== 'self' && !String(c.id).startsWith('48:'));
  const trigger = $('#run-send');
  const picked = await new Promise(resolve => {
    const dialog = chooseFromList({
      trigger, title: t('Send the summary to'), multiple: true, max: 10, allLabel: t('Chats'),
      items: chats.map(c => ({ value: c.id, label: c.topic || (c.members ?? []).join(', '), detail: c.type === 'group' ? tn((c.members ?? []).length, '1 member', '{n} members') : t('Person') })),
      empty: t('No chat to choose: connect the Microsoft account first.'), confirm: t('Continue'),
      countText: n => tn(n, '1 chat chosen', '{n} chats chosen'),
      apply: values => resolve(values),
    });
    dialog.addEventListener('close', () => resolve([]), { once: true });
  });
  if (!picked.length) return;
  runsData.holding = true;
  const text = el('textarea', { class: 'run-send-text', id: 'run-send-text', value: chosen.text, maxLength: 8000 });
  const destinations = picked.map(id => chats.find(c => c.id === id)).filter(Boolean).map(chat => {
    const members = chat.type === 'group' ? (chat.members ?? []) : [];
    const boxes = members.map(name => { const box = el('input', { type: 'checkbox', value: name }); return { name, box, row: el('label', { class: 'check' }, box, name) }; });
    const outcome = el('p', { class: 'hint send-outcome', 'aria-live': 'polite' });
    const doubtful = (run.summary_sends ?? []).some(s => s.summary === chosen.id && s.chat?.id === chat.id && s.outcome === 'uncertain');
    const again = doubtful ? el('input', { type: 'checkbox' }) : null;
    const card = el('div', { class: 'resource run-destination', 'data-chat': chat.id },
      el('div', { class: 'resource-head' }, el('strong', { textContent: chat.topic || members.join(', ') }), badge(chat.type === 'group' ? t('Group chat') : t('Person'), 'muted')),
      doubtful ? el('div', { class: 'callout warn' }, el('p', { textContent: t('An earlier send of this summary to this chat is uncertain: it may have been posted. Check Teams first; it is left out unless you send it again.') }),
        el('label', { class: 'check' }, again, t('Send again anyway'))) : null,
      boxes.length ? el('fieldset', {}, el('legend', { textContent: t('Mention') }), ...boxes.map(b => b.row)) : null, outcome);
    return { chat, boxes, outcome, card, again };
  });
  const send = twice(t('Confirm and send'), t('Send to {n} chat(s) now', { n: String(destinations.length) }), async () => {
    const edited = text.value.trim() !== chosen.text.trim();
    for (const destination of destinations) {
      if (destination.outcome.dataset.done) continue;
      if (destination.again && !destination.again.checked) {
        destination.outcome.replaceChildren(badge(t('Not sent'), 'muted'), ' ', el('span', { textContent: t('Left out: an earlier send may have been posted.') }));
        continue;
      }
      const mentions = destination.boxes.filter(b => b.box.checked).map(b => b.name);
      try {
        const reply = await invoke('workflow_summary_send', { run: run.id, summary: chosen.id, chat: destination.chat.id, mentions, ...(edited ? { text: text.value } : {}), confirmed: true });
        destination.outcome.dataset.done = 'sent';
        destination.outcome.replaceChildren(badge(t('Sent'), 'ok'), ' ', el('span', { textContent: reply.warning ? tx(reply.warning) : t('Posted as you.') }));
      } catch (error) {
        destination.outcome.dataset.done = 'failed';
        destination.outcome.replaceChildren(badge(t('Not sent'), 'warn'), ' ', el('span', { textContent: message(error) }));
      }
    }
    done(t('Sends finished; each destination shows its outcome.'));
    // Every destination has its outcome: nothing is left to send from this panel (busy() re-enables a button, so it hides).
    send.hidden = destinations.every(d => d.outcome.dataset.done);
  });
  const close = el('button', { type: 'button', class: 'secondary', textContent: t('Close') });
  close.onclick = () => { runsData.holding = false; area.replaceChildren(); refresh(); };
  area.replaceChildren(el('div', { class: 'panel', id: 'run-send-panel' },
    el('h3', { textContent: t('Send this summary') }),
    el('p', { class: 'hint', textContent: t('Exactly this text is posted as you, once per chat, with the members you tick mentioned. Nothing is sent until you confirm.') }),
    el('label', {}, t('Text'), text),
    el('div', { class: 'list' }, ...destinations.map(d => d.card)),
    el('div', { class: 'actions' }, send, close)));
  text.focus();
}

export { loadWorkflowRuns, openWorkflowRun, closeWorkflowRun, showRunsList, refreshActiveRuns, activeRunFor, runLinkFor, WAITING };
