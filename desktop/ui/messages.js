// Messages: received messages, their answers, sources and feedback.
import { t, tn, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el, field, sleep } from './dom.js';
import { current, live } from './state.js';
import { busy, done, fail, notify } from './notices.js';
import { selectTab, showTab } from './nav.js';
import { link, markdown } from './markdown.js';
import { kindName, noteOrigin, setNoteOrigin } from './knowledge.js';
import { openPeople, questionAssociation, renderPeople, sourceTitle } from './knowledge-map.js';

// ---------------------------------------------------------------- messages

const statusLabels = {
  sent: [t('Answered'), 'ok'], dry_run: [t('Observed (not sent)'), 'info'], sending: [t('Sending'), 'active'],
  processing: [t('In progress'), 'active'], pending: [t('Queued'), 'active'], failed: [t('Error'), 'error'],
  uncertain: [t('Uncertain send'), 'error'], refining: [t('Generating'), 'active'],
};
const reasonLabels = {
  question_associated: t('The message was associated with a question. Generate an answer to review it.'),
  no_matching_question: t('No configured question matched. Associate it with a question to handle similar messages.'),
  question_matching_unavailable: t('The question could not be identified. Left for review.'),
  question_sources_unavailable: t('The question was identified, but none of its sources is enabled and authorized for this conversation.'),
  rejected_answer_repeated: t('The answer repeats a response you rejected for this question. Left for review.'),
  ineligible_message: t('Not addressed to the assistant: your own, from a group without a mention, too old, empty or an answer of the app.'),
  sensitive_question: t('The question contains sensitive data; it is left to you.'),
  no_authorized_resource: t('No source is authorized for this conversation.'),
  informational_message: t('Informational message: it asks for no answer.'),
  personal_request: t('It asks you for a call, a meeting, a joint review or your availability: it is left to you.'),
  deterministic_greeting: t('Greeting answered with the configured text.'),
  deterministic_thanks: t('Shared link: the sender was thanked.'),
  supported_answer: t('Answer generated.'),
  unsafe_proposal: t('Withheld: it contained sensitive data or did not fit in a Teams message.'),
  unsafe_answer: t('Withheld: it contained sensitive data or did not fit in a Teams message.'),
  reference_selection_failed: t('Withheld while choosing references (earlier version).'),
  final_gate: t('Withheld by the final review (earlier version).'),
  message_changed: t('The message changed, was deleted or stopped being eligible before sending.'),
  send_result_unknown_manual_review: t('It is unknown whether the send arrived: check it in Teams (never retried).'),
  read_or_provider_error: t('Error reading Teams or calling a provider; it is retried.'),
  generating: t('Writing the answer.'), retrieving: t('Searching the sources.'), tool_started: t('Reading a tool.'),
  selecting_references: t('Choosing references.'), final_review: t('Final review.'),
  holding_reply: t('Sending the holding notice.'),
  linking: t('Linking work to work items.'),
  links_planned: t('Link plan shown; waiting for your confirmation.'),
  links_applied: t('Confirmed links written to Azure DevOps (never retried).'),
  links_cancelled: t('Link plan cancelled; nothing was linked.'),
  links_unresolved: t('The link request could not be resolved; nothing was planned.'),
  links_not_configured: t('Linking asked for, but no source has a write credential.'),
  refining: t('Writing the answer at your request.'),
  draft_ready: t('Answer ready: review it and send it from here.'),
  refinement_failed: t('The answer could not be written again; the earlier proposal is kept.'),
  manual_send: t('Sent by you from the app after reviewing it.'),
};
const kindLabels = { self: t('Personal chat'), direct: t('Direct'), group: t('Group'), unsupported: t('Other') };
const intentLabels = { report_tasks: t('Report tasks'), question: t('question'), activity_review: t('activity review'), greeting: t('greeting'), shared_link: t('shared link'), link: t('link work to work items'),
  statement: t('informational'), personal: t('request for you') };
const linkLabels = { sending: t('sending'), linked: t('linked'), already_linked: t('already linked'), failed: t('failed'), uncertain: t('uncertain (check the work item; never retried)') };
let messageRows = [];
let messageFilter = 'all';
const openMessages = new Set();
/** The message to scroll to once Messages renders it (opened from Home). */
let focusMessage = null;

