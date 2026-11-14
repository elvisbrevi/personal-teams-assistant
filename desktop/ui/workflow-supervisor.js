// Workflows › Runs: the workflow supervisor's looks at one run (src/app/workflows/supervisor.rs):
// what it found, the checks of the final report with «Verified» or «Not verified» and code's
// fact, its drafts, and each action it proposed with its state. A proposal waits for the person
// and is carried out once with `workflow_supervision_act` (a restart through the run's restart
// form with the drafted note, a post to one chat the person chooses, a work item through Create
// request), or set aside with `workflow_supervision_dismiss`. The run view passes its shared
// pieces (`twice`, `when`, holding the detail, opening the restart form).
import { t, tn, tx } from './i18n.js';
import { invoke } from './control.js';
import { badge, el } from './dom.js';
import { busy, done } from './notices.js';
import { chooseFromList } from './directory-sheet.js';
import { link, markdown } from './markdown.js';
import { failureText, runStateLabel } from './run-states.js';
import { openRequest } from './todo-request.js';

/**
 * What the run view lends the panel.
 * @typedef {{ twice: (label: string, confirm: string, act: () => Promise<void>) => HTMLButtonElement,
 *   when: (seconds: number) => string, hold: (held: boolean) => void, restart: (proposed: any) => void }} Shared
 */


/** What each action the supervisor may propose does, for the person. */
const ACTION_NAMES = {
  restart: () => t('Restart with the drafted note'),
  resume: () => t('Resume the run'),
  cancel: () => t('Cancel the run'),
  pause_project: () => t('Pause this project\'s runs'),
  create_work_item: () => t('Create the drafted work item'),
  post_summary: () => t('Send the drafted summary by Teams'),
  move_ahead: () => t('Move the run ahead in the queue'),
};
const ACTION_STATES = {
  proposed: () => t('Proposed'), done: () => t('Done by the supervisor'), attempting: () => t('In progress'), executed: () => t('Carried out'),
  refused: () => t('Refused'), uncertain: () => t('Uncertain'), opened: () => t('Opened in Create request'), expired: () => t('Expired'), dismissed: () => t('Set aside'),
};
const EVENT_NAMES = {
  run_stopped: () => t('The run stopped'), run_waiting: () => t('The run waited too long'), machine_offline: () => t('Its machine went offline'),
  session_disagrees: () => t('Its state and its session disagree'), requested: () => t('You asked'),
};

/** The supervisor's latest look at a run, or null. */
function latestLook(run) {
  const looks = run.supervision ?? [];
  return looks.length ? looks[looks.length - 1] : null;
}
/** A proposal of the latest look for `action` that still waits for the person. */
function proposal(run, action) {
  const look = latestLook(run);
  const found = look?.actions?.find(a => a.action === action && a.state === 'proposed');
  return found ? { look, action: found } : null;
}
/** Carry out one proposal, as the person confirmed it. */
function act(run, proposed, extra = {}) {
  return invoke('workflow_supervision_act', { run: run.id, look: proposed.look.id, action: proposed.action.id, ...extra, confirmed: true });
}

