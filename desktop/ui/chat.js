// Test chat: its sources and answer bubbles.
import { t } from './i18n.js';
import { $, badge, el } from './dom.js';
import { live } from './state.js';
import { link, markdown } from './markdown.js';
import { feedbackBlock, reasonText } from './messages.js';
import { kindName } from './knowledge.js';
import { questionAssociation, sourceTitle } from './knowledge-map.js';

// ---------------------------------------------------------------- test chat

const chosen = new Set();

function renderChatSources() {
  const container = $('#chat-sources');
  container.replaceChildren();
  // The simulation reads the saved knowledge map, so offer what is persisted.
  const usable = live.map.resources.filter(r => r.enabled && r.external_processing);
  for (const id of [...chosen]) if (!usable.some(r => r.id === id)) chosen.delete(id);
  $('#chat-all').hidden = usable.length < 2;
  if (!usable.length) {
    container.append(el('p', { class: 'hint', textContent: t('No sources are ready. In Knowledge, enable one, authorize its external processing and save.') }));
    return;
  }
  for (const resource of usable) {
    const box = el('input', { type: 'checkbox', value: resource.id, checked: chosen.has(resource.id) });
    box.onchange = () => { if (box.checked) chosen.add(resource.id); else chosen.delete(resource.id); };
    container.append(el('label', { class: 'check' }, box,
      el('span', {}, el('strong', { textContent: resource.id }), el('small', { textContent: `${kindName(resource)} · ${resource.description}` }))));
  }
}

function bubble(kind, ...content) {
  $('#chat-empty').hidden = true;
  const node = el('div', { class: `bubble ${kind}` }, ...content);
  $('#history').append(node);
  node.scrollIntoView({ block: 'end', behavior: 'smooth' });
  return node;
}

function answerBubble(result) {
  const row = { resource: result.resource, status: result.status, audit: { ...result, question: result.request, proposed: result.answer } };
  const question = el('p', { class: 'hint', textContent: result.identified_question ? `${t('Identified question')}: ${t(result.identified_question.text)}` : t('No question identified') });
  const sources = el('p', { class: 'hint', textContent: `${t('Available sources')}: ${(result.available_sources ?? []).map(sourceTitle).join(', ')}\n${t('Sources selected to answer')}: ${(result.selected_sources ?? []).map(sourceTitle).join(', ')}` });
  const review = result.resource && !result.identified_question && ['no_matching_question', 'question_matching_unavailable'].includes(result.reason) ? questionAssociation(row, q => { question.textContent = `${t('Identified question')}: ${t(q.text)}`; }) : null;
  if (!result.answer) return bubble('bot none', `${t('No answer')}: ${reasonText(result.reason) || result.status}`, question, sources, review);
  const meta = el('div', { class: 'meta' });
  for (const id of result.used_sources ?? []) meta.append(badge(id, 'active'));
  if (result.partial) meta.append(badge(t('Partial coverage'), 'warn'));
  for (const warning of result.warnings ?? []) meta.append(badge(warning, 'warn'));
  let refs = null;
  if (result.references?.length) {
    refs = el('div', { class: 'refs' }, el('strong', { textContent: t('Verified references') }),
      el('ul', {}, ...result.references.map(ref => el('li', {}, link(ref.label || ref.id, ref.url)))));
  }
  return bubble('bot', markdown(result.answer), question, sources, meta.childNodes.length ? meta : null, refs, result.resource ? feedbackBlock(row) : null);
}

function resizeComposer() {
  const area = $('#message');
  area.style.height = 'auto';
  area.style.height = `${Math.min(area.scrollHeight + 2, 192)}px`;
}

export { chosen, renderChatSources, bubble, answerBubble, resizeComposer };