function messageState(row) {
  const reason = row.audit.reason || '';
  if (row.status === 'ignored') {
    if (reason === 'ineligible_message') return [t('Not addressed'), 'muted'];
    if (reason === 'draft_ready') return [t('Draft ready'), 'info'];
    if (/^(unsafe_|invalid_references|reference_selection_failed|final_gate|refinement_failed)/.test(reason)) return [t('Withheld'), 'warn'];
    return [t('No answer'), 'muted'];
  }
  return statusLabels[row.status] || [row.status, 'muted'];
}

function reasonText(reason, audit) {
  if (!reason) return '';
  if (reason === 'refinement_failed' && !audit?.proposed?.trim()) return t('The answer could not be written; try again.');
  if (reason.startsWith('invalid_references')) return t('Withheld: references or links that could not be verified ({detail}).', { detail: reason.replace('invalid_references: ', '') });
  return reasonLabels[reason] || reason;
}

const millis = row => row.audit.received_at ?? row.created_at * 1000;
const relative = new Intl.RelativeTimeFormat(uiLocale(), { numeric: 'auto' });
function ago(at) {
  const seconds = Math.round((at - Date.now()) / 1000);
  for (const [unit, size] of /** @type {[Intl.RelativeTimeFormatUnit, number][]} */ ([['day', 86400], ['hour', 3600], ['minute', 60]])) {
    if (Math.abs(seconds) >= size) return relative.format(Math.round(seconds / size), unit);
  }
  return t('just now');
}
const when = row => new Date(millis(row)).toLocaleString(uiLocale());
const whenShort = row => new Date(millis(row)).toLocaleString(uiLocale(), { dateStyle: 'medium', timeStyle: 'short' });
const ineligible = row => row.audit.reason === 'ineligible_message' && row.status === 'ignored';
/** Lowercase without accents, so «revision» finds «revisión». */
const folded = text => String(text ?? '').normalize('NFD').replace(/\p{Diacritic}/gu, '').toLowerCase();
/** Whether a message's recorded text, outcome or kind contains every searched word. */
function matchesSearch(row, query) {
  const words = folded(query).split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const audit = row.audit;
  const text = folded([audit.question, audit.resolved_question, audit.topic, audit.proposed, audit.sent, audit.refinement,
    audit.provider, reasonText(audit.reason), messageState(row)[0], kindOf(row)].join(' '));
  return words.every(word => text.includes(word));
}

function excerpt(row) {
  const audit = row.audit;
  const reply = audit.sent || audit.proposed;
  return audit.question
    || (reply ? `${t('Answer')}: ${reply.replace(/\s+/g, ' ').slice(0, 140)}` : null)
    || (audit.reason === 'ineligible_message' ? t('(the text of messages not addressed to the assistant is not stored)') : t('(no text recorded)'));
}

function kindOf(row) {
  return kindLabels[row.audit.conversation_kind] || (row.resource.includes('48:notes') ? t('Personal chat') : row.resource.startsWith('simulation:') ? t('Simulation') : 'Teams');
}

/** @param {boolean | 'markdown'} [pre] */
function messageField(container, label, text, pre = false) {
  if (!text) return;
  const body = pre === 'markdown' ? markdown(text) : el(pre ? 'pre' : 'p', { textContent: text });
  container.append(el('div', { class: 'field-block' }, el('strong', { textContent: label }), body));
}

/** What the answer cited and the Wiki pages it read without citing: the answer links them in
 *  its text, and this record keeps the full list. */
function referencesField(container, audit) {
  const cited = new Set(audit.used_sources || []);
  const seen = new Set();
  const once = ref => !seen.has(ref.url) && seen.add(ref.url);
  const used = (audit.references || []).filter(ref => cited.has(ref.id) && once(ref));
  const pages = (audit.references || []).filter(ref => ref.kind === 'wiki' && !cited.has(ref.id) && once(ref));
  const list = refs => el('ul', {}, ...refs.map(ref => el('li', {}, link(ref.label || ref.id, ref.url))));
  if (used.length) container.append(el('div', { class: 'field-block' }, el('strong', { textContent: t('Cited references') }), list(used)));
  if (pages.length) container.append(el('div', { class: 'field-block' }, el('strong', { textContent: t('Wiki pages read, not cited') }), list(pages)));
}

function matchesFilter(row, filter) {
  const [, tone] = messageState(row);
  return filter === 'all' || (filter === 'sent' && tone === 'ok')
    || (filter === 'withheld' && (tone === 'warn' || row.audit.reason === 'draft_ready' || (tone === 'muted' && row.audit.reason !== 'ineligible_message')))
    || (filter === 'error' && tone === 'error') || (filter === 'active' && tone === 'active');
}