/** The controls of one proposal: the action itself (twice, or its panel) and setting it aside. */
function proposalControls(run, proposed, area, refresh, shared) {
  const { twice } = shared;
  const controls = [];
  const after = async text => { done(text); await refresh(); };
  switch (proposed.action.action) {
    case 'restart': {
      const open = el('button', { type: 'button', class: 'secondary', id: 'run-restart-open', textContent: t('Restart with a note') });
      open.onclick = () => shared.restart(proposed);
      controls.push(open);
      break;
    }
    case 'resume':
      controls.push(twice(t('Resume'), t('Confirm resume'), async () => { await act(run, proposed); await after(t('Run resumed.')); }));
      break;
    case 'cancel': {
      const cancel = twice(t('Cancel run'), t('Confirm cancel'), async () => { await act(run, proposed); await after(t('Run cancelled.')); });
      cancel.classList.add('danger');
      controls.push(cancel);
      break;
    }
    case 'pause_project':
      controls.push(twice(t('Pause'), t('Confirm pause'), async () => { await act(run, proposed); await after(t('The project\'s runs are paused.')); }));
      break;
    case 'post_summary': {
      const send = el('button', { type: 'button', class: 'secondary', textContent: t('Send by Teams') });
      send.onclick = () => busy(send, () => showProposedSend(run, proposed, area, refresh, send, shared));
      controls.push(send);
      break;
    }
    case 'create_work_item': {
      const create = el('button', { type: 'button', class: 'secondary', textContent: t('Open Create request') });
      create.onclick = () => busy(create, async () => {
        const reply = await act(run, proposed);
        const draft = reply.create_request ?? {};
        const description = [draft.title, draft.description, draft.repository ? t('Repository: {name}', { name: draft.repository }) : null].filter(Boolean).join('\n\n');
        await openRequest({ provider: 'azure', project: draft.project, organization: draft.organization, kind: draft.type === 'bug' ? 'bug' : 'hu', context: description });
      });
      controls.push(create);
      break;
    }
    default: break;
  }
  const dismiss = el('button', { type: 'button', class: 'link-button', textContent: t('Set aside') });
  dismiss.onclick = () => busy(dismiss, async () => {
    await invoke('workflow_supervision_dismiss', { run: run.id, look: proposed.look.id, action: proposed.action.id });
    await after(t('Proposal set aside.'));
  });
  controls.push(dismiss);
  return controls;
}

