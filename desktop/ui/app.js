// The GUI's entry module: wires the page's controls to the modules and starts the GUI.
// index.html loads it with <script type="module"> and preloads its graph (modulepreload).
import { transport } from './transport.js';
import { t, tx } from './i18n.js';
import { invoke } from './control.js';
import { $, el, field, lines, names } from './dom.js';
import { current, dirty, session, setSession, tab } from './state.js';
import { chooseLook, DEFAULT_PALETTE, DEFAULT_STYLE, PALETTE_KEY, STYLE_KEY, THEME_KEY } from './appearance.js';
import { busy, done, fail, notify } from './notices.js';
import { markDirty, selectTab, setNavOverflow, showTab } from './nav.js';
import {
  backRelatedItem, closeTodoDetail, loadTodo, loadTodoDetail, setTodoLimit, TODO_LIMITS, todoLimit,
} from './todo.js';
import { loadAgentHistory, openRequest } from './todo-request.js';
import { chooseDirectories } from './directory-sheet.js';
import { loadMachines, machineRoots, renderMachineRoots, setMachineRoots } from './settings-machines.js';
import { applying, refreshLive, reload, save, startAssistant } from './profile.js';
import { renderChatList, renderStylePreview, STYLE_PRESETS } from './settings-general.js';
import { leaveAccount, loadAccountConfig, loadReadiness } from './settings-account.js';
import { loadVariables } from './settings-variables.js';
import { llmCatalog, llmRows, renderChain, setLlmCatalog } from './settings-models.js';
import {
  activityDay, activityDayFocus, activityDayRequest, activityDaysScroll, activityDetailRequest, activityRunning,
  activityTask, activityTaskFocus, activityTasksScroll, activityWindow, loadActivityTask, loadRegistration,
  setActivityDay, setActivityDayRequest, setActivityDetailRequest, setActivityRunning, setActivityTask,
  showActivityView,
} from './activities.js';
import { addEffortType, renderScheduleFields } from './settings-daily.js';
import { folded, loadActivity, loadMessages, renderMessages, setFilter } from './messages.js';
import {
  addRepositorySource, listGithub, loadNotes, newNoteContexts, noteOrigin, openSources, recordSource, sessionsSource,
  renderNoteContexts, renderNotes, renderRepositories, renderResources, setNewNoteContexts, setNoteOrigin,
} from './knowledge.js';
import {
  newQuestionSources, openContexts, openPeople, openQuestions, renderContexts, renderEntries, renderOrganization,
  renderPeople, renderQuestions,
} from './knowledge-map.js';
import { openGuides, renderGuides } from './guides.js';
import { answerBubble, bubble, chosen, resizeComposer } from './chat.js';
import { machineTrust } from './dialogs.js';
import { closeWorkflowRun, loadWorkflowRuns, showRunsList } from './workflow-runs.js';
import { showPause } from './workflow-pause.js';

// ---------------------------------------------------------------- wiring

$('#machines-refresh').onclick = () => busy($('#machines-refresh'), loadMachines);
$('#machine-save').onclick = () => busy($('#machine-save'), async () => {
  await invoke('machine_save', { name: $('#machine-name').value, roots: machineRoots });
  await loadMachines(); done(t('Machine settings saved.'));
});
$('#machine-roots-choose').onclick = () => chooseDirectories({ trigger: $('#machine-roots-choose'), title: t('Allowed directory roots'), selected: machineRoots, allowEmpty: true,
  browse: path => invoke('machine_browse', { path }), apply: paths => { setMachineRoots(paths); renderMachineRoots(); } });
$('#repo-path-choose').onclick = () => chooseDirectories({ trigger: $('#repo-path-choose'), title: t('Checkout path'), selected: lines($('#repo-path').value), multiple: false,
  browse: path => invoke('machine_browse', { path }), apply: paths => { field('#repo-path', paths[0]); $('#repo-path-value').textContent = paths[0]; } });