/** Context typed for an answer, kept across the list's refreshes. */
const answerContext = new Map();
/** Sources chosen for an answer (resource → ids), kept across refreshes; absent means its last choice. */
const answerSources = new Map();
/** Messages whose source chooser is open. */
const openSourceChoices = new Set();
let sendArmed = null;
/** A message addressed to the assistant that was not sent an answer can get one written from here
 *  (one with sensitive data never reaches a model). */
const unsent = row => ['ignored', 'dry_run'].includes(row.status) && !row.audit.sent && !!row.audit.question
  && row.audit.reason !== 'sensitive_question' && !row.resource.startsWith('simulation:');
/** An unsent message whose answer exists (withheld, observed or written here) can be sent. */
const unsentDraft = row => unsent(row) && !!row.audit.proposed?.trim();

/** The sources the user may choose for an answer: enabled and allowed external processing, like the
 *  test chat. None chosen means the ones authorized for the conversation. */
function sourceChooser(row) {
  const usable = live.map.resources.filter(r => r.enabled && r.external_processing);
  const chosen = answerSources.get(row.resource) ?? new Set((row.audit.chosen_sources || []).filter(id => usable.some(r => r.id === id)));
  const conversation = row.resource.replace(/\/messages\/.*$/, '');
  const authorized = r => r.allowed_conversations.some(c => c === conversation || c === '*');
  const summaryNote = el('small');
  const describe = () => {
    summaryNote.textContent = chosen.size ? tn(chosen.size, '1 chosen', '{n} chosen') : t('Automatic');
  };
  describe();
  const fold = el('details', { class: 'fold answer-sources', open: openSourceChoices.has(row.resource) },
    el('summary', {}, t('Sources'), summaryNote));
  fold.ontoggle = () => { if (fold.open) openSourceChoices.add(row.resource); else openSourceChoices.delete(row.resource); };
  if (!usable.length) {
    fold.append(el('p', { class: 'hint', textContent: t('No sources are ready. In Knowledge, enable one, authorize its external processing and save.') }));
    return fold;
  }
  fold.append(el('p', { class: 'hint', textContent: t('Choose what the assistant reads for this answer. With none chosen, it uses the sources authorized for this chat.') }));
  const list = el('div', { class: 'choices' });
  for (const resource of usable) {
    const box = el('input', { type: 'checkbox', value: resource.id, checked: chosen.has(resource.id) });
    box.onchange = () => {
      if (box.checked) chosen.add(resource.id); else chosen.delete(resource.id);
      answerSources.set(row.resource, chosen);
      describe();
    };
    list.append(el('label', { class: 'check' }, box,
      el('span', {}, el('strong', { textContent: resource.id }),
        el('small', { textContent: `${kindName(resource)} · ${resource.description}${authorized(resource) ? ` · ${t('authorized for this chat')}` : ''}` }))));
  }
  fold.append(list);
  return fold;
}