/** The supervisor's latest look: what it found, the checks of the report, its drafts and proposals; older looks folded. */
function supervision(run, area, refresh, shared) {
  const { when } = shared;
  const looks = run.supervision ?? [];
  const queued = run.supervising ?? [];
  const parts = [];
  if (queued.length) parts.push(el('p', { class: 'hint', id: 'run-supervising', role: 'status', textContent: queued.some(q => q.state === 'running') ? t('The supervisor is looking at this run…') : t('A look at this run is queued.') }));
  const look = latestLook(run);
  if (!look) return parts.length ? el('div', { id: 'run-supervision' }, ...parts) : el('div', {});
  const head = el('div', { class: 'panel-head' }, el('h3', { textContent: t('Supervisor') }),
    el('span', { class: 'hint', textContent: [EVENT_NAMES[look.event]?.() ?? look.event, when(look.created_at), look.supervised_by?.model].filter(Boolean).join(' · ') }));
  const body = [head];
  if (look.outcome !== 'done') {
    body.push(el('div', { class: 'callout warn' }, el('p', {}, el('strong', { textContent: t('The look gave no answer.') }), ' ', look.failure ? failureText(look.failure) : '')));
  } else {
    const a = look.assessment ?? {};
    body.push(el('dl', { class: 'todo-fields run-assessment' },
      ...[[t('Done'), a.done], [t('Left'), a.left], [t('Recommends'), a.recommendation]].filter(([, v]) => v)
        .map(([label, value]) => el('div', {}, el('dt', { textContent: label }), el('dd', { textContent: value })))));
    const checks = look.checks ?? [];
    if (checks.length) {
      body.push(el('div', { class: 'list run-checks' }, ...checks.map(c => el('div', { class: 'resource run-check', 'data-result': c.result },
        el('div', { class: 'resource-head' }, badge(c.result === 'verified' ? t('Verified') : t('Not verified'), c.result === 'verified' ? 'ok' : 'warn'), el('span', { textContent: c.claim })),
        el('p', { class: 'resource-facts' }, el('small', { textContent: tx(c.fact) }))))));
    }
    if (look.unverified?.length) body.push(el('div', {}, el('strong', { textContent: t('It could not verify') }), el('ul', {}, ...look.unverified.map(u => el('li', { textContent: u })))));
    const gaps = [...(look.unread ?? []).map(u => t('Not read: {what}', { what: u })), ...(look.truncated ?? []).map(u => t('Cut to fit: {what}', { what: u }))];
    if (gaps.length) body.push(el('p', { class: 'hint run-look-gaps', textContent: gaps.join(' · ') }));
    if (look.handover) body.push(el('details', { class: 'fold' }, el('summary', { textContent: t('Drafted handover note') }), el('p', { class: 'run-handover-text', textContent: look.handover })));
    if (look.summary) body.push(el('details', { class: 'fold' }, el('summary', { textContent: t('Drafted summary') }), markdown(look.summary)));
    if (look.work_items?.length) {
      body.push(el('details', { class: 'fold' }, el('summary', { textContent: tn(look.work_items.length, '1 drafted work item', '{n} drafted work items') }),
        el('div', { class: 'list' }, ...look.work_items.map(w => el('div', { class: 'resource' },
          el('div', { class: 'resource-head' }, badge(w.type === 'bug' ? t('Bug') : t('HU'), 'muted'), el('strong', { textContent: w.title })),
          el('p', { textContent: w.description }), w.reason ? el('p', { class: 'hint', textContent: w.reason }) : null)))));
    }
    const actions = look.actions ?? [];
    if (actions.length) {
      body.push(el('div', { class: 'list run-proposals' }, ...actions.map(action => {
        const waiting = action.state === 'proposed';
        return el('div', { class: 'resource run-proposal', 'data-action': action.action, 'data-state': action.state },
          el('div', { class: 'resource-head' }, badge(ACTION_STATES[action.state]?.() ?? action.state, waiting ? 'warn' : ['executed', 'done'].includes(action.state) ? 'ok' : 'muted'),
            el('strong', { textContent: ACTION_NAMES[action.action]?.() ?? action.action })),
          action.reason ? el('p', { class: 'hint', textContent: action.reason }) : null,
          action.outcome ? el('p', { class: 'hint', textContent: tx(action.outcome) }) : null,
          waiting ? el('div', { class: 'actions' }, ...proposalControls(run, { look, action }, area, refresh, shared)) : null);
      })));
    }
  }
  const older = looks.slice(0, -1).reverse();
  if (older.length) {
    body.push(el('details', { class: 'fold' }, el('summary', { textContent: tn(older.length, '1 earlier look', '{n} earlier looks') }),
      el('div', { class: 'list' }, ...older.map(l => el('div', { class: 'resource' },
        el('p', { class: 'resource-facts' }, el('small', { textContent: [EVENT_NAMES[l.event]?.() ?? l.event, when(l.created_at)].join(' · ') })),
        l.assessment?.recommendation ? el('p', { textContent: l.assessment.recommendation }) : null)))));
  }
  return el('div', { id: 'run-supervision' }, ...parts, el('article', { class: 'panel run-look' }, ...body));
}

