// Knowledge: contexts, questions, people and the organization.
import { t, tn } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el, names } from './dom.js';
import { current } from './state.js';
import { busy, done } from './notices.js';
import { markDirty } from './nav.js';
import { applying } from './profile.js';
import { ago, loadMessages } from './messages.js';
import { kindName, renderNoteContexts } from './knowledge.js';

// ---------------------------------------------------------------- contexts

/** Contexts whose edit fold is open, kept across re-renders. */
const openContexts = new Set();

function renderContexts() {
  const container = $('#contexts');
  container.replaceChildren();
  const contexts = current.map.contexts ??= [];
  $('#contexts-summary').textContent = contexts.length ? tn(contexts.length, '1 context', '{n} contexts') : '';
  if (!contexts.length) container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No contexts yet. Add one for each subject you answer about, with the words that name it.') })));
  for (const context of contexts) {
    const facts = el('p', { class: 'resource-facts' });
    const renderFacts = () => facts.replaceChildren(
      el('span', { textContent: context.description || t('No description') }),
      el('small', { textContent: [`${t('Keywords')}: ${context.keywords.join(', ') || '—'}`,
        `${t('Sources')}: ${context.sources.join(', ') || t('none')}`].join(' · ') }));
    renderFacts();
    const input = (label, value, placeholder, onchange) => {
      const element = el('input', { value, placeholder, spellcheck: false });
      element.oninput = () => { onchange(element.value); renderFacts(); markDirty(); };
      return el('label', {}, label, element);
    };
    const area = (label, value, placeholder, onchange) => {
      const element = el('textarea', { value, placeholder, rows: 3 });
      element.oninput = () => { onchange(element.value); renderFacts(); markDirty(); };
      return el('label', {}, label, element);
    };
    const sources = el('fieldset', { class: 'choices' }, el('legend', { textContent: t('Sources it reads') }));
    for (const resource of current.map.resources) {
      const box = el('input', { type: 'checkbox', value: resource.id, checked: context.sources.includes(resource.id) });
      box.onchange = () => {
        context.sources = box.checked ? [...context.sources, resource.id] : context.sources.filter(id => id !== resource.id);
        renderFacts(); markDirty();
      };
      sources.append(el('label', { class: 'check' }, box, el('span', {}, el('strong', { textContent: resource.id }), el('small', { textContent: kindName(resource) }))));
    }
    if (!current.map.resources.length) sources.append(el('p', { class: 'hint', textContent: t('Add sources first.') }));
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove context') });
    remove.onclick = () => { current.map.contexts = contexts.filter(c => c !== context); markDirty(); renderContexts(); renderNoteContexts(); };
    const edit = el('details', { class: 'fold resource-edit', open: openContexts.has(context.id) },
      el('summary', { textContent: t('Edit keywords, sources and instructions') }),
      el('div', { class: 'grid' },
        input(t('Name'), context.name, t('Billing'), value => { context.name = value; }),
        input(t('Keywords'), context.keywords.join(', '), t('Comma separated'), value => { context.keywords = names(value); })),
      input(t('Description'), context.description, t('What the subject covers'), value => { context.description = value; }),
      sources,
      area(t('Instructions for answers'), context.instructions, t('Terms to use, tone, what to always include'), value => { context.instructions = value; }),
      el('div', { class: 'actions' }, remove));
    edit.ontoggle = () => { if (edit.open) openContexts.add(context.id); else openContexts.delete(context.id); };
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: context.name || context.id }), badge(context.id, 'muted'), el('span', { class: 'grow' }),
        badge(tn(context.sources.length, '1 source', '{n} sources'), context.sources.length ? 'active' : 'warn')),
      facts, edit));
  }
}

// ---------------------------------------------------------------- questions

