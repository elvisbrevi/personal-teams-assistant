// Workflows › Blocks: the library of reusable blocks (instructions, Teams notifications and
// launchers) and their editor. A block that acts is confirmed with what it does when it is saved
// (`workflow_block_save` with `confirmed`); a launcher block only from an owner's transport.
import { t, tn, tx } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el } from './dom.js';
import { busy, done } from './notices.js';
import { showTab } from './nav.js';
import { applying } from './profile.js';
import { chooseFromList } from './directory-sheet.js';
import { changedAt, loadWorkflows, slug, workflowData } from './workflows.js';

const KIND_NAMES = { instruction: t('Instruction'), teams_notification: t('Teams notification'), launcher: t('Launcher') };
const STAGES = ['before_planning', 'plan_ready', 'children_created', 'child_done', 'pull_requests_completed', 'done', 'failed'];
const STAGE_NAMES = {
  before_planning: t('Before planning'), plan_ready: t('Plan ready for review'), children_created: t('Children created'),
  child_done: t('Each child done'), pull_requests_completed: t('Pull requests completed'), done: t('Run done'), failed: t('Run failed'),
};
/** The placeholders a notification's template may hold; code fills them in when it posts. */
const PLACEHOLDERS = {
  item: t('the work item\'s number and title'), id: t('the work item\'s number alone'), title: t('the work item\'s title alone'), link: t('its link'), pull_requests: t('the links of its pull requests'), summary: t('the director\'s progress summary'), state: t('the run\'s state'),
};
const LIMITS = { name: 80, text: 8000, template: 4000, mentions: 20, mention: 100, chat: 200 };
/** A name cut to the host's limit, with an ellipsis when it was longer. */
const fit = (text, max) => [...text].length > max ? `${[...text].slice(0, max - 1).join('').trimEnd()}…` : text;
/** The personal chat (Teams notes) is always the user's own. */
const PERSONAL_CHAT = '48:notes';

/** Working copies of blocks being edited (`new` for one not saved yet) and which folds are open. */
const drafts = new Map();
const openBlocks = new Set();
const outcomes = new Map();
/** The user's Teams chats (`teams_chats`), read once when a notification is edited. */
let chats = null;
/** The coordinator's approved launchers, and whether this transport may save a launcher block. */
let launcherState = { owner: false, launchers: [], error: '' };

const message = error => tx(String(error?.message ?? error));

/** Whether launchers can be referenced here: only the owner's transports list them. */
async function loadLaunchers() {
  try {
    const reply = await invoke('variable_consumer_list');
    launcherState = { owner: Array.isArray(reply.launchers), launchers: (reply.launchers ?? []).filter(l => l.approved), error: '' };
  } catch (error) {
    launcherState = { owner: false, launchers: [], error: message(error) };
  }
  return launcherState;
}

async function loadChats() {
  if (chats) return chats;
  const reply = await invoke('teams_chats', { limit: 500 });
  chats = reply.chats ?? [];
  return chats;
}

/** One line on what a block does, for rows and the workflow editor. */
function blockSummary(block) {
  if (block.kind === 'teams_notification') {
    return block.mentions?.length ? t('Posts to {chat}, mentioning {names}', { chat: block.chat?.name ?? '', names: block.mentions.join(', ') }) : t('Posts to {chat}', { chat: block.chat?.name ?? '' });
  }
  if (block.kind === 'launcher') return t('Runs the launcher {name}', { name: block.launcher?.id ?? '' });
  const text = (block.text ?? '').replace(/\s+/g, ' ').trim();
  return text.length > 90 ? `${text.slice(0, 90)}…` : text;
}

function renderBlocks() {
  const container = $('#block-rows');
  const { blocks, editable } = workflowData;
  $('#blocks-summary').textContent = blocks.length ? tn(blocks.length, '1 block', '{n} blocks') : '';
  $('#block-new').hidden = !editable;
  $('#block-new').disabled = drafts.has('new');
  const rows = [];
  if (editable && drafts.has('new')) rows.push(blockRow(drafts.get('new'), null));
  for (const block of blocks) rows.push(blockRow(drafts.get(block.id) ?? block, block));
  if (!rows.length) rows.push(el('div', { class: 'empty' }, el('p', { textContent: t('No blocks yet. Write an instruction such as «start from branch feature/222», a Teams notification or a launcher step, then add it to your workflows.') })));
  container.replaceChildren(...rows);
}