$('#machine-disconnect').onclick = () => busy($('#machine-disconnect'), async () => {
  await invoke('machine_disconnect');
  leaveAccount();
  await loadMachines();
  await Promise.all([loadVariables(), loadAccountConfig(), loadReadiness()]);
});
$('#todo-provider').onchange = () => {
  $('#todo-state').replaceChildren(el('option', { value: '', textContent: t('All states') }));
  loadTodo();
};
$('#todo-source').onchange = () => {
  $('#todo-state').replaceChildren(el('option', { value: '', textContent: t('All states') }));
  // Another source has its own process: its types replace the previous ones.
  if ($('#todo-kind').value.startsWith('type:')) $('#todo-kind').value = '';
  $('#todo-kind').querySelector('optgroup')?.remove();
  loadTodo();
};
$('#todo-kind').onchange = () => {
  $('#todo-state').replaceChildren(el('option', { value: '', textContent: t('All states') }));
  loadTodo();
};
$('#todo-github-kind').onchange = () => loadTodo();
$('#todo-back').onclick = () => closeTodoDetail();
$('#todo-detail-refresh').onclick = () => loadTodoDetail();
$('#todo-state').onchange = () => loadTodo();
$('#todo-more').onclick = () => busy($('#todo-more'), async () => {
  const shown = $('#todo-list').querySelectorAll('.todo-item').length;
  setTodoLimit(TODO_LIMITS.find(limit => limit > todoLimit) ?? todoLimit);
  await loadTodo({ more: true });
  // Reading continues where the list ended.
  $('#todo-list').querySelectorAll('.todo-item')[shown]?.querySelector('.todo-select')?.focus({ preventScroll: true });
});
$('#todo-agent-guides').onclick = () => { showTab('agent-guides'); $('#agent-guides h1').focus(); };
$('#agent-guides-back').onclick = () => { showTab('todo'); $('#todo-agent-guides').focus(); };
$('#teams-machine-open').onclick = () => showTab('settings', 'panel-machines');
$('#todo-create').onclick = openRequest;
$('#todo-agent-history').onclick = () => showTab('agent-history');
$('#agent-history-back').onclick = () => showTab('todo');
$('#agent-history-refresh').onclick = () => busy($('#agent-history-refresh'), loadAgentHistory);
$('#todo-refresh').onclick = () => busy($('#todo-refresh'), () => loadTodo({ keep: true }));

// After a section change, focus its heading so a screen reader starts there (no visible ring after a click).
document.querySelectorAll('section > .page-head h1').forEach(heading => { heading.tabIndex = -1; });
document.querySelectorAll('nav button[data-tab]').forEach(button => {
  button.onclick = () => { showTab(button.dataset.tab); $(`#${button.dataset.tab} h1`)?.focus({ preventScroll: true }); };
});
$('#nav-more').onclick = () => setNavOverflow($('#nav-more').getAttribute('aria-expanded') !== 'true');
document.addEventListener('click', event => { if (!/** @type {Element} */ (event.target).closest('nav')) setNavOverflow(false); });
document.addEventListener('keydown', event => {
  if (event.key === 'Escape' && $('#nav-more').getAttribute('aria-expanded') === 'true') {
    setNavOverflow(false);
    $('#nav-more').focus();
  }
});
for (const list of document.querySelectorAll('[role=tablist]')) {
  const tabs = [...list.querySelectorAll('[role=tab]')];
  for (const button of tabs) {
    button.onclick = () => selectTab(list, button.dataset.group);
    // Arrow keys, Home and End move between tabs, as in any tab list.
    button.onkeydown = event => {
      const at = tabs.indexOf(button);
      const next = { ArrowRight: at + 1, ArrowLeft: at - 1, Home: 0, End: tabs.length - 1 }[event.key];
      if (next === undefined) return;
      event.preventDefault();
      selectTab(list, tabs[(next + tabs.length) % tabs.length].dataset.group, true);
    };
  }
}
document.querySelectorAll('.cfg').forEach(input => {
  input.addEventListener('input', markDirty);
  input.addEventListener('change', markDirty);
});
$('#discover-chats').addEventListener('change', renderChatList);
$('#reading-all-chats').addEventListener('change', renderChatList);
$('#style-preset').addEventListener('change', () => {
  const chosen = $('#style-preset').value;
  const preset = STYLE_PRESETS[chosen];
  if (preset) {
    field('#style-accent', preset.accent.toLowerCase());
    field('#style-band', preset.band);
    field('#style-label-color', preset.label_color);
  } else {
    $('#style-custom').open = true;
  }
  renderStylePreview();
  $('#style-preset').value = chosen;
});
['#style-enabled', '#style-label', '#style-position', '#style-accent', '#style-band', '#style-label-color', '#style-icon', '#style-emoji', '#style-stripe-width', '#style-band-height', '#style-text-size', '#style-label-size', '#llm-language']
  .forEach(id => $(id).addEventListener('input', renderStylePreview));