/** Generate the answer to an unanswered message, or once it exists: regenerate it with context, or send it. */
function answerActions(row) {
  const drafted = unsentDraft(row);
  const sources = sourceChooser(row);
  const context = drafted ? el('textarea', { class: 'answer-context', rows: 3, maxLength: 4000,
    placeholder: t('E.g. It is already in validation and Alex owns it; the production deployment is planned for 10/20 and the production variables are still missing.'),
    value: answerContext.get(row.resource) || '' }) : null;
  if (context) {
    context.setAttribute('data-resource', row.resource);
    context.oninput = () => answerContext.set(row.resource, context.value);
  }
  const generate = el('button', { class: drafted ? 'secondary' : '', type: 'button', textContent: drafted ? t('Regenerate answer') : t('Generate answer') });
  generate.onclick = () => busy(generate, async () => {
    const chosen = [...sources.querySelectorAll('input:checked')].map(box => box.value);
    await invoke('answer_refine', { resource: row.resource, context: context?.value ?? '', sources: chosen });
    answerContext.delete(row.resource);
    answerSources.delete(row.resource);
    notify(t('Writing the answer…'), 'info');
    await followRefinement(row.resource);
  });
  if (!drafted) {
    return el('div', { class: 'field-block draft-actions' },
      el('strong', { textContent: t('Answer this message') }),
      el('p', { class: 'hint', textContent: t('The assistant reads the conversation and the sources and writes an answer for you to review. Nothing is sent until you send it.') }),
      sources,
      el('div', { class: 'actions' }, generate));
  }
  const send = el('button', { type: 'button', textContent: t('Send answer') });
  send.onclick = () => {
    // Sending is final: the first tap asks, the second one sends.
    if (sendArmed !== row.resource) {
      sendArmed = row.resource;
      send.textContent = t('Tap again to send');
      setTimeout(() => {
        if (sendArmed === row.resource) { sendArmed = null; send.textContent = t('Send answer'); }
      }, 4000);
      return;
    }
    sendArmed = null;
    send.textContent = t('Send answer');
    busy(send, async () => {
      const result = await invoke('answer_send', { resource: row.resource });
      if (result.status === 'sent') done(t('Answer sent to Teams.'));
      else notify(t('It is unknown whether the send arrived: check it in Teams (never retried).'), 'error');
      await loadMessages();
    });
  };
  return el('div', { class: 'field-block draft-actions' },
    el('strong', { textContent: t('Review and send') }),
    el('label', {}, t('Context for the assistant'), context),
    sources,
    el('div', { class: 'actions' }, generate, send),
    el('p', { class: 'hint', textContent: t('Regenerating writes the answer again with your context and the chosen sources, without sending it. Sending posts the answer shown above once, as you.') }));
}

/** An answer exists (sent or proposed) and the message is not being processed. */
const rateable = row => !!(row.audit.sent || row.audit.proposed)?.trim()
  && !['pending', 'processing', 'refining', 'sending'].includes(row.status);
/** Ratings being edited, kept across refreshes: resource → { rating, comment, example, context }. */
const feedbackDrafts = new Map();

/** Rate an answer, keep it as an example for similar requests, or save it as a verified note. */
function feedbackBlock(row) {
  const saved = row.audit.feedback ?? {};
  const draft = feedbackDrafts.get(row.resource) ?? { rating: saved.rating ?? null, comment: saved.comment ?? '', example: !!saved.example, context: saved.context ?? '' };
  const remember = () => feedbackDrafts.set(row.resource, draft);
  const ratings = el('div', { class: 'segmented', role: 'group', 'aria-label': t('Rate this answer') });
  const example = el('input', { type: 'checkbox', checked: draft.example });
  const exampleRow = el('label', { class: 'check' }, example, t('Use it as an example for similar requests'));
  const contexts = current?.map?.contexts ?? [];
  const context = el('select', { 'aria-label': t('Context of the example') },
    el('option', { value: '', textContent: t('Every context') }),
    ...contexts.map(c => el('option', { value: c.id, textContent: c.name || c.id, selected: draft.context === c.id })));
  const exampleFields = el('div', { class: 'example-fields' }, exampleRow, ...(contexts.length ? [context] : []),
    el('p', { class: 'hint', textContent: t('An example guides the structure and tone of later answers in any chat, never their facts. Do not choose answers with private details.') }));
  const sync = () => {
    for (const button of ratings.querySelectorAll('button')) button.setAttribute('aria-pressed', String(button.dataset.rating === draft.rating));
    exampleFields.hidden = !!row.audit.identified_question || draft.rating !== 'good';
  };
  for (const [rating, label] of [['good', t('Good answer')], ['bad', t('Needs work')]]) {
    const button = el('button', { type: 'button', textContent: label });
    button.dataset.rating = rating;
    button.onclick = () => { draft.rating = draft.rating === rating ? null : rating; if (draft.rating !== 'good') draft.example = false; example.checked = draft.example; remember(); sync(); };
    ratings.append(button);
  }
  example.onchange = () => { draft.example = example.checked; remember(); };
  context.onchange = () => { draft.context = context.value; remember(); };
  const comment = el('textarea', { class: 'feedback-comment', rows: 2, maxLength: 2000, value: draft.comment,
    placeholder: t('What was right or what should change') });
  comment.setAttribute('data-resource', row.resource);
  comment.oninput = () => { draft.comment = comment.value; remember(); };
  const save = el('button', { type: 'button', class: 'secondary', textContent: t('Save rating') });
  save.onclick = () => busy(save, async () => {
    await invoke('answer_feedback', { resource: row.resource, rating: draft.rating, comment: draft.comment.trim(), example: draft.example, context: draft.context || null });
    feedbackDrafts.delete(row.resource);
    done(draft.rating ? t('Rating saved.') : t('Rating cleared.'));
    if (!row.resource.startsWith('simulation:')) await loadMessages();
  });
  const asNote = el('button', { type: 'button', class: 'secondary', textContent: t('Save as a note') });
  asNote.onclick = () => noteFromAnswer(row);
  sync();
  return el('div', { class: 'field-block feedback' },
    el('strong', { textContent: t('Your rating') }),
    ratings, exampleFields,
    ...(row.audit.identified_question ? [el('p', { class: 'hint', textContent: t('Good ratings suggest this approach. Bad ratings tell the assistant what to avoid for this question. Current facts always come from the selected sources.') })] : []),
    el('label', {}, t('Comment'), comment),
    el('div', { class: 'actions' }, asNote, save));
}

