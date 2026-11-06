// Knowledge: sources, repositories, GitHub and notes.
import { t, tn } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el, lines, names, rowText } from './dom.js';
import { current } from './state.js';
import { busy, done, fail, notify } from './notices.js';
import { markDirty } from './nav.js';
import { chooseDirectories } from './directory-sheet.js';
import { readySources } from './home.js';
import { applying } from './profile.js';
import { ago } from './messages.js';
import { renderContexts, renderOrganization } from './knowledge-map.js';

// ---------------------------------------------------------------- knowledge

const toolNames = {
  azure_devops_wiki: 'Azure DevOps Wiki', azure_devops_status: t('Azure DevOps activity'), azure_devops: t('Azure DevOps work items'),
  sql_server: 'SQL Server', rabbitmq: t('RabbitMQ queue'), http: 'HTTP GET', teams_messages: t('My Teams messages'),
  activity_record: t('Activity record'),
};

/** The activity record as a new source: disabled, without audiences or external processing. */
function recordSource() {
  return { id: 'activity-record', description: t('Existing Azure tasks saved by day, with their HU and hours.'),
    topics: [], enabled: false, external_processing: false, allowed_conversations: [], allowed_senders: [],
    kind: 'tool', tool: { type: 'activity_record' } };
}

/** An inline Azure DevOps catalog as its organizations' names. */
const catalogText = catalog => catalog.map(c => c.organization.replace(/^https:\/\/dev\.azure\.com\//, '').replace(/\/$/, '')).join(', ');

function kindName(resource) {
  if (resource.kind === 'file') return t('File');
  if (resource.kind === 'url' || resource.kind === 'urls') return 'URL';
  if (resource.kind === 'repository') return t('Repository');
  if (resource.kind === 'notes') return t('Notes');
  if (resource.kind === 'organization') return t('Organization');
  return toolNames[resource.tool?.type] || resource.tool?.type || t('Tool');
}

/** What the knowledge index holds per source (`index_status`), loaded with the Knowledge section. */
let indexStatus = { sources: [] };
const indexOf = resource => indexStatus.sources.find(s => s.id === resource.id)?.index;

function indexText(resource) {
  const held = indexOf(resource);
  if (resource.kind === 'notes') return tn(indexStatus.notes?.documents ?? 0, '1 note', '{n} notes');
  if (resource.kind === 'organization') return tn(indexStatus.organization?.documents ?? 0, '1 entry or guide', '{n} entries and guides');
  if (!held?.synced_at) return t('Not indexed yet: it is indexed the first time an answer reads it');
  return t('{files} files, {sections} sections indexed {when}', { files: held.documents, sections: held.sections, when: ago(held.synced_at * 1000) });
}

function accessText(resource) {
  if (resource.kind === 'file') return `${resource.repository} / ${resource.path}`;
  if (resource.kind === 'url') return resource.url;
  if (resource.kind === 'urls') return resource.urls.join(' · ');
  if (resource.kind === 'repository') return `${t('Repository')}: ${resource.repository} · ${indexText(resource)}`;
  if (resource.kind === 'notes') return resource.contexts?.length
    ? t('Your verified notes of {contexts} and the general ones', { contexts: resource.contexts.join(', ') })
    : `${t('Your verified notes')} · ${indexText(resource)}`;
  if (resource.kind === 'organization') return `${t('Your organization\'s entries and work item guides')} · ${indexText(resource)}`;
  const tool = resource.tool ?? {};
  if (tool.page_urls) return tool.page_urls.join(' · ');
  if (tool.organization) return `${tool.organization} / ${tool.project}`;
  if (tool.repository) return `${t('Catalog')}: ${tool.repository} / ${tool.path}`;
  if (tool.catalog?.length) return `${t('Catalog')}: ${catalogText(tool.catalog)}`;
  if (tool.type === 'activity_record') return t('Daily task record, read on this device');
  if (tool.url) return tool.url;
  if (tool.operation) return `${t('Fixed query')}: ${tool.operation}`;
  if (tool.type === 'teams_messages') return t('Your own messages, read with the connected Microsoft account');
  return t('Read-only tool');
}

function sourceState(resource) {
  if (!resource.enabled) return badge(t('Disabled'), 'muted');
  if (!resource.external_processing) return badge(t('No external processing'), 'warn');
  if (!resource.allowed_conversations.length) return badge(t('No Teams audience'), 'warn');
  return badge(t('Ready'), 'ok');
}

/** Sources whose edit fold is open, kept across re-renders. */
const openSources = new Set();

function renderResources() {
  const container = $('#resources');
  container.replaceChildren();
  const resources = current.map.resources;
  const wikiChoice = $('#url-source-wiki');
  const wikiValue = wikiChoice.value;
  wikiChoice.replaceChildren(el('option', { value: '', textContent: t('Public pages (no sign-in)') }), ...resources.filter(r => r.tool?.type === 'azure_devops_wiki' && !r.tool.page_urls).map(r => el('option', { value: r.id, textContent: r.id })));
  if ([...wikiChoice.options].some(o => o.value === wikiValue)) wikiChoice.value = wikiValue;
  $('#sources-summary').textContent = resources.length ? t('{ready} of {total} ready for Teams', { ready: readySources(current.map).length, total: resources.length }) : '';
  if (!resources.length) {
    container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No sources yet. Add a repository or a file from one, or register the Wiki with pta sources add.') })));
  }
  $('#record-offer').hidden = resources.some(r => r.tool?.type === 'activity_record');
  for (const resource of resources) {
    let state = sourceState(resource);
    const facts = el('p', { class: 'resource-facts' });
    // The row reads as a summary; the text fields live in the fold below and keep it current.
    const renderFacts = () => facts.replaceChildren(
      el('span', { textContent: resource.description || t('No description') }),
      el('small', { textContent: [accessText(resource),
        `${t('Topics')}: ${resource.topics.join(', ') || '—'}`,
        `${t('Audience')}: ${resource.allowed_conversations.join(', ') || t('none')}`].join(' · ') }));
    renderFacts();
    const input = (label, value, placeholder, onchange) => {
      const element = el('input', { value, placeholder, spellcheck: false });
      element.oninput = () => { onchange(element.value); renderFacts(); markDirty(); };
      return el('label', {}, label, element);
    };
    const toggles = el('div', { class: 'resource-toggles' });
    for (const [key, text] of [['enabled', t('Enabled')], ['external_processing', t('Authorize external processing')]]) {
      const box = el('input', { type: 'checkbox', checked: resource[key] });
      box.onchange = () => { resource[key] = box.checked; markDirty(); renderResources(); };
      toggles.append(el('label', { class: 'check' }, box, text));
    }
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove source') });
    remove.onclick = () => {
      if (current.map.questions?.some(q => q.sources.includes(resource.id))) return fail(t('This source belongs to a question. Change or remove that question first.'));
      current.map.resources = current.map.resources.filter(r => r !== resource);
      forgetSources([resource.id]);
      markDirty(); renderResources(); renderContexts();
    };
    const actions = [remove];
    if (resource.kind === 'repository') {
      const refresh = el('button', { type: 'button', class: 'secondary small', textContent: t('Refresh index') });
      refresh.onclick = () => busy(refresh, async () => {
        const [result] = await invoke('index_refresh', { source: resource.id });
        await loadIndexStatus();
        const sync = result?.sync ?? {};
        done(t('{id} indexed: {added} new, {updated} changed, {removed} removed files.', { id: resource.id, added: sync.added ?? 0, updated: sync.updated ?? 0, removed: sync.removed ?? 0 }));
      });
      actions.unshift(refresh);
    }
    const edit = el('details', { class: 'fold resource-edit', open: openSources.has(resource.id) },
      el('summary', { textContent: t('Edit description, topics and audience') }),
      el('div', { class: 'grid' },
        input(t('Description'), resource.description, t('Which questions it answers'), value => { resource.description = value; }),
        input(t('Topics'), resource.topics.join(', '), t('Comma separated'), value => { resource.topics = names(value); }),
        input(t('Authorized conversations'), resource.allowed_conversations.join(', '), t('Comma-separated chat IDs, or *'), value => {
          resource.allowed_conversations = names(value);
          const next = sourceState(resource);
          state.replaceWith(next);
          state = next;
        })),
      el('div', { class: 'actions' }, ...actions));
    if (resource.kind === 'urls' || Array.isArray(resource.tool?.page_urls)) {
      const urls = el('textarea', { rows: 4, value: (resource.urls ?? resource.tool.page_urls).join('\n'), spellcheck: false });
      urls.oninput = () => { if (resource.kind === 'urls') resource.urls = lines(urls.value); else resource.tool.page_urls = lines(urls.value); renderFacts(); markDirty({ target: urls }); };
      edit.insertBefore(el('label', {}, t('URLs, one per line'), urls), edit.lastElementChild);
    }
    edit.ontoggle = () => { if (edit.open) openSources.add(resource.id); else openSources.delete(resource.id); };
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { textContent: resource.id }), badge(kindName(resource), 'active'), el('span', { class: 'grow' }), state),
      facts, toggles, edit));
  }
  renderOrganization();
}