const openQuestions = new Set();
const newQuestionSources = new Set();
const sourceTitle = id => { const r = current?.map?.resources.find(r => r.id === id); return r ? `${r.id} · ${kindName(r)}` : id; };
function questionSourceChoices(selected, changed) {
  const choices = el('fieldset', { class: 'choices' }, el('legend', { textContent: t('Sources for this question') }));
  for (const source of current.map.resources) {
    const box = el('input', { type: 'checkbox', value: source.id, checked: selected.has(source.id) });
    box.onchange = () => { if (box.checked) selected.add(source.id); else selected.delete(source.id); changed([...selected]); };
    choices.append(el('label', { class: 'check' }, box, el('span', {}, el('strong', { textContent: source.id }),
      el('small', { textContent: `${kindName(source)}${!source.enabled || !source.external_processing ? ` · ${t('Unavailable until enabled and authorized')}` : ''}` }))));
  }
  return choices;
}
function renderQuestions() {
  const container = $('#questions');
  container.replaceChildren();
  const questions = current.map.questions ??= [];
  $('#questions-summary').textContent = tn(questions.length, '1 question', '{n} questions');
  const choices = questionSourceChoices(newQuestionSources, () => {});
  choices.id = 'question-sources';
  $('#question-sources').replaceWith(choices);
  if (!questions.length) container.append(el('p', { class: 'hint', textContent: t('No questions yet. Add one to let the assistant recognize equivalent requests.') }));
  for (const question of questions) {
    const text = el('input', { value: t(question.text), maxLength: 1000 });
    text.oninput = () => { question.text = text.value; markDirty({ target: container }); };
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove question') });
    remove.onclick = () => { current.map.questions = questions.filter(q => q.id !== question.id); markDirty({ target: container }); renderQuestions(); };
    const edit = el('details', { class: 'fold resource-edit', open: openQuestions.has(question.id) },
      el('summary', { textContent: t('Edit question') }), el('label', {}, t('Question'), text),
      questionSourceChoices(new Set(question.sources), sources => { question.sources = sources; markDirty({ target: container }); }),
      el('div', { class: 'field-block' }, el('strong', { textContent: t('Associated requests') }),
        ...(question.examples?.length ? [el('ul', {}, ...question.examples.map(example => el('li', { textContent: example })))] : [el('p', { class: 'hint', textContent: t('Requests associated during review appear here.') })])),
      el('div', { class: 'actions' }, remove));
    edit.ontoggle = () => { if (edit.open) openQuestions.add(question.id); else openQuestions.delete(question.id); };
    container.append(el('div', { class: 'resource' }, el('strong', { textContent: t(question.text) }),
      el('p', { class: 'resource-facts', textContent: question.sources.map(sourceTitle).join(', ') }), edit));
  }
}

/** Association forms being filled (resource → { open, question, text, selected }), kept across list refreshes. */
const associationDrafts = new Map();

/** @param {(question: any) => void} [associated] */
function questionAssociation(row, associated = () => {}) {
  const draft = associationDrafts.get(row.resource) ?? { open: false, question: '', text: row.audit.question ?? '', selected: new Set() };
  associationDrafts.set(row.resource, draft);
  const select = el('select', {}, el('option', { value: '', textContent: t('Create a new question') }),
    ...(current.map.questions ?? []).map(q => el('option', { value: q.id, textContent: t(q.text) })));
  select.value = draft.question;
  if (select.value !== draft.question) { draft.question = ''; select.value = ''; }
  const text = el('input', { value: draft.text, maxLength: 1000 });
  text.oninput = () => { draft.text = text.value; };
  const textLabel = el('label', {}, t('New question'), text);
  textLabel.hidden = !!select.value;
  const selected = draft.selected;
  let choices = questionSourceChoices(selected, () => {});
  select.onchange = () => {
    textLabel.hidden = !!select.value;
    draft.question = select.value;
    selected.clear();
    for (const id of current.map.questions?.find(q => q.id === select.value)?.sources ?? []) selected.add(id);
    const next = questionSourceChoices(selected, () => {}); choices.replaceWith(next); choices = next;
  };
  const outcome = el('p', { class: 'hint', role: 'status' });
  const save = el('button', { type: 'button', class: 'secondary', textContent: t('Save association') });
  const form = el('details', { class: 'fold question-association', open: draft.open }, el('summary', { textContent: t('Associate question') }),
    el('label', {}, t('Existing question or new'), select), textLabel, choices,
    el('p', { class: 'hint', textContent: t('The message becomes an example of this question. Source audiences still apply. Saving does not send an answer.') }), save, outcome);
  form.ontoggle = () => { draft.open = form.open; };
  save.onclick = () => busy(save, async () => {
    if (!selected.size || (!select.value && !text.value.trim())) throw t('Write a question and select at least one source.');
    const result = await applying(() => invoke('question_associate', { resource: row.resource,
      ...(select.value ? { question_id: select.value } : { text: text.value.trim() }), sources: [...selected] }));
    row.audit.identified_question = result.question;
    associationDrafts.delete(row.resource);
    associated(result.question);
    outcome.textContent = t('Associated with: {question}', { question: t(result.question.text) });
    done(t('Question associated. You can review it in Questions.'));
    if (!row.resource.startsWith('simulation:')) await loadMessages();
  });
  return form;
}