/** The supervisor's drafted summary to one chat: the chat, its mentions, then one confirmation with the exact text. */
async function showProposedSend(run, proposed, area, refresh, trigger, shared) {
  const { twice } = shared;
  const directory = (await invoke('teams_chats', {})).chats ?? [];
  const chats = directory.filter(c => c.id !== 'self' && !String(c.id).startsWith('48:'));
  const picked = await new Promise(resolve => {
    const dialog = chooseFromList({
      trigger, title: t('Send the summary to'), multiple: false, allLabel: t('Chats'),
      items: chats.map(c => ({ value: c.id, label: c.topic || (c.members ?? []).join(', '), detail: c.type === 'group' ? tn((c.members ?? []).length, '1 member', '{n} members') : t('Person') })),
      empty: t('No chat to choose: connect the Microsoft account first.'), confirm: t('Continue'),
      countText: n => tn(n, '1 chat chosen', '{n} chats chosen'),
      apply: values => resolve(values),
    });
    dialog.addEventListener('close', () => resolve([]), { once: true });
  });
  const chat = chats.find(c => c.id === picked[0]);
  if (!chat) return;
  shared.hold(true);
  const members = chat.type === 'group' ? (chat.members ?? []) : [];
  const boxes = members.map(name => ({ name, box: el('input', { type: 'checkbox', value: name }) }));
  const send = twice(t('Confirm and send'), t('Send now'), async () => {
    await act(run, proposed, { chat: chat.id, mentions: boxes.filter(b => b.box.checked).map(b => b.name) });
    shared.hold(false);
    area.replaceChildren();
    done(t('Summary sent.'));
    await refresh();
  });
  const close = el('button', { type: 'button', class: 'secondary', textContent: t('Close') });
  close.onclick = () => { shared.hold(false); area.replaceChildren(); refresh(); };
  area.replaceChildren(el('div', { class: 'panel', id: 'run-proposed-send' },
    el('h3', { textContent: t('Send the supervisor\'s summary') }),
    el('p', { class: 'hint', textContent: t('Exactly this text is posted as you in {chat}, once, with the members you tick mentioned. Nothing is sent until you confirm.', { chat: chat.topic || members.join(', ') }) }),
    markdown(proposed.look.summary),
    boxes.length ? el('fieldset', {}, el('legend', { textContent: t('Mention') }), ...boxes.map(b => el('label', { class: 'check' }, b.box, b.name))) : null,
    el('div', { class: 'actions' }, send, close)));
}

// ---- Pre-approval of a workflow ----

/**
 * What a workflow pre-approves of the supervisor, edited in place (`draft.preapprove`): a restart
 * after a cut session, and a post of its drafted summary to a chat the person chooses with the
 * members it mentions. Saved only after the confirmation that lists them (`preapproveLines`).
 * @param {any} draft
 * @param {() => void} changed
 */
function preapproveField(draft, changed) {
  draft.preapprove ??= [];
  const entry = action => draft.preapprove.find(p => p.action === action);
  const set = (action, value) => {
    draft.preapprove = [...draft.preapprove.filter(p => p.action !== action), ...(value ? [value] : [])]
      .sort((a, b) => a.action.localeCompare(b.action));
    changed();
  };
  const restart = el('input', { type: 'checkbox', checked: Boolean(entry('restart_after_cut')), 'data-preapprove': 'restart_after_cut' });
  restart.onchange = () => set('restart_after_cut', restart.checked ? { action: 'restart_after_cut' } : null);
  const post = el('input', { type: 'checkbox', checked: Boolean(entry('post_summary')), 'data-preapprove': 'post_summary' });
  const chatLine = el('div', { class: 'workflow-preapprove-chat' });
  const renderChat = () => {
    const current = entry('post_summary');
    const choose = el('button', { type: 'button', class: 'secondary small', textContent: current?.chat ? t('Change chat') : t('Choose chat') });
    choose.onclick = () => busy(choose, async () => {
      const chats = ((await invoke('teams_chats', {})).chats ?? []).filter(c => c.id !== 'self' && !String(c.id).startsWith('48:'));
      const picked = await new Promise(resolve => {
        const dialog = chooseFromList({
          trigger: choose, title: t('Post the summary to'), multiple: false, allLabel: t('Chats'),
          items: chats.map(c => ({ value: c.id, label: c.topic || (c.members ?? []).join(', '), detail: c.type === 'group' ? tn((c.members ?? []).length, '1 member', '{n} members') : t('Person') })),
          empty: t('No chat to choose: connect the Microsoft account first.'), confirm: t('Continue'),
          countText: n => tn(n, '1 chat chosen', '{n} chats chosen'),
          apply: values => resolve(values),
        });
        dialog.addEventListener('close', () => resolve([]), { once: true });
      });
      const chat = chats.find(c => c.id === picked[0]);
      if (!chat) return;
      set('post_summary', { action: 'post_summary', chat: { id: chat.id, name: chat.topic || (chat.members ?? []).join(', ') }, mentions: [], members: chat.type === 'group' ? (chat.members ?? []) : [] });
      renderChat();
    });
    const members = current?.members ?? [];
    const boxes = members.map(name => {
      const box = el('input', { type: 'checkbox', checked: (current.mentions ?? []).includes(name) });
      box.onchange = () => {
        const mentions = new Set(current.mentions ?? []);
        if (box.checked) mentions.add(name); else mentions.delete(name);
        current.mentions = members.filter(m => mentions.has(m));
        changed();
      };
      return el('label', { class: 'check' }, box, name);
    });
    chatLine.hidden = !post.checked;
    chatLine.replaceChildren(
      el('p', { class: 'hint', textContent: current?.chat ? t('Chat: {name}', { name: current.chat.name }) : t('Choose the chat it posts to.') }),
      boxes.length ? el('fieldset', {}, el('legend', { textContent: t('Mention') }), ...boxes)
        : (current?.mentions?.length ? el('p', { class: 'hint', textContent: t('Mentions {names}', { names: current.mentions.join(', ') }) }) : null),
      el('div', { class: 'actions' }, choose));
  };
  post.onchange = () => {
    if (!post.checked) set('post_summary', null);
    renderChat();
  };
  renderChat();
  return el('fieldset', { class: 'workflow-preapprove' }, el('legend', { textContent: t('What the supervisor may do without asking') }),
    el('p', { class: 'hint', textContent: t('By default every step the workflow supervisor proposes waits for you. Each box lets it act for this workflow\'s runs once code finds every condition; it is recorded on the run.') }),
    el('div', { class: 'choice-list' },
      el('label', { class: 'check' }, restart, el('span', {}, t('Restart a run whose coding session was cut'),
        el('small', { textContent: t('With its drafted handover note, once; never a run it restarted itself.') }))),
      el('label', { class: 'check' }, post, el('span', {}, t('Post its summary of a stopped run to a chat'),
        el('small', { textContent: t('As you, once per look, only to the chat and members chosen here, and only when the text passes the checks of an answer.') })))),
    chatLine);
}