/** Removed sources leave the contexts and the organization that named them. */
function forgetSources(ids) {
  for (const context of current.map.contexts ?? []) context.sources = context.sources.filter(id => !ids.includes(id));
  const organization = current.map.organization;
  if (organization?.sources) organization.sources = organization.sources.filter(id => !ids.includes(id));
}

function renderRepositories() {
  const container = $('#repositories');
  container.replaceChildren();
  const choices = $('#source-repo');
  choices.replaceChildren();
  const entries = Object.entries(current.map.repositories);
  if (!entries.length) container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No repositories. Add a local checkout or clone one from GitHub.') })));
  for (const [alias, path] of entries) {
    choices.append(new Option(alias, alias));
    const input = el('code', { textContent: path });
    const choose = el('button', { type: 'button', class: 'secondary', textContent: t('Choose directory'), 'aria-label': t('Choose directory for {alias}', { alias }) });
    choose.onclick = () => chooseDirectories({ trigger: choose, title: t('Path of {alias}', { alias }), selected: [path], multiple: false,
      browse: path => invoke('machine_browse', { path }), apply: paths => { current.map.repositories[alias] = paths[0]; input.textContent = paths[0]; markDirty({ target: choose }); } });
    const update = el('button', { class: 'secondary', textContent: t('Update') });
    update.onclick = () => busy(update, async () => { await invoke('update_github_repository', { alias }); done(t('{alias} updated.', { alias })); });
    const remove = el('button', { class: 'danger', textContent: t('Remove') });
    remove.onclick = () => {
      const tools = current.map.resources.filter(r => r.kind === 'tool' && r.tool?.repository === alias);
      if (tools.length) return fail(t('{alias} cannot be removed: {ids} use it. Remove those sources first.', { alias, ids: tools.map(r => r.id).join(', ') }));
      const files = current.map.resources.filter(r => r.repository === alias);
      if (current.map.questions?.some(q => files.some(r => q.sources.includes(r.id)))) return fail(t('This source belongs to a question. Change or remove that question first.'));
      delete current.map.repositories[alias];
      current.map.resources = current.map.resources.filter(r => r.repository !== alias);
      forgetSources(files.map(r => r.id));
      markDirty();
      renderRepositories(); renderResources(); renderContexts();
      if (files.length) notify(t('Its sources were removed too: {ids}.', { ids: files.map(r => r.id).join(', ') }));
    };
    const source = current.map.resources.find(r => r.kind === 'repository' && r.repository === alias);
    const asSource = el('button', { class: 'secondary', textContent: t('Use as a source') });
    asSource.onclick = () => { addRepositorySource(alias, t('Knowledge in the {alias} repository', { alias })); };
    container.append(el('div', { class: 'row' },
      rowText(alias, source ? t('Git checkout · source {id}', { id: source.id }) : t('Git checkout')), input, choose,
      ...(source ? [] : [asSource]), update, remove));
  }
}