function usedBy(id) {
  return (workflowData.usedBy[id] ?? []).map(w => workflowData.workflows.find(x => x.id === w)?.name || w);
}

function blockRow(draft, saved) {
  const key = saved ? saved.id : 'new';
  const users = saved ? usedBy(saved.id) : [];
  const shown = saved ?? draft;
  const row = el('div', { class: 'resource block-row', 'data-block': key },
    el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: shown.name || t('New block') }),
      badge(KIND_NAMES[shown.kind], 'active'), shown.stage ? badge(STAGE_NAMES[shown.stage], 'muted') : null, el('span', { class: 'grow' }),
      saved ? badge(users.length ? tn(users.length, 'Used by 1 workflow', 'Used by {n} workflows') : t('Not used'), users.length ? 'info' : 'muted') : badge(t('Not saved yet'), 'info')),
    el('p', { class: 'resource-facts' }, el('span', { textContent: blockSummary(shown) || t('No text yet') }),
      el('small', { textContent: [users.length ? t('Used by: {names}', { names: users.join(', ') }) : null, saved ? t('Changed {when}', { when: changedAt(saved.updated_at) }) : null].filter(Boolean).join(' · ') })));
  if (!workflowData.editable) return row;
  const edit = el('details', { class: 'fold resource-edit', open: openBlocks.has(key) },
    el('summary', { textContent: saved ? t('Edit the block') : t('Write the new block') }));
  let built = false;
  const build = () => {
    if (built) return;
    built = true;
    if (!drafts.has(key)) drafts.set(key, structuredClone(saved));
    edit.append(blockEditor(drafts.get(key), saved));
  };
  edit.ontoggle = () => { if (edit.open) { openBlocks.add(key); build(); } else openBlocks.delete(key); };
  if (edit.open) build();
  row.append(edit);
  return row;
}

function counted(input, max) {
  const counter = el('small', { class: 'counter' });
  const count = () => {
    const length = [...input.value].length;
    counter.textContent = t('{n} of {max} characters', { n: length, max });
    counter.classList.toggle('over', length > max);
  };
  input.addEventListener('input', count);
  count();
  return counter;
}

/** The block as the host stores it: only the fields of its kind. */
function payload(draft, saved) {
  const base = { id: saved ? saved.id : draft.id, name: draft.name.trim(), kind: draft.kind, updated_at: saved?.updated_at ?? 0 };
  if (draft.kind === 'instruction') return { ...base, text: draft.text };
  if (draft.kind === 'teams_notification') return { ...base, stage: draft.stage, chat: draft.chat, mentions: draft.mentions, template: draft.template };
  return { ...base, stage: draft.stage, launcher: draft.launcher };
}

function unknownPlaceholders(template) {
  return [...template.matchAll(/\{([A-Za-z0-9_]+)\}/g)].map(m => m[1]).filter(name => !(name in PLACEHOLDERS));
}