/** Opens Knowledge › Notes with the answer as a new verified note. */
function noteFromAnswer(row) {
  const answer = (row.audit.sent || row.audit.proposed || '').replace(/\[([^\]\n]+)\]\([^)\s]*\)/g, '$1');
  showTab('knowledge');
  selectTab($('#knowledge-tabs'), 'notes');
  $('#note-form').open = true;
  setNoteOrigin({ origin: 'answer', reference: row.resource });
  field('#note-title', (row.audit.resolved_question || row.audit.question || '').slice(0, 200));
  field('#note-text', answer);
  $('#note-title').focus();
}

/** Opens the profile of the person who wrote, or a new one with their name and Teams ID. */
function personLink(row) {
  const known = (current?.map?.people ?? []).find(p => p.id === row.audit.person);
  const button = el('button', { type: 'button', class: 'link-button', textContent: known
    ? t('How you address {name}', { name: known.name }) : t('Add {name} to People', { name: row.audit.sender_name }) });
  button.onclick = () => {
    showTab('knowledge');
    selectTab($('#knowledge-tabs'), 'people');
    if (known) { openPeople.add(known.id); renderPeople(); return; }
    $('#person-form').open = true;
    field('#person-name', row.audit.sender_name);
    field('#person-sender', row.audit.sender_id ?? '');
    $('#person-address').focus();
  };
  return el('div', { class: 'field-block' }, button);
}

/** Follows an answer being written until it is ready, kept or failed. */
async function followRefinement(resource) {
  const deadline = Date.now() + 25 * 60_000;
  while (Date.now() < deadline) {
    await sleep(3000);
    await loadMessages();
    const row = messageRows.find(r => r.resource === resource);
    if (row?.status === 'refining') continue;
    if (row?.audit.reason === 'draft_ready') done(t('Answer ready: review it and send it from here.'));
    else if (row?.audit.reason === 'refinement_failed') fail(reasonText(row.audit.reason, row.audit));
    else if (row) notify(reasonText(row.audit.reason, row.audit), 'info');
    return;
  }
}

