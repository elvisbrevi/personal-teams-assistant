// To-Do › Guides and instructions: work item guides and agent instructions.
import { t, tn } from './i18n.js';
import { $, badge, el, lines, names } from './dom.js';
import { current } from './state.js';
import { markDirty } from './nav.js';
import { renderOrganization } from './knowledge-map.js';

// ---------------------------------------------------------------- work item guides and agent instructions

/** Guides and guide fields whose edit fold is open, kept across re-renders. */
const openGuides = new Set();
const openGuideFields = new Set();
const openPrompts = new Set();
const providerLabel = provider => provider === 'github' ? 'GitHub' : 'Azure DevOps';

function guideFieldEditor(guide, field, changed) {
  const key = `${guide.id}/${field.reference}`;
  const summary = el('summary');
  const renderSummary = () => summary.replaceChildren(el('strong', { textContent: field.name || field.reference }), ' ', el('code', { textContent: field.reference }),
    ...(field.required ? [' ', badge(t('Required'), 'warn')] : []), ...(field.fill ? [' ', badge(t('Agents propose it'), 'active')] : []));
  renderSummary();
  const input = (label, value, placeholder, onchange, options = {}) => {
    const element = el('input', { value: value ?? '', placeholder, spellcheck: false, ...options });
    element.oninput = () => { onchange(element.value); renderSummary(); changed(); };
    return el('label', {}, label, element);
  };
  const check = (label, value, onchange, disabled = false) => {
    const box = el('input', { type: 'checkbox', checked: Boolean(value), disabled });
    box.onchange = () => { onchange(box.checked); renderSummary(); changed(); };
    return el('label', { class: 'check' }, box, label);
  };
  const values = el('textarea', { value: (field.values ?? []).join('\n'), rows: 3, spellcheck: false });
  values.oninput = () => { field.values = lines(values.value); changed(); };
  const guidance = el('textarea', { value: field.guidance ?? '', rows: 2 });
  guidance.oninput = () => { field.guidance = guidance.value; changed(); };
  const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove field') });
  remove.onclick = () => { guide.fields = guide.fields.filter(f => f !== field); openGuideFields.delete(key); changed(); renderGuides(); };
  const fold = el('details', { class: 'fold', open: openGuideFields.has(key) }, summary,
    el('div', { class: 'grid' },
      input(t('Reference name'), field.reference, 'Custom.Field', value => { field.reference = value.trim(); }, { translate: 'no' }),
      input(t('Label'), field.name, t('As people see it'), value => { field.name = value; }),
      input(t('Default value'), field.default, '', value => { field.default = value; })),
    el('div', { class: 'grid' },
      check(t('Required'), field.required, value => { field.required = value; }),
      check(t('The agents propose its value'), field.fill, value => { field.fill = value; }, guide.provider === 'github' || field.reference.startsWith('System.'))),
    el('label', {}, t('Allowed values, one per line'), values),
    el('label', {}, t('What it is for and how to fill it'), guidance),
    el('div', { class: 'actions' }, remove));
  fold.ontoggle = () => { if (fold.open) openGuideFields.add(key); else openGuideFields.delete(key); };
  return fold;
}