// A suggested emoji fills the field as if typed, so it marks the section edited and redraws the preview.
document.querySelectorAll('#style-emoji-choices button').forEach(button => {
  button.addEventListener('click', () => {
    $('#style-emoji').value = button.dataset.emoji;
    $('#style-emoji').dispatchEvent(new Event('input', { bubbles: true }));
  });
});
document.querySelectorAll('#theme-choice button').forEach(button => {
  button.onclick = () => chooseLook(THEME_KEY, 'theme', button.dataset.themeChoice === 'system' ? null : button.dataset.themeChoice);
});
document.querySelectorAll('#palette-choice button').forEach(button => {
  button.onclick = () => chooseLook(PALETTE_KEY, 'palette', button.dataset.palette === DEFAULT_PALETTE ? null : button.dataset.palette);
});
document.querySelectorAll('#style-choice button').forEach(button => {
  button.onclick = () => chooseLook(STYLE_KEY, 'style', button.dataset.style === DEFAULT_STYLE ? null : button.dataset.style);
});
$('#registration-schedule').addEventListener('change', renderScheduleFields);
$('#registration-effort-type-add').addEventListener('click', addEffortType);
$('#notice-close').onclick = () => { $('#notice').hidden = true; };
$('#save').onclick = () => busy($('#save'), async () => { await save(); done(t('Settings saved.')); });
$('#discard').onclick = () => busy($('#discard'), async () => { await reload(); notify(t('Changes discarded.')); });
$('#start').onclick = () => startAssistant($('#start'));
$('#restart').onclick = () => busy($('#restart'), async () => {
  if (dirty) await save();
  await invoke('restart');
  await refreshLive();
  done(t('Assistant restarted with the saved settings.'));
});
$('#stop').onclick = () => busy($('#stop'), async () => { await invoke('stop_assistant'); await refreshLive(); done(t('Assistant stopped.')); });
$('#activity-all').onclick = () => showTab('messages');
$('#workflow-attention-open').onclick = () => showRunsList();
$('#runs-refresh').onclick = () => loadWorkflowRuns();
$('#runs-pause-open').onclick = () => showPause(loadWorkflowRuns);
$('#runs-state').onchange = () => loadWorkflowRuns();
$('#runs-workflow').onchange = () => loadWorkflowRuns();
$('#run-detail-back').onclick = () => closeWorkflowRun();
// The Runs tab reads its list when chosen with a click or the keyboard (arrow keys focus the tab they select).
for (const event of ['click', 'focus']) $('#workflows-tab-runs').addEventListener(event, () => { if (!$('#workflows-runs').hidden && $('#workflow-run-detail').hidden) loadWorkflowRuns(); });
$('#activity-notice-open').onclick = () => busy($('#activity-notice-open'), () => invoke('activity_open'));
$('#activity-settings').onclick = () => showTab('settings', 'panel-registration');
$('#activity-refresh').onclick = () => busy($('#activity-refresh'), loadRegistration);
$('#activity-log-from').onchange = $('#activity-log-to').onchange = () => loadRegistration().catch(fail);
$('#activity-run').onclick = () => busy($('#activity-run'), async () => {
  if (dirty) await save();
  await invoke('activity_run', { day: $('#activity-day').value || null });
  setActivityRunning(true);
  await loadRegistration();
});
$('#activity-days-back').onclick = () => {
  setActivityDayRequest(activityDayRequest + 1); setActivityDetailRequest(activityDetailRequest + 1);
  setActivityDay(null); setActivityTask(null);
  showActivityView('master');
  activityDayFocus?.focus({ preventScroll: true }); window.scrollTo(0, activityDaysScroll);
};
$('#activity-task-back').onclick = () => {
  if (backRelatedItem('activity')) return;
  setActivityDetailRequest(activityDetailRequest + 1); setActivityTask(null);
  showActivityView('day-view');
  activityTaskFocus?.focus({ preventScroll: true }); window.scrollTo(0, activityTasksScroll);
};
$('#activity-detail-refresh').onclick = () => loadActivityTask();
setInterval(() => { if (current && (tab === 'registration' || activityRunning)) loadRegistration().catch(() => {}); }, 5_000);