function renderMessages() {
  const list = $('#messages-list');
  // A refresh rebuilds the list: the context being typed keeps its focus and cursor.
  const typing = ['answer-context', 'feedback-comment'].some(name => document.activeElement?.classList?.contains(name)) ? /** @type {HTMLTextAreaElement} */ (document.activeElement) : null;
  const typingAt = typing ? [typing.dataset.resource, typing.selectionStart, typing.selectionEnd] : null;
  const hideIneligible = $('#messages-hide-ineligible').checked;
  const query = $('#messages-search').value;
  const visible = messageRows.filter(row => !(hideIneligible && ineligible(row)) && matchesSearch(row, query));
  const rows = visible.filter(row => matchesFilter(row, messageFilter));
  // Each filter shows how many of the visible messages it holds.
  for (const button of document.querySelectorAll('#messages-filters button')) {
    const count = visible.filter(row => matchesFilter(row, button.dataset.filter)).length;
    button.querySelector('.count').textContent = count ? String(count) : '';
  }
  $('#messages-summary').textContent = rows.length === messageRows.length
    ? t('{n} recent messages', { n: messageRows.length })
    : t('Showing {shown} of {total} recent messages', { shown: rows.length, total: messageRows.length });
  list.replaceChildren();
  if (!rows.length) {
    const searching = Boolean(query.trim());
    const clear = searching ? el('button', { type: 'button', class: 'secondary small', textContent: t('Clear search') }) : null;
    if (clear) clear.onclick = () => { field('#messages-search', ''); renderMessages(); $('#messages-search').focus(); };
    list.append(el('div', { class: 'empty' }, el('p', { textContent: searching ? t('No messages match this search.')
      : messageRows.length ? t('No messages for this filter.') : t('No messages yet. They show up here when the assistant receives one from Teams.') }), clear));
    return;
  }
  for (const row of rows) {
    const audit = row.audit;
    const [label, tone] = messageState(row);
    const item = el('details', { class: 'message', open: openMessages.has(row.resource) });
    item.dataset.resource = row.resource;
    item.ontoggle = () => { if (item.open) openMessages.add(row.resource); else openMessages.delete(row.resource); };
    item.append(el('summary', {}, badge(label, tone), el('span', { class: 'kind', textContent: kindOf(row) }),
      el('time', { textContent: whenShort(row), title: when(row) }), el('span', { class: 'excerpt', textContent: excerpt(row) })));
    const body = el('div', { class: 'message-body' });
    messageField(body, t('Message'), audit.question, true);
    if (audit.resolved_question) messageField(body, t('Interpreted with the context'), `${audit.resolved_question}${audit.topic ? ` · ${t('topic searched')}: ${audit.topic}` : ''}`);
    messageField(body, t('Identified question'), audit.identified_question ? t(audit.identified_question.text) : t('No question identified'));
    if (audit.available_sources?.length) messageField(body, t('Available sources'), audit.available_sources.map(sourceTitle).join(', '));
    if (audit.selected_sources?.length) messageField(body, t('Sources selected to answer'), audit.selected_sources.map(sourceTitle).join(', '));
    if (!audit.identified_question && ['no_matching_question', 'question_matching_unavailable'].includes(audit.reason)) body.append(questionAssociation(row));
    messageField(body, t('Outcome'), reasonText(audit.reason, audit));
    if (row.status === 'sent') messageField(body, t('Answer sent'), audit.sent || audit.proposed, 'markdown');
    else messageField(body, row.status === 'dry_run' ? t('Proposal (observation mode)') : t('Proposed answer (not sent)'), audit.proposed, 'markdown');
    referencesField(body, audit);
    if (audit.refinement) messageField(body, t('Your context for the assistant'), audit.refinement, true);
    if (row.status === 'refining') {
      messageField(body, t('Generating'), audit.proposed?.trim()
        ? t('The assistant is writing the answer again with your context; it is not sent until you send it.')
        : t('The assistant is reading the conversation and writing an answer; it is not sent until you send it.'));
    }
    if (unsent(row)) body.append(answerActions(row));
    if (rateable(row)) body.append(feedbackBlock(row));
    if (audit.sender_name && audit.conversation_kind !== 'self') body.append(personLink(row));
    const details = [];
    if (audit.intent) details.push(`${t('Intent')}: ${intentLabels[audit.intent] || audit.intent}`);
    if (audit.history_messages) details.push(t('Context: {n} earlier messages', { n: audit.history_messages }));
    if (audit.provider) details.push(`${t('Model')}: ${audit.provider}`);
    if (audit.provider_fallbacks?.length) details.push(`${t('Failed first')}: ${audit.provider_fallbacks.join(', ')}`);
    if (audit.source) details.push(`${t('Sources')}: ${audit.source}`);
    if (audit.references?.length) details.push(`${t('References: {cited} cited of {total}', { cited: audit.used_sources?.length ?? 0, total: audit.references.length })}${audit.reference_selection === 'code' ? ` ${t('(chosen by code; no model chose them)')}` : ''}`);
    if (audit.final_check) details.push(`${t('Final review (earlier version)')}: ${audit.final_check === 'unavailable' ? t('unavailable') : audit.final_check}`);
    const notice = { sending: t('sending'), sent: t('sent'), uncertain: t('uncertain (never retried)') };
    if (audit.holding_reply) details.push(`${t('Holding notice')}: ${notice[audit.holding_reply] || audit.holding_reply}`);
    if (audit.withheld_notice) details.push(`${t('Withheld-answer notice')}: ${notice[audit.withheld_notice] || audit.withheld_notice}`);
    for (const link of audit.links || []) {
      const target = link.created_title
        ? t('new task {id}«{title}» under #{parent} {parentTitle}', { id: link.created ? `#${link.created} ` : '', title: link.created_title, parent: link.work_item, parentTitle: link.title })
        : `#${link.work_item} ${link.title}`;
      details.push(`${t('Registered')}: ${link.activity} → ${target}: ${linkLabels[link.status] || link.status}`);
    }
    messageField(body, t('Details'), details.join('\n'), true);
    if (audit.trace?.length) {
      const log = el('ol', { class: 'trace' });
      for (const step of audit.trace) {
        log.append(el('li', {}, el('time', { textContent: new Date(step.at).toLocaleTimeString(uiLocale()) }), el('b', { textContent: step.step }),
          step.detail ? ` — ${step.detail}` : ''));
      }
      body.append(el('div', { class: 'field-block' }, el('strong', { textContent: t('Log') }), log));
    } else {
      messageField(body, t('Log'), t('This message was processed by an earlier version, without a step log.'));
    }
    body.append(el('small', { class: 'resource-id', textContent: row.resource }));
    item.append(body);
    list.append(item);
  }
  const restored = typingAt && [...list.querySelectorAll(`.${typing.classList.contains('feedback-comment') ? 'feedback-comment' : 'answer-context'}`)].find(node => node.dataset.resource === typingAt[0]);
  if (restored) { restored.focus(); restored.setSelectionRange(typingAt[1], typingAt[2]); }
  // A message opened from Home: show it once it is in the list.
  const focused = focusMessage && [...list.querySelectorAll('.message')].find(node => node.dataset.resource === focusMessage);
  if (focused) {
    focusMessage = null;
    focused.scrollIntoView({ block: 'start' });
    focused.querySelector('summary').focus({ preventScroll: true });
  }
}