/** A repository as a source of its own, disabled until the user authorizes it. */
function addRepositorySource(alias, description) {
  let id = alias.replace(/_/g, '-');
  for (let n = 2; current.map.resources.some(r => r.id === id); n++) id = `${alias.replace(/_/g, '-').slice(0, 60)}-${n}`;
  current.map.resources.push({ id, description, topics: [], enabled: false, external_processing: false,
    allowed_conversations: [], allowed_senders: [], kind: 'repository', repository: alias });
  openSources.add(id);
  markDirty({ target: $('#repositories') });
  renderRepositories(); renderResources();
  notify(t('Source {id} added, disabled. Enable it, authorize it, choose its conversations and save.', { id }));
}

// ---------------------------------------------------------------- notes

let notesList = [];
/** Where the note being added comes from, when it is an answer: { origin, reference }. */
let noteOrigin = null;
/** Notes whose edit fold is open, and the one whose removal waits for a second tap. */
const openNotes = new Set();
let removeArmed = null;

async function loadIndexStatus() {
  indexStatus = await invoke('index_status');
  renderResources();
}

async function loadNotes() {
  const query = $('#notes-query').value.trim();
  notesList = await invoke('notes_list', query ? { query, limit: 100 } : { limit: 200 });
  renderNotes();
}

/** The contexts a note can belong to, as checkboxes. */
function contextChoices(container, chosen, onchange) {
  container.querySelectorAll('label, p').forEach(node => node.remove());
  const contexts = current.map.contexts ?? [];
  if (!contexts.length) container.append(el('p', { class: 'hint', textContent: t('Without contexts, a note belongs to every subject.') }));
  for (const context of contexts) {
    const box = el('input', { type: 'checkbox', value: context.id, checked: chosen.includes(context.id) });
    box.onchange = () => onchange(box.checked ? [...chosen.filter(id => id !== context.id), context.id] : chosen.filter(id => id !== context.id));
    container.append(el('label', { class: 'check' }, box, el('span', { textContent: context.name || context.id })));
  }
}
let newNoteContexts = [];
function renderNoteContexts() {
  contextChoices($('#note-contexts'), newNoteContexts, value => { newNoteContexts = value; renderNoteContexts(); });
}

const originNames = { agent: t('From an agent'), answer: t('From an answer'), import: t('Imported') };