/** The pre-approvals a save confirms, as list items. */
function preapproveLines(preapprove) {
  return (preapprove ?? []).map(p => el('li', {}, p.action === 'restart_after_cut'
    ? t('The supervisor may restart a run whose coding session was cut, with its drafted note.')
    : t('The supervisor may post its summary of a stopped run to {chat}, mentioning {names}.', { chat: p.chat?.name ?? '?', names: p.mentions?.length ? p.mentions.join(', ') : t('nobody') })));
}

/** What a save sends of the pre-approvals: the host's fields only. */
function preapproveSent(preapprove) {
  return (preapprove ?? []).map(p => p.action === 'post_summary' ? { action: p.action, chat: p.chat, mentions: p.mentions ?? [] } : { action: p.action });
}

// ---- Audits ----

const FROM_NAMES = { comment: () => t('a comment'), report: () => t('a report'), session: () => t('a session') };

/**
 * The supervisor's latest audit of a batch of runs, under the runs list, with «Ask for an audit»
 * while it is on: per run its result, the merged branches that still exist, the findings left only
 * in comments, the proposals and the drafted work items (each opened in Create request).
 * @param {HTMLElement} box
 * @param {boolean} on
 * @param {{ when: (seconds: number) => string }} shared
 */
async function renderAudit(box, on, shared) {
  let reply;
  try {
    reply = await invoke('workflow_supervision', { audit: 'latest' });
  } catch {
    box.replaceChildren();
    return;
  }
  const audit = reply.audit;
  const queued = reply.queued ?? [];
  const parts = [];
  if (on) {
    const ask = el('button', { type: 'button', class: 'secondary', id: 'runs-audit-ask', textContent: t('Ask for an audit'), disabled: queued.length > 0 });
    ask.onclick = () => busy(ask, async () => {
      await invoke('workflow_supervise', { audit: true });
      done(t('The supervisor is auditing the recent runs; the audit appears here.'));
      await renderAudit(box, on, shared);
    });
    parts.push(el('div', { class: 'actions' }, ask));
  }
  if (queued.length) parts.push(el('p', { class: 'hint', role: 'status', textContent: t('An audit is queued or running.') }));
  if (!audit) {
    box.replaceChildren(...(parts.length ? [el('section', { class: 'panel run-audit' }, el('h3', { textContent: t('Audit') }), el('p', { class: 'hint', textContent: t('No audit yet: the supervisor writes one when every run of a batch has ended.') }), ...parts)] : []));
    return;
  }
  const items = (audit.items ?? []).map(i => el('li', {}, link(`#${i.item} ${i.title}`, i.url), ' ', badge(runStateLabel(i.state), 'muted'),
    i.not_verified ? badge(tn(i.not_verified, '1 claim not verified', '{n} claims not verified'), 'warn') : null,
    i.result ? el('span', { textContent: ` ${i.result}` }) : null));
  const branches = (audit.merged_branches ?? []).map(b => el('li', { textContent: t('Branch {branch} in {repository} (pull request !{id} completed) still exists.', { branch: b.branch, repository: b.repository, id: String(b.pull_request) }) }));
  const findings = (audit.findings ?? []).map(f => el('li', {}, el('span', { textContent: f.text }), el('small', { textContent: ` · ${t('from {source}', { source: FROM_NAMES[f.from]?.() ?? f.from })}${f.needs_item ? ` · ${t('needs a work item')}` : ''}` })));
  const drafts = (audit.work_items ?? []).map(w => {
    const run = (audit.items ?? []).find(i => i.run === w.run);
    const open = el('button', { type: 'button', class: 'secondary small', textContent: t('Open Create request') });
    open.onclick = () => busy(open, () => openRequest({ provider: 'azure', project: run?.project, organization: run?.organization, kind: w.draft.type === 'bug' ? 'bug' : 'hu',
      context: [w.draft.title, w.draft.description, w.draft.repository ? t('Repository: {name}', { name: w.draft.repository.name }) : null].filter(Boolean).join('\n\n') }));
    return el('div', { class: 'resource' }, el('div', { class: 'resource-head' }, badge(w.draft.type === 'bug' ? t('Bug') : t('HU'), 'muted'), el('strong', { textContent: w.draft.title })),
      el('p', { textContent: w.draft.description }), el('div', { class: 'actions' }, open));
  });
  const fold = (title, list) => list.length ? el('details', { class: 'fold', open: true }, el('summary', { textContent: title }), list[0]?.tagName === 'LI' ? el('ul', {}, ...list) : el('div', { class: 'list' }, ...list)) : null;
  box.replaceChildren(el('section', { class: 'panel run-audit', id: 'run-audit' },
    el('div', { class: 'panel-head' }, el('h3', { textContent: t('Audit') }), el('span', { class: 'hint', textContent: `${shared.when(audit.created_at)} · ${audit.supervised_by?.model ?? ''}` })),
    audit.outcome !== 'done' ? el('div', { class: 'callout warn' }, el('p', {}, el('strong', { textContent: t('The audit gave no answer.') }), ' ', audit.failure ? failureText(audit.failure) : '')) : null,
    audit.overview ? el('p', { textContent: audit.overview }) : null,
    fold(tn(items.length, '1 run', '{n} runs'), items),
    fold(tn(branches.length, '1 merged branch still exists', '{n} merged branches still exist'), branches),
    fold(tn(findings.length, '1 finding left in comments', '{n} findings left in comments'), findings),
    fold(t('Proposals'), (audit.proposals ?? []).map(p => el('li', { textContent: p }))),
    fold(tn(drafts.length, '1 drafted work item', '{n} drafted work items'), drafts),
    ...parts));
}

export { act, proposal, supervision, preapproveField, preapproveLines, preapproveSent, renderAudit };