/** Opens one message in Messages, expanded, whatever filter or search was active. */
function openMessage(resource) {
  focusMessage = resource;
  openMessages.add(resource);
  field('#messages-search', '');
  setFilter('all');
  showTab('messages');
}

function renderAttention() {
  const since = Date.now() - 24 * 3600_000;
  const errors = messageRows.filter(row => messageState(row)[1] === 'error' && millis(row) >= since).length;
  const counter = $('#nav-attention');
  counter.hidden = !errors;
  counter.textContent = errors;
  counter.title = t('{n} message(s) with an error or an uncertain send in the last 24 h', { n: errors });
}

async function loadMessages() {
  messageRows = await invoke('audit', { limit: 100, resource: null, content: true });
  renderMessages();
  renderAttention();
}

async function loadActivity() {
  try { messageRows = await invoke('audit', { limit: 100, resource: null, content: true }); }
  catch { messageRows = []; }
  renderAttention();
  const relevant = messageRows.filter(row => !ineligible(row));
  const tally = (filter) => relevant.filter(row => matchesFilter(row, filter)).length;
  const stats = [[t('Answered'), tally('sent'), 'ok', 'sent'], [t('Withheld or unanswered'), tally('withheld'), 'warn', 'withheld'],
    [t('Errors'), tally('error'), 'error', 'error'], [t('In progress'), tally('active'), 'active', 'active']];
  // Every outcome keeps its tile; only those with messages take their color.
  $('#activity-stats').replaceChildren(...stats.map(([label, n, tone, filter]) => {
    const stat = el('button', { type: 'button', class: `stat ${n ? tone : 'muted'}`, title: t('See in Messages') },
      el('strong', { textContent: String(n) }), el('span', { textContent: label }));
    stat.onclick = () => { setFilter(filter); showTab('messages'); };
    return stat;
  }));
  const recent = relevant.slice(0, 6);
  $('#activity-list').replaceChildren(...(recent.length ? recent.map(row => {
    const [label, tone] = messageState(row);
    const open = el('button', { type: 'button', class: 'activity-row', title: t('Open in Messages') }, badge(label, tone),
      el('time', { textContent: ago(millis(row)), title: when(row) }), el('span', { class: 'excerpt', textContent: excerpt(row) }));
    open.onclick = () => openMessage(row.resource);
    return open;
  }) : [el('div', { class: 'empty' }, el('p', { textContent: t('No messages addressed to the assistant yet.') }))]));
}

function setFilter(filter) {
  messageFilter = filter;
  for (const button of document.querySelectorAll('#messages-filters button')) {
    button.setAttribute('aria-pressed', String(button.dataset.filter === filter));
  }
  renderMessages();
}

export { reasonText, ago, folded, feedbackBlock, renderMessages, loadMessages, loadActivity, setFilter };