function renderGuides() {
  const container = $('#guides');
  container.replaceChildren();
  const guides = current.map.work_item_guides ??= [];
  $('#guides-summary').textContent = guides.length ? tn(guides.length, '1 guide', '{n} guides') : '';
  if (!guides.length) container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No guides yet. Add one for each work item type you create: HU, bug, task, support task…') })));
  for (const guide of guides) {
    guide.fields ??= [];
    const facts = el('p', { class: 'resource-facts' });
    const renderFacts = () => facts.replaceChildren(el('span', { textContent: guide.purpose || t('No purpose written yet') }),
      el('small', { textContent: [`${t('Projects')}: ${(guide.projects ?? []).join(', ') || t('all')}`,
        tn(guide.fields.filter(f => f.fill).length, '1 field the agents fill', '{n} fields the agents fill')].join(' · ') }));
    renderFacts();
    const changed = () => { renderFacts(); markDirty({ target: container }); };
    const input = (label, value, placeholder, onchange, options = {}) => {
      const element = el('input', { value: value ?? '', placeholder, spellcheck: false, ...options });
      element.oninput = () => { onchange(element.value); changed(); };
      return el('label', {}, label, element);
    };
    const area = (label, value, placeholder, onchange, rows = 3) => {
      const element = el('textarea', { value: value ?? '', placeholder, rows });
      element.oninput = () => { onchange(element.value); changed(); };
      return el('label', {}, label, element);
    };
    const provider = el('select', {}, ...['azure', 'github'].map(value => el('option', { value, textContent: providerLabel(value), selected: guide.provider === value })));
    provider.onchange = () => {
      guide.provider = provider.value;
      if (guide.provider === 'github') { for (const f of guide.fields) f.fill = false; guide.listed_with_bugs = false; }
      changed(); renderGuides();
    };
    const withBugs = el('input', { type: 'checkbox', checked: Boolean(guide.listed_with_bugs), disabled: guide.provider !== 'azure' });
    withBugs.onchange = () => { guide.listed_with_bugs = withBugs.checked; changed(); };
    const fields = el('div', { class: 'list' }, ...guide.fields.map(field => guideFieldEditor(guide, field, changed)));
    const addField = el('button', { type: 'button', class: 'secondary small', textContent: t('Add field') });
    addField.onclick = () => {
      const field = { reference: '', name: '', required: false, values: [], default: '', guidance: '', fill: false };
      guide.fields.push(field);
      openGuideFields.add(`${guide.id}/`);
      changed(); renderGuides();
    };
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove guide') });
    remove.onclick = () => { current.map.work_item_guides = guides.filter(g => g !== guide); markDirty({ target: container }); renderGuides(); renderOrganization(); };
    const edit = el('details', { class: 'fold resource-edit', open: openGuides.has(guide.id) },
      el('summary', { textContent: t('Edit the guide and its fields') }),
      el('div', { class: 'grid' },
        input(t('What you call it'), guide.name, 'HU', value => { guide.name = value; }),
        input(t('Type, as the provider names it'), guide.type, 'Product Backlog Item', value => { guide.type = value.trim(); }, { translate: 'no' }),
        el('label', {}, t('Provider'), provider),
        input(t('Projects or repositories, comma separated'), (guide.projects ?? []).join(', '), t('Empty applies to all'), value => { guide.projects = names(value); })),
      el('label', { class: 'check' }, withBugs, t('To-Do lists this task type with the bugs')),
      area(t('When it is used'), guide.purpose, t('What this type is for in your organization'), value => { guide.purpose = value; }),
      area(t('How to write it'), guide.instructions, t('Title pattern, description structure, conventions'), value => { guide.instructions = value; }, 6),
      el('fieldset', {}, el('legend', { textContent: t('Fields') }), fields, el('div', { class: 'actions' }, addField)),
      el('div', { class: 'actions' }, remove));
    edit.ontoggle = () => { if (edit.open) openGuides.add(guide.id); else openGuides.delete(guide.id); };
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: guide.name || guide.type }), el('code', { textContent: guide.type }), el('span', { class: 'grow' }),
        badge(providerLabel(guide.provider), 'muted'), badge(tn(guide.fields.length, '1 field', '{n} fields'), guide.fields.length ? 'active' : 'muted')),
      facts, edit));
  }
}

function renderAgentPrompts() {
  const container = $('#agent-prompts');
  container.replaceChildren();
  const changes = current.map.agent_prompts ??= [];
  for (const built of current.agent_prompts ?? []) {
    const change = () => changes.find(c => c.id === built.id);
    const state = el('span');
    const renderState = () => state.replaceChildren(change() ? badge(t('Your instructions'), 'active') : badge(t('Default instructions'), 'muted'));
    renderState();
    const text = el('textarea', { value: change()?.instructions ?? built.instructions, rows: 8 });
    text.oninput = () => {
      changes.splice(0, changes.length, ...changes.filter(c => c.id !== built.id));
      if (text.value.trim() !== built.instructions) changes.push({ id: built.id, instructions: text.value });
      renderState(); markDirty({ target: container });
    };
    const reset = el('button', { type: 'button', class: 'secondary small', textContent: t('Restore the default') });
    reset.onclick = () => {
      changes.splice(0, changes.length, ...changes.filter(c => c.id !== built.id));
      text.value = built.instructions;
      renderState(); markDirty({ target: container });
    };
    const edit = el('details', { class: 'fold resource-edit', open: openPrompts.has(built.id) }, el('summary', { textContent: t('Edit the instructions') }),
      el('label', {}, t('Instructions'), text), el('div', { class: 'actions' }, reset));
    edit.ontoggle = () => { if (edit.open) openPrompts.add(built.id); else openPrompts.delete(built.id); };
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: t(built.name) }), el('span', { class: 'grow' }), state),
      el('p', { class: 'resource-facts' }, el('span', { textContent: t(built.summary) })), edit));
  }
}

export { openGuides, renderGuides, renderAgentPrompts };