$('#messages-refresh').onclick = () => busy($('#messages-refresh'), loadMessages);
document.querySelectorAll('#messages-filters button').forEach(button => { button.onclick = () => setFilter(button.dataset.filter); });
$('#messages-hide-ineligible').onchange = renderMessages;
$('#messages-search').oninput = renderMessages;
setInterval(() => {
  // Not while context is typed for an answer: a rebuilt list would move it under the user.
  if (tab === 'messages' && $('#messages-auto').checked && !document.activeElement?.classList?.contains('answer-context')
    && !document.activeElement?.closest?.('.question-association')) loadMessages().catch(() => {});
}, 10_000);
$('#add-llm').onclick = () => {
  const info = llmCatalog.providers.find(provider => provider.id === $('#add-llm-provider').value);
  if (!info || llmRows.length >= 16) return;
  const model = info.models[0];
  if (info.id === 'opencode' && !model) return;
  llmRows.push({ provider: info.id, model: model?.id ?? '', effort: model?.default_effort ?? info.efforts[0], enabled: info.available });
  markDirty({ target: $('#llm-chain') });
  renderChain();
  $('#llm-chain li:last-child .field input, #llm-chain li:last-child .field select')?.focus();
};
$('#detect-llm').onclick = () => busy($('#detect-llm'), async () => {
  setLlmCatalog(await invoke('llm_providers')); renderChain(); done(t('CLIs detected again.'));
});
$('#teams-login').onclick = () => busy($('#teams-login'), () => applying(async () => {
  await invoke('connect_microsoft'); done(t('Microsoft account connected.'));
}));
$('#import').onclick = () => busy($('#import'), async () => {
  await invoke('import_existing', { path: $('#legacy-path').value.trim() }); await reload(); done(t('Settings imported.'));
});
$('#self-chat-enable').onclick = () => busy($('#self-chat-enable'), () => applying(async () => {
  await invoke('self_chat_enable', { id: $('#self-chat-id').value.trim() || null });
  done(t('Personal chat validated and enabled. Source audiences are kept.'));
}));
$('#self-chat-disable').onclick = () => busy($('#self-chat-disable'), () => applying(async () => {
  await invoke('self_chat_disable'); done(t('Personal chat disabled.'));
}));
$('#self-chat-test').onclick = () => busy($('#self-chat-test'), async () => {
  const result = await invoke('test_self_chat');
  if (result.account_scope_verified) done(t('Personal chat validated. Write a new question in Teams to check reception and the answer.'));
  else notify(t('Incomplete test.'));
});
$('#add-repo').onclick = () => {
  const alias = $('#repo-alias').value.trim();
  const path = $('#repo-path').value.trim();
  const description = $('#repo-description').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(alias) || !path) return fail(t('Give a valid alias (lowercase letters, digits, - or _) and the checkout path.'));
  if (current.map.repositories[alias]) return fail(t('That alias already exists.'));
  current.map.repositories[alias] = path;
  field('#repo-alias', ''); field('#repo-path', ''); $('#repo-path-value').textContent = ''; field('#repo-description', '');
  markDirty({ target: $('#add-repo') });
  if (description) addRepositorySource(alias, description);
  else renderRepositories();
};
$('#github-connect').onclick = () => busy($('#github-connect'), async () => {
  const prompt = await invoke('begin_github_login', { clientId: $('#github-client-id').value.trim() });
  $('#github-code').textContent = prompt.user_code;
  $('#github-device').hidden = false;
  notify(t('Code ready. Authorize the GitHub App with the repositories you want.'));
});
$('#github-open').onclick = () => {
  if (transport.browser) window.open('https://github.com/login/device', '_blank', 'noopener,noreferrer');
  else invoke('open_github_login').catch(fail);
};
$('#github-finish').onclick = () => busy($('#github-finish'), async () => {
  await invoke('finish_github_login');
  $('#github-device').hidden = true;
  await refreshLive();
  await listGithub();
  done(t('GitHub connected.'));
});
$('#github-list').onclick = () => busy($('#github-list'), listGithub);
$('#github-disconnect').onclick = () => busy($('#github-disconnect'), async () => {
  await invoke('disconnect_github');
  $('#github-repositories').replaceChildren();
  await refreshLive();
  done(t('GitHub disconnected. Local checkouts stay.'));
});
$('#source-kind').onchange = () => { $('#source-path-label').hidden = $('#source-kind').value !== 'file'; };
$('#add-source').onclick = () => {
  const id = $('#source-id').value.trim();
  const repository = $('#source-repo').value;
  const whole = $('#source-kind').value === 'repository';
  const path = $('#source-path').value.trim();
  const description = $('#source-description').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || !repository || (!whole && !path) || !description) return fail(whole
    ? t('Fill in the ID (lowercase letters, digits, - or _), repository and description.')
    : t('Fill in the ID (lowercase letters, digits, - or _), repository, file and description.'));
  if (current.map.resources.some(r => r.id === id)) return fail(t('That ID already exists.'));
  current.map.resources.push({ id, description, topics: names($('#source-topics').value), enabled: false,
    external_processing: false, allowed_conversations: [], allowed_senders: [],
    ...(whole ? { kind: 'repository', repository } : { kind: 'file', repository, path }) });
  for (const name of ['#source-id', '#source-path', '#source-topics', '#source-description']) field(name, '');
  markDirty();
  renderResources();
  notify(t('Source {id} added, disabled. Save the changes to keep it.', { id }));
};
$('#add-url-source').onclick = () => {
  const id = $('#url-source-id').value.trim(), urls = [...new Set(lines($('#url-source-addresses').value))];
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || current.map.resources.some(r => r.id === id)) return fail(t('Choose a unique source ID using lowercase letters, digits, - or _.'));
  if (!urls.length || urls.length > 10) return fail(t('Add between one and ten HTTPS URLs.'));
  let parsed;
  try { parsed = urls.map(value => new URL(value)); } catch { return fail(t('Add between one and ten HTTPS URLs.')); }
  if (parsed.some(url => url.protocol !== 'https:' || url.username || url.password || url.hash || (url.port && url.port !== '443'))) return fail(t('Add between one and ten HTTPS URLs.'));
  const wiki = current.map.resources.find(r => r.id === $('#url-source-wiki').value);
  if (parsed.some(url => url.hostname === 'dev.azure.com') && !wiki) return fail(t('Select an Azure DevOps Wiki connection for these pages.'));
  if (wiki && parsed.some(url => url.hostname !== 'dev.azure.com' || !url.pathname.includes('/_wiki/wikis/'))) return fail(t('The Wiki connection reads only Azure DevOps Wiki page URLs.'));
  current.map.resources.push({ id, description: $('#url-source-description').value.trim() || id, topics: [], enabled: false, external_processing: false, allowed_conversations: [], allowed_senders: [],
    ...(wiki ? { kind: 'tool', tool: { ...structuredClone(wiki.tool), repository_docs: false, page_urls: urls } } : { kind: 'urls', urls }) });
  for (const selector of ['#url-source-id', '#url-source-description', '#url-source-addresses']) field(selector, '');
  $('#url-source-form').open = false; openSources.add(id);
  markDirty({ target: $('#add-url-source') }); renderResources(); renderQuestions();
  notify(t('Source {id} added, disabled. Enable it, authorize it, choose its conversations and save.', { id }));
};
$('#add-record').onclick = () => {
  if (current.map.resources.some(r => r.tool?.type === 'activity_record')) return;
  const source = recordSource();
  if (current.map.resources.some(r => r.id === source.id)) return fail(t('That ID already exists.'));
  current.map.resources.push(source);
  openSources.add(source.id);
  markDirty({ target: $('#add-record') });
  renderResources();
  notify(t('Source {id} added, disabled. Enable it, authorize it, choose its conversations and save.', { id: source.id }));
};
$('#add-sessions-source').onclick = () => {
  if (current.map.resources.some(r => r.tool?.type === 'agent_sessions')) return;
  const source = sessionsSource();
  current.map.resources.push(source);
  openSources.add(source.id);
  markDirty({ target: $('#add-sessions-source') });
  renderResources();
  notify(t('Source {id} added, disabled. Enable it, authorize it, choose its conversations and save.', { id: source.id }));
};
$('#add-person').onclick = () => {
  const name = $('#person-name').value.trim();
  const sender = $('#person-sender').value.trim();
  if (!name) return fail(t('Write the person\'s name.'));
  const people = current.map.people ??= [];
  const base = folded(name).replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').replace(/^[^a-z]+/, '') || 'person';
  let id = base.slice(0, 60);
  for (let n = 2; people.some(p => p.id === id); n++) id = `${base.slice(0, 56)}-${n}`;
  people.push({ id, name, senders: sender ? [sender] : [], aliases: [], address: $('#person-address').value.trim(), notes: '' });
  openPeople.add(id);
  for (const name of ['#person-name', '#person-sender', '#person-address']) field(name, '');
  $('#person-form').open = false;
  markDirty({ target: $('#add-person') });
  renderPeople();
  notify(t('{name} added. Save the changes to keep it.', { name }));
};
$('#add-question').onclick = () => {
  const text = $('#question-text').value.trim();
  if (!text || !newQuestionSources.size) return fail(t('Write a question and select at least one source.'));
  const question = { id: `question-${crypto.randomUUID().replaceAll('-', '')}`, text, sources: [...newQuestionSources], examples: [] };
  (current.map.questions ??= []).push(question);
  openQuestions.add(question.id);
  field('#question-text', ''); newQuestionSources.clear();
  $('#question-form').open = false;
  markDirty({ target: $('#add-question') }); renderQuestions();
  notify(t('Question added. Save the changes to keep it.'));
};
$('#add-context').onclick = () => {
  const id = $('#context-id').value.trim();
  const name = $('#context-name').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || !name) return fail(t('Give the context an ID (lowercase letters, digits, - or _) and a name.'));
  const contexts = current.map.contexts ??= [];
  if (contexts.some(c => c.id === id)) return fail(t('That ID already exists.'));
  contexts.push({ id, name, description: '', keywords: names($('#context-keywords').value), sources: [], instructions: '' });
  openContexts.add(id);
  for (const name of ['#context-id', '#context-name', '#context-keywords']) field(name, '');
  markDirty({ target: $('#add-context') });
  renderContexts(); renderNoteContexts();
  notify(t('Context {id} added. Choose its sources and save the changes.', { id }));
};
$('#add-guide').onclick = () => {
  const id = $('#guide-id').value.trim();
  const type = $('#guide-type').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || !type) return fail(t('Give the guide an ID (lowercase letters, digits, - or _) and the type as the provider names it.'));
  const guides = current.map.work_item_guides ??= [];
  if (guides.some(g => g.id === id)) return fail(t('That ID already exists.'));
  guides.push({ id, provider: $('#guide-provider').value, type, name: $('#guide-name').value.trim(), projects: [], purpose: '', instructions: '', fields: [] });
  openGuides.add(id);
  for (const name of ['#guide-id', '#guide-type', '#guide-name']) field(name, '');
  $('#guide-form').open = false;
  markDirty({ target: $('#add-guide') });
  renderGuides(); renderOrganization();
  notify(t('Guide {id} added. Describe how the type is used and save the changes.', { id }));
};
$('#add-entry').onclick = () => {
  const id = $('#entry-id').value.trim();
  const title = $('#entry-title').value.trim();
  const text = $('#entry-text').value;
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || !title || !text.trim()) return fail(t('Give the entry an ID (lowercase letters, digits, - or _), a title and a text.'));
  const organization = current.map.organization ??= {};
  const entries = organization.entries ??= [];
  if (entries.some(e => e.id === id)) return fail(t('That ID already exists.'));
  entries.push({ id, kind: $('#entry-kind').value, title, text, projects: [] });
  for (const name of ['#entry-id', '#entry-title', '#entry-text']) field(name, '');
  $('#entry-form').open = false;
  markDirty({ target: $('#add-entry') });
  renderEntries(); renderOrganization();
  notify(t('Entry {id} added. Save the changes to keep it.', { id }));
};
$('#add-organization-source').onclick = () => {
  if (current.map.resources.some(r => r.kind === 'organization')) return;
  const id = current.map.resources.some(r => r.id === 'organization') ? 'organization-entries' : 'organization';
  current.map.resources.push({ id, description: t('How my organization works: norms, architecture, conventions and work item guides.'),
    topics: [], enabled: false, external_processing: false, allowed_conversations: [], allowed_senders: [], kind: 'organization' });
  openSources.add(id);
  markDirty({ target: $('#add-organization-source') });
  renderResources();
  selectTab($('#knowledge-tabs'), 'sources');
  notify(t('Source {id} added, disabled. Enable it, authorize it, choose its conversations and save.', { id }));
};
$('#add-notes-source').onclick = () => {
  if (current.map.resources.some(r => r.kind === 'notes')) return;
  const id = current.map.resources.some(r => r.id === 'notes') ? 'verified-notes' : 'notes';
  current.map.resources.push({ id, description: t('Facts I verified: procedures, decisions and how things are done here.'),
    topics: [], enabled: false, external_processing: false, allowed_conversations: [], allowed_senders: [], kind: 'notes' });
  openSources.add(id);
  markDirty({ target: $('#add-notes-source') });
  renderResources(); renderNotes();
  selectTab($('#knowledge-tabs'), 'sources');
  notify(t('Source {id} added, disabled. Enable it, authorize it, choose its conversations and save.', { id }));
};
// A note started from an answer stops being that answer's once the form closes.
$('#note-form').ontoggle = () => { if (!$('#note-form').open) setNoteOrigin(null); };
let notesSearch;
$('#notes-query').oninput = () => { clearTimeout(notesSearch); notesSearch = setTimeout(() => loadNotes().catch(fail), 300); };
$('#add-note').onclick = () => busy($('#add-note'), async () => {
  const note = { title: $('#note-title').value.trim(), text: $('#note-text').value.trim(),
    keywords: names($('#note-keywords').value), contexts: newNoteContexts, ...(noteOrigin ?? {}) };
  if (!note.title || !note.text) return fail(t('A note needs a title and a text.'));
  await invoke('notes_save', { note });
  for (const name of ['#note-title', '#note-text', '#note-keywords']) field(name, '');
  setNewNoteContexts([]); renderNoteContexts();
  setNoteOrigin(null);
  $('#note-form').open = false;
  done(t('Note saved.'));
  await loadNotes();
});
$('#chat-all').onclick = () => {
  const boxes = [...$('#chat-sources').querySelectorAll('input')];
  const all = boxes.every(box => box.checked);
  for (const box of boxes) { box.checked = !all; if (box.checked) chosen.add(box.value); else chosen.delete(box.value); }
};
$('#chat-reset').onclick = () => {
  setSession(crypto.randomUUID());
  $('#history').replaceChildren($('#chat-empty'));
  $('#chat-empty').hidden = false;
};
$('#message').addEventListener('input', resizeComposer);
$('#message').addEventListener('keydown', event => {
  if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) { event.preventDefault(); $('#send').click(); }
});
$('#chat-form').onsubmit = async event => {
  event.preventDefault();
  const text = $('#message').value.trim();
  const sources = [...chosen];
  if (!text) return;
  if (!sources.length) return fail(t('Select at least one source for the test.'));
  const send = $('#send');
  send.disabled = true;
  bubble('you', text);
  field('#message', '');
  resizeComposer();
  const elapsed = el('span', { textContent: t('Writing…') });
  const pending = bubble('bot', el('span', { class: 'typing' }, el('i'), el('i'), el('i'), elapsed));
  const started = Date.now();
  const timer = setInterval(() => { elapsed.textContent = t('Writing… {n} s', { n: Math.round((Date.now() - started) / 1000) }); }, 1000);
  try {
    const result = await invoke('chat', { input: { session, text, group: false, mentioned: false, sources } });
    pending.remove();
    answerBubble({ ...result, request: text });
  } catch (error) {
    pending.remove();
    bubble('bot none', t('The test could not complete: {error}', { error: tx(String(error?.message ?? error)) }));
  } finally {
    clearInterval(timer);
    send.disabled = false;
  }
};

reload().then(async () => { await loadActivity(); await loadRegistration(); if (activityWindow) showTab('registration'); }).catch(fail);

if (transport.browser) {
  transport.ready.then(identity => {
    const enroll = new URLSearchParams(window.location.search).get('enroll');
    if (enroll) machineTrust(enroll).catch(fail);
    $('#web-account').hidden = $('#panel-web-account').hidden = $('#web-hosting-note').hidden = false;
    $('#web-username').textContent = identity.username;
    for (const id of ['#public-url', '#bind', '#tunnel-config', '#cloudflare-tunnel']) $(id).disabled = true;
    for (const button of [$('#web-logout'), $('#web-logout-settings')]) {
      button.onclick = () => busy(button, async () => {
        leaveAccount();
        const result = await transport.request('/api/logout', {});
        window.location.replace(result.logout_url);
      });
    }
  }).catch(fail);
}