function renderNotes() {
  const container = $('#notes');
  container.replaceChildren();
  $('#notes-summary').textContent = tn(notesList.length, '1 note', '{n} notes');
  $('#notes-offer').hidden = current.map.resources.some(r => r.kind === 'notes');
  if (!notesList.length) {
    container.append(el('div', { class: 'empty' }, el('p', { textContent: $('#notes-query').value.trim()
      ? t('No note matches the search.') : t('No notes yet. Add what you verified, or ask an agent to add it with pta notes add.') })));
  }
  for (const note of notesList) {
    const draft = structuredClone(note);
    const title = el('input', { value: note.title });
    title.oninput = () => { draft.title = title.value; };
    const text = el('textarea', { value: note.text, rows: 5 });
    text.oninput = () => { draft.text = text.value; };
    const keywords = el('input', { value: note.keywords.join(', ') });
    keywords.oninput = () => { draft.keywords = names(keywords.value); };
    const contexts = el('fieldset', { class: 'choices' }, el('legend', { textContent: t('Contexts') }));
    const renderChoices = () => contextChoices(contexts, draft.contexts, value => { draft.contexts = value; renderChoices(); });
    renderChoices();
    const save = el('button', { type: 'button', class: 'secondary small', textContent: t('Save note') });
    save.onclick = () => busy(save, async () => {
      await invoke('notes_save', { note: draft });
      done(t('Note saved.'));
      await loadNotes();
    });
    const remove = el('button', { type: 'button', class: 'danger small', textContent: t('Remove note') });
    remove.onclick = () => {
      if (removeArmed !== note.id) {
        removeArmed = note.id;
        remove.textContent = t('Tap again to remove');
        setTimeout(() => { if (removeArmed === note.id) { removeArmed = null; remove.textContent = t('Remove note'); } }, 4000);
        return;
      }
      removeArmed = null;
      busy(remove, async () => { await invoke('notes_remove', { id: note.id }); openNotes.delete(note.id); done(t('Note removed.')); await loadNotes(); });
    };
    const edit = el('details', { class: 'fold resource-edit', open: openNotes.has(note.id) },
      el('summary', { textContent: t('Edit note') }),
      el('label', {}, t('Title'), title), el('label', {}, t('Text'), text),
      el('label', {}, t('Keywords'), keywords), contexts,
      el('div', { class: 'actions' }, save, remove));
    edit.ontoggle = () => { if (edit.open) openNotes.add(note.id); else openNotes.delete(note.id); };
    const contextNames = note.contexts.map(id => (current.map.contexts ?? []).find(c => c.id === id)?.name || id);
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: note.title }), el('span', { class: 'grow' }),
        ...(originNames[note.origin] ? [badge(originNames[note.origin], 'info')] : []),
        ...(contextNames.length ? contextNames.map(name => badge(name, 'active')) : [badge(t('Every context'), 'muted')])),
      el('p', { class: 'resource-facts' },
        el('span', { textContent: note.text.length > 280 ? `${note.text.slice(0, 280)}…` : note.text }),
        el('small', { textContent: [note.keywords.length ? `${t('Keywords')}: ${note.keywords.join(', ')}` : '',
          t('Updated {when}', { when: ago(note.updated_at * 1000) })].filter(Boolean).join(' · ') })),
      edit));
  }
}

async function loadKnowledge() {
  await Promise.all([loadIndexStatus(), loadNotes()]);
}

async function listGithub() {
  const repos = await invoke('github_repositories');
  const container = $('#github-repositories');
  container.replaceChildren();
  if (!repos.length) { container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No repositories are installed for this GitHub App.') }))); return; }
  for (const repo of repos) {
    const alias = el('input', { spellcheck: false, 'aria-label': t('Local alias of {name}', { name: repo.full_name }),
      value: repo.full_name.split('/').pop().toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/^[^a-z]+/, 'repo-').slice(0, 64) });
    const clone = el('button', { class: 'secondary', textContent: t('Clone and add') });
    clone.onclick = () => busy(clone, () => applying(async () => {
      await invoke('clone_github_repository', { fullName: repo.full_name, alias: alias.value.trim() });
      done(t('{name} added.', { name: repo.full_name }));
    }));
    container.append(el('div', { class: 'row' }, rowText(repo.full_name, repo.private ? t('Private') : t('Public')), alias, clone));
  }
}

/** @param {any} value */
function setNoteOrigin(value) { noteOrigin = value; }
/** @param {any} value */
function setNewNoteContexts(value) { newNoteContexts = value; }

export {
  toolNames, recordSource, kindName, openSources, renderResources, renderRepositories, addRepositorySource,
  noteOrigin, loadNotes, newNoteContexts, renderNoteContexts, renderNotes, loadKnowledge, listGithub,
  setNewNoteContexts, setNoteOrigin,
};