// ---------------------------------------------------------------- people

/** People whose edit fold is open, kept across re-renders. */
const openPeople = new Set();

function renderPeople() {
  const container = $('#people');
  container.replaceChildren();
  const people = current.map.people ??= [];
  $('#people-summary').textContent = people.length ? tn(people.length, '1 person', '{n} people') : '';
  if (!people.length) container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No people yet. Add someone here, or from one of their messages in Messages.') })));
  for (const person of people) {
    const facts = el('p', { class: 'resource-facts' });
    const renderFacts = () => facts.replaceChildren(el('span', { textContent: person.address || t('No note on how to address them') }),
      ...(person.notes ? [el('small', { textContent: person.notes })] : []));
    renderFacts();
    const input = (label, value, placeholder, onchange, options = {}) => {
      const element = el('input', { value, placeholder, spellcheck: false, ...options });
      element.oninput = () => { onchange(element.value); renderFacts(); markDirty(); };
      return el('label', {}, label, element);
    };
    const area = (label, value, placeholder, onchange) => {
      const element = el('textarea', { value, placeholder, rows: 2 });
      element.oninput = () => { onchange(element.value); renderFacts(); markDirty(); };
      return el('label', {}, label, element);
    };
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove person') });
    remove.onclick = () => { current.map.people = people.filter(p => p !== person); markDirty(); renderPeople(); };
    const edit = el('details', { class: 'fold resource-edit', open: openPeople.has(person.id) },
      el('summary', { textContent: t('Edit how you address them') }),
      el('div', { class: 'grid' },
        input(t('Name'), person.name, 'Ana Pérez', value => { person.name = value; }),
        input(t('Other names'), (person.aliases ?? []).join(', '), t('Comma separated'), value => { person.aliases = names(value); })),
      area(t('How you address them'), person.address, t('Informal, by first name, two or three sentences'), value => { person.address = value; }),
      area(t('What to keep in mind'), person.notes, t('Their role, what they usually ask'), value => { person.notes = value; }),
      input(t('Teams user IDs'), (person.senders ?? []).join(', '), t('Comma separated'), value => { person.senders = names(value); }, { translate: 'no' }),
      el('div', { class: 'actions' }, remove));
    edit.ontoggle = () => { if (edit.open) openPeople.add(person.id); else openPeople.delete(person.id); };
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: person.name }), el('span', { class: 'grow' }),
        badge(person.senders?.length ? t('Teams ID known') : t('By name'), person.senders?.length ? 'ok' : 'muted')),
      facts, edit));
  }
}

// ---------------------------------------------------------------- organization

const entryKinds = { norm: t('Norm'), architecture: t('Architecture'), convention: t('Convention'), glossary_term: t('Glossary term'), template: t('Template') };
/** Entries whose edit fold is open, kept across re-renders. */
const openEntries = new Set();
/** Sources that can document the organization: a Wiki, a repository, a file or a URL. */
const documentsOrganization = r => ['file', 'url', 'urls', 'repository'].includes(r.kind) || r.tool?.type === 'azure_devops_wiki';