function blockEditor(draft, saved) {
  const key = saved ? saved.id : 'new';
  draft.mentions ??= []; draft.text ??= ''; draft.template ??= '';
  const outcome = outcomes.get(key) ?? {};
  const problem = el('div', { class: 'callout error', role: 'alert', hidden: !outcome.error, textContent: outcome.error ?? '' });
  const warnings = el('div', { class: 'callout warn', role: 'status', hidden: !outcome.warnings?.length }, ...(outcome.warnings ?? []).map(text => el('p', { textContent: tx(text) })));
  const report = ({ error = '', list = [] }) => {
    outcomes.set(key, { error, warnings: list });
    problem.hidden = !error; problem.textContent = error;
    warnings.replaceChildren(...list.map(text => el('p', { textContent: tx(text) })));
    warnings.hidden = !list.length;
  };
  const confirmation = el('div', { class: 'callout info block-confirm', role: 'status', hidden: true });
  const save = el('button', { type: 'button' });
  let reviewing = false;
  const unreview = () => {
    reviewing = false;
    confirmation.hidden = true;
    save.textContent = draft.kind === 'instruction' ? t('Save block') : t('Review and save');
  };
  const touched = () => { draft.changed = true; unreview(); };

  const kind = el('select', {}, ...Object.entries(KIND_NAMES).map(([value, text]) => el('option', { value, textContent: text,
    disabled: value === 'launcher' && !launcherState.owner && draft.kind !== 'launcher' })));
  kind.value = draft.kind;
  const name = el('input', { value: draft.name, maxLength: LIMITS.name });
  const fields = el('div', { class: 'grid' }, el('label', {}, t('Kind'), kind), el('label', {}, t('Name'), name));
  let idInput = null;
  if (!saved) {
    idInput = el('input', { value: draft.id, maxLength: 64, translate: 'no', spellcheck: false, autocapitalize: 'none' });
    idInput.oninput = () => { draft.id = idInput.value.trim(); draft.idEdited = true; touched(); };
    fields.append(el('label', {}, t('Block ID'), el('small', { textContent: t('Lowercase letters, digits, - and _; it cannot change once saved.') }), idInput));
  }
  name.oninput = () => {
    draft.name = name.value;
    if (idInput && !draft.idEdited) { draft.id = slug(draft.name, workflowData.blocks.map(b => b.id), 'b'); idInput.value = draft.id; }
    touched();
  };
  const stage = el('select', {}, ...STAGES.map(value => el('option', { value, textContent: STAGE_NAMES[value] })));
  stage.onchange = () => { draft.stage = stage.value; touched(); };
  const stageField = el('label', {}, t('Stage'), el('small', { textContent: t('When in a run the block acts; each block acts at most once each time that stage occurs.') }), stage);
  const body = el('div', {});

  const instructionFields = () => {
    const text = el('textarea', { value: draft.text, rows: 6, maxLength: LIMITS.text });
    text.oninput = () => { draft.text = text.value; touched(); };
    return [el('label', {}, t('Instruction'), el('small', { textContent: t('What the planning and coding agents read, as untrusted input under the fixed rules: it cannot let them write work items, post to Teams, merge or deploy; only what the workflow allows can.') }), text, counted(text, LIMITS.text))];
  };

  const notificationFields = () => {
    const chatProblem = el('p', { class: 'callout error', role: 'alert', hidden: true });
    const chat = el('button', { type: 'button', class: 'secondary request-target', 'aria-haspopup': 'dialog', id: `block-chat-${key}`,
      'aria-labelledby': `block-chat-label-${key} block-chat-${key}`, textContent: draft.chat?.name || t('Choose a chat') });
    const mentions = el('fieldset', {}, el('legend', { textContent: t('Mention') }));
    const renderMentions = () => {
      const record = draft.chat?.id === PERSONAL_CHAT ? { members: [] } : chats?.find(c => c.id === draft.chat?.id);
      const members = [...new Set([...(record?.members ?? []), ...draft.mentions])];
      mentions.replaceChildren(el('legend', { textContent: t('Mention') }), ...members.map(member => {
        const box = el('input', { type: 'checkbox', checked: draft.mentions.includes(member) });
        box.onchange = () => {
          draft.mentions = box.checked ? [...draft.mentions, member] : draft.mentions.filter(m => m !== member);
          touched(); renderMentions();
        };
        const missing = record && !record.members.includes(member);
        return el('label', { class: 'check' }, box, el('span', {}, member, missing ? el('small', { textContent: ` · ${t('not in this chat')}` }) : null));
      }));
      if (!members.length) mentions.append(el('p', { class: 'hint', textContent: draft.chat ? record ? t('Nobody else is in this chat.') : t('Choose the chat again to list its members.') : t('Choose the chat first.') }));
      if (draft.mentions.length > LIMITS.mentions) mentions.append(el('p', { class: 'callout warn', textContent: t('Mention at most {n} members.', { n: LIMITS.mentions }) }));
    };
    chat.onclick = () => busy(chat, async () => {
      chatProblem.hidden = true;
      try { await loadChats(); } catch (error) { chatProblem.hidden = false; chatProblem.textContent = message(error); return; }
      const items = [{ value: PERSONAL_CHAT, label: t('Your personal chat') }, ...chats.map(c => ({ value: c.id, label: c.topic || c.members.join(', '), detail: c.members.length ? tn(c.members.length, '1 member', '{n} members') : '' }))];
      chooseFromList({ trigger: chat, title: t('Choose a chat'), allLabel: t('Your chats'), empty: t('No matching chats.'),
        countText: n => tn(n, '{n} chat', '{n} chats'), items, selected: draft.chat ? [draft.chat.id] : [],
        apply: ([id]) => {
          const item = items.find(i => i.value === id);
          if (!item) return;
          if (draft.chat?.id !== id) draft.mentions = [];
          // A chat without a topic is named by its members, which can be longer than a name may be.
          draft.chat = { id, name: fit(item.label, LIMITS.chat) };
          chat.textContent = draft.chat.name;
          touched(); renderMentions();
        } });
    });
    renderMentions();
    // Members are listed once the user's chats are read.
    if (draft.chat && !chats && draft.chat.id !== PERSONAL_CHAT) loadChats().then(renderMentions, () => {});
    const template = el('textarea', { value: draft.template, rows: 5, maxLength: LIMITS.template });
    const placeholderProblem = el('p', { class: 'callout error', role: 'alert', hidden: true });
    const checkPlaceholders = () => {
      const unknown = unknownPlaceholders(template.value);
      placeholderProblem.hidden = !unknown.length;
      placeholderProblem.textContent = unknown.length ? t('Unknown placeholders: {names}. Use only the ones listed.', { names: unknown.map(n => `{${n}}`).join(', ') }) : '';
    };
    template.oninput = () => { draft.template = template.value; checkPlaceholders(); touched(); };
    const insert = name => {
      const at = template.selectionStart ?? template.value.length;
      const end = template.selectionEnd ?? at;
      template.setRangeText(`{${name}}`, at, end, 'end');
      draft.template = template.value; checkPlaceholders(); touched();
      template.focus();
    };
    const placeholders = el('div', { class: 'placeholder-list' }, ...Object.entries(PLACEHOLDERS).map(([name, meaning]) => {
      const button = el('button', { type: 'button', class: 'secondary small', translate: 'no', textContent: `{${name}}`, title: meaning,
        'aria-label': `{${name}}: ${meaning}` });
      button.onclick = () => insert(name);
      return button;
    }));
    checkPlaceholders();
    return [el('div', { class: 'field-block' }, el('label', { id: `block-chat-label-${key}`, textContent: t('Teams chat') }), chat, chatProblem,
      el('small', { class: 'hint', textContent: t('Only chats you belong to; channels are not offered. It posts once each time its stage occurs, as you.') })),
    mentions,
    el('label', {}, t('Message'), el('small', { textContent: t('The members you mention are matched to the chat\'s members again when it posts.') }), template, counted(template, LIMITS.template)),
    el('p', { class: 'hint', textContent: t('Placeholders: {item} the work item, {link} its link, {pull_requests} its pull requests, {summary} the director\'s summary, {state} the run\'s state. Select one to insert it.') }),
    placeholders, placeholderProblem];
  };

  const launcherFields = () => {
    if (!launcherState.owner) {
      return [el('p', { class: 'callout info', textContent: t('Only you save a launcher block, at this computer\'s local page or in your own portal session. Open one of them to choose the launcher.') })];
    }
    if (!launcherState.launchers.length) {
      const open = el('button', { type: 'button', class: 'link-button', textContent: t('Open local programs in Settings') });
      open.onclick = () => showTab('settings', 'panel-consumers');
      return [el('p', { class: 'hint', textContent: launcherState.error || t('There is no approved launcher on this computer yet. Define one with pta, review it and approve it in Settings › Variables, then choose it here.') }), el('div', { class: 'actions' }, open)];
    }
    const choice = el('select', {}, el('option', { value: '', textContent: t('Choose a launcher') }),
      ...launcherState.launchers.map(l => el('option', { value: l.id, textContent: l.id })));
    choice.value = launcherState.launchers.some(l => l.id === draft.launcher?.id) ? draft.launcher.id : '';
    const facts = el('dl', { class: 'facts' });
    const renderFacts = () => {
      const launcher = launcherState.launchers.find(l => l.id === choice.value);
      const stale = draft.launcher && launcher && launcher.digest !== draft.launcher.digest;
      facts.replaceChildren(...(launcher ? [
        el('dt', { textContent: t('Program') }), el('dd', { translate: 'no', textContent: [launcher.program, ...(launcher.args ?? [])].join(' ') }),
        el('dt', { textContent: t('Program digest') }), el('dd', {}, el('code', { translate: 'no', textContent: launcher.digest })),
        el('dt', { textContent: t('Description') }), el('dd', { textContent: launcher.description || '—' }),
      ] : []));
      if (stale) facts.append(el('dt', { textContent: t('Saved digest') }), el('dd', { class: 'warn' }, el('code', { translate: 'no', textContent: draft.launcher.digest })));
    };
    choice.onchange = () => {
      const launcher = launcherState.launchers.find(l => l.id === choice.value);
      draft.launcher = launcher ? { id: launcher.id, digest: launcher.digest } : null;
      touched(); renderFacts();
    };
    renderFacts();
    const missing = draft.launcher && !launcherState.launchers.some(l => l.id === draft.launcher.id);
    return [el('label', {}, t('Approved launcher'), el('small', { textContent: t('A program you approved on this computer, the account\'s coordinator. It runs with its own timeout, only while it stays approved with this digest.') }), choice),
      missing ? el('p', { class: 'callout warn', textContent: t('The saved launcher {name} is no longer approved here; choose an approved one.', { name: draft.launcher.id }) }) : null, facts];
  };

  const renderBody = () => {
    stageField.hidden = draft.kind === 'instruction';
    if (draft.kind !== 'instruction' && !draft.stage) draft.stage = draft.kind === 'launcher' ? 'pull_requests_completed' : 'done';
    stage.value = draft.stage ?? 'done';
    body.replaceChildren(...(draft.kind === 'instruction' ? instructionFields() : draft.kind === 'teams_notification' ? notificationFields() : launcherFields()));
    unreview();
  };
  kind.onchange = () => { draft.kind = kind.value; draft.changed = true; renderBody(); };

  /** What the user confirms for a block that acts, in their language. */
  const describe = () => {
    const at = STAGE_NAMES[draft.stage];
    if (draft.kind === 'teams_notification') {
      return [el('p', { textContent: draft.mentions.length
        ? t('At «{stage}», this block posts the message below to the Teams chat «{chat}», mentioning {names}, once each time that stage occurs:', { stage: at, chat: draft.chat.name, names: draft.mentions.join(', ') })
        : t('At «{stage}», this block posts the message below to the Teams chat «{chat}», once each time that stage occurs:', { stage: at, chat: draft.chat.name }) }),
      el('pre', { textContent: draft.template })];
    }
    const launcher = launcherState.launchers.find(l => l.id === draft.launcher.id);
    return [el('p', { textContent: t('At «{stage}», this block runs your approved launcher {name} ({program}) with program digest {digest}.', { stage: at, name: draft.launcher.id, program: launcher?.program ?? '', digest: draft.launcher.digest }) })];
  };
  const missing = () => {
    const list = [];
    if (!draft.name.trim()) list.push(t('Write its name.'));
    if (!saved && !/^[a-z][a-z0-9_-]{0,63}$/.test(draft.id)) list.push(t('Give it an ID of lowercase letters, digits, - and _, starting with a letter.'));
    if (!saved && workflowData.blocks.some(b => b.id === draft.id)) list.push(t('Another block has the ID {id}.', { id: draft.id }));
    if (draft.kind === 'instruction' && !draft.text.trim()) list.push(t('Write the instruction.'));
    if (draft.kind === 'instruction' && [...draft.text].length > LIMITS.text) list.push(t('Shorten the instruction.'));
    if (draft.kind === 'teams_notification') {
      if (!draft.chat) list.push(t('Choose the chat.'));
      else if (!draft.chat.name.trim() || [...draft.chat.name].length > LIMITS.chat) list.push(t('Choose the chat again: its name must have 1 to {n} characters.', { n: LIMITS.chat }));
      if (draft.mentions.some(m => [...m].length > LIMITS.mention || m.includes(']'))) list.push(t('Mention only members whose names have at most {n} characters and no closing bracket.', { n: LIMITS.mention }));
      if (!draft.template.trim()) list.push(t('Write the message.'));
      if ([...draft.template].length > LIMITS.template) list.push(t('Shorten the message.'));
      if (unknownPlaceholders(draft.template).length) list.push(t('Use only the listed placeholders.'));
      if (draft.mentions.length > LIMITS.mentions) list.push(t('Mention at most {n} members.', { n: LIMITS.mentions }));
    }
    if (draft.kind === 'launcher') {
      if (!launcherState.owner) list.push(t('Save launcher blocks at this computer\'s local page or in your own portal session.'));
      else if (!draft.launcher || !launcherState.launchers.some(l => l.id === draft.launcher.id)) list.push(t('Choose an approved launcher.'));
    }
    return list;
  };
  save.onclick = () => busy(save, async () => {
    const list = missing();
    if (list.length) { report({ error: `${t('Before saving:')} ${list.join(' ')}` }); return; }
    report({});
    const acting = draft.kind !== 'instruction';
    if (acting && !reviewing) {
      reviewing = true;
      confirmation.replaceChildren(...describe());
      confirmation.hidden = false;
      save.textContent = t('Confirm and save');
      return;
    }
    const block = payload(draft, saved);
    try {
      const reply = await applying(() => invoke('workflow_block_save', { block, confirmed: acting }));
      const result = reply.block ?? block;
      outcomes.set(result.id, { error: '', warnings: reply.warnings ?? [] });
      drafts.delete('new'); outcomes.delete('new');
      drafts.set(result.id, structuredClone(result));
      openBlocks.delete('new'); openBlocks.add(result.id);
      done(saved ? t('Block {name} saved.', { name: result.name }) : t('Block {name} created.', { name: result.name }));
      await loadWorkflows();
      $(`[data-block="${result.id}"] summary`)?.focus({ preventScroll: true });
    } catch (error) {
      unreview();
      report({ error: message(error) });
    }
  });

  const actions = [];
  if (saved) {
    const remove = el('button', { type: 'button', class: 'danger', textContent: t('Remove block') });
    let armed = null;
    remove.onclick = () => busy(remove, async () => {
      const users = usedBy(saved.id);
      if (users.length) { report({ error: t('This block is used by {names}; take it out of them first.', { names: users.join(', ') }) }); return; }
      if (!armed) {
        remove.textContent = t('Tap again to remove');
        armed = setTimeout(() => { armed = null; remove.textContent = t('Remove block'); }, 4000);
        return;
      }
      clearTimeout(armed); armed = null;
      try {
        await applying(() => invoke('workflow_block_remove', { id: saved.id }));
        drafts.delete(saved.id); openBlocks.delete(saved.id); outcomes.delete(saved.id);
        done(t('Block {name} removed.', { name: saved.name }));
        await loadWorkflows();
        $('#block-new').focus({ preventScroll: true });
      } catch (error) { remove.textContent = t('Remove block'); report({ error: message(error) }); }
    });
    actions.push(remove, el('span', { class: 'grow' }));
  } else {
    const discard = el('button', { type: 'button', class: 'secondary', textContent: t('Discard') });
    discard.onclick = () => { drafts.delete('new'); outcomes.delete('new'); openBlocks.delete('new'); renderBlocks(); $('#block-new').focus(); };
    actions.push(discard);
  }
  actions.push(save);
  renderBody();
  return el('div', { class: 'block-editor' }, fields, stageField, body,
    launcherState.owner || draft.kind === 'launcher' ? null : el('p', { class: 'hint', textContent: t('Launcher blocks are saved only at this computer\'s local page or in your own portal session.') }),
    problem, warnings, confirmation, el('div', { class: 'actions' }, ...actions));
}

function newBlock() {
  if (drafts.has('new')) return;
  drafts.set('new', { id: '', name: '', kind: 'instruction', text: '', mentions: [], template: '', chat: null, launcher: null, updated_at: 0, changed: true });
  openBlocks.add('new');
  renderBlocks();
  $('[data-block="new"] select')?.focus();
}

$('#block-new').onclick = newBlock;

export { KIND_NAMES, STAGE_NAMES, PLACEHOLDERS, blockSummary, loadLaunchers, renderBlocks };