function renderOrganization() {
  const organization = current.map.organization ??= {};
  const name = $('#organization-name');
  if (document.activeElement !== name) name.value = organization.name ?? '';
  $('#organization-offer').hidden = current.map.resources.some(r => r.kind === 'organization');
  const fieldset = $('#organization-sources');
  fieldset.querySelectorAll('label, p').forEach(node => node.remove());
  const chosen = organization.sources ??= [];
  const usable = current.map.resources.filter(documentsOrganization);
  if (!usable.length) fieldset.append(el('p', { class: 'hint', textContent: t('Add a Wiki, repository, file or URL source in Sources to name it here.') }));
  for (const resource of usable) {
    const box = el('input', { type: 'checkbox', value: resource.id, checked: chosen.includes(resource.id) });
    box.onchange = () => {
      organization.sources = box.checked ? [...chosen.filter(id => id !== resource.id), resource.id] : chosen.filter(id => id !== resource.id);
      markDirty({ target: fieldset }); renderOrganization();
    };
    fieldset.append(el('label', { class: 'check' }, box, el('span', { textContent: resource.id }), ' ', el('small', { textContent: kindName(resource) })));
  }
  const entries = organization.entries?.length ?? 0;
  $('#organization-summary').textContent = entries ? tn(entries, '1 entry', '{n} entries') : '';
}
$('#organization-name').oninput = event => {
  (current.map.organization ??= {}).name = event.target.value;
  markDirty(event);
};

function renderEntries() {
  const container = $('#organization-entries');
  container.replaceChildren();
  const organization = current.map.organization ??= {};
  const entries = organization.entries ??= [];
  $('#entries-summary').textContent = entries.length ? tn(entries.length, '1 entry', '{n} entries') : '';
  if (!entries.length) container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No entries yet. Add the norms, architecture and conventions you follow, or ask an agent to add them with pta organization entries add.') })));
  for (const entry of entries) {
    entry.projects ??= [];
    const facts = el('p', { class: 'resource-facts' });
    const renderFacts = () => facts.replaceChildren(el('span', { textContent: entry.text.split('\n').find(line => line.trim()) ?? '' }),
      el('small', { textContent: [`${t('Projects')}: ${entry.projects.join(', ') || t('all')}`,
        ...(entry.updated_at ? [t('Updated {when}', { when: ago(entry.updated_at * 1000) })] : [])].join(' · ') }));
    renderFacts();
    const changed = () => { renderFacts(); markDirty({ target: container }); };
    const title = el('input', { value: entry.title });
    title.oninput = () => { entry.title = title.value; heading.textContent = title.value; changed(); };
    const kind = el('select', {}, ...Object.entries(entryKinds).map(([value, label]) => el('option', { value, textContent: label, selected: entry.kind === value })));
    const kindBadge = badge(entryKinds[entry.kind] ?? entry.kind, 'muted');
    kind.onchange = () => { entry.kind = kind.value; kindBadge.textContent = entryKinds[entry.kind]; changed(); };
    const projects = el('input', { value: entry.projects.join(', '), placeholder: t('Empty applies to all'), spellcheck: false });
    projects.oninput = () => { entry.projects = names(projects.value); changed(); };
    const text = el('textarea', { value: entry.text, rows: 8 });
    text.oninput = () => { entry.text = text.value; changed(); };
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove entry') });
    remove.onclick = () => { organization.entries = entries.filter(e => e !== entry); openEntries.delete(entry.id); markDirty({ target: container }); renderEntries(); renderOrganization(); };
    const heading = el('strong', { class: 'title', textContent: entry.title });
    const edit = el('details', { class: 'fold resource-edit', open: openEntries.has(entry.id) },
      el('summary', { textContent: t('Edit the entry') }),
      el('div', { class: 'grid' },
        el('label', {}, t('Title'), title),
        el('label', {}, t('Kind'), kind),
        el('label', {}, t('Projects or repositories, comma separated'), projects)),
      el('label', {}, t('Text, in Markdown'), text),
      el('div', { class: 'actions' }, remove));
    edit.ontoggle = () => { if (edit.open) openEntries.add(entry.id); else openEntries.delete(entry.id); };
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, heading, el('code', { textContent: entry.id }), el('span', { class: 'grow' }), kindBadge),
      facts, edit));
  }
}

export {
  openContexts, renderContexts, openQuestions, newQuestionSources, sourceTitle, renderQuestions,
  questionAssociation, openPeople, renderPeople, renderOrganization, renderEntries,
};
