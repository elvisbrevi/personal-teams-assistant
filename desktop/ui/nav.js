// Navigation between sections, category tabs, and the save bar's record of unsaved sections.
import { $ } from './dom.js';
import { dirty, setDirty, setTab, tab } from './state.js';
import { fail } from './notices.js';
import { loadTodo } from './todo.js';
import { loadAgentHistory } from './todo-request.js';
import { loadMachines } from './settings-machines.js';
import { loadAccountConfig, loadAgentEndpoint } from './settings-account.js';
import { clearRevealedVariable, loadVariables } from './settings-variables.js';
import { loadUpdates } from './settings-updates.js';
import { loadCloud } from './settings-cloud.js';
import { loadRegistration, renderActivityNotice } from './activities.js';
import { loadActivity, loadMessages } from './messages.js';
import { loadKnowledge } from './knowledge.js';

/** Views opened from To-Do: they have no navigation item of their own. */
const TODO_VIEWS = ['implementation', 'create-request', 'agent-history', 'agent-guides'];
function showTab(name, anchor) {
  setTab(name);
  clearRevealedVariable();
  setNavOverflow(false);
  for (const section of ['overview', 'todo', ...TODO_VIEWS, 'settings', 'messages', 'knowledge', 'chat', 'registration']) $('#' + section).hidden = section !== name;
  for (const button of document.querySelectorAll('nav button[data-tab]')) {
    button.setAttribute('aria-current', button.dataset.tab === (TODO_VIEWS.includes(name) ? 'todo' : name) ? 'page' : 'false');
  }
  $('#nav-more').setAttribute('aria-current', ['knowledge', 'chat'].includes(name) ? 'page' : 'false');
  if (name === 'messages') loadMessages().catch(fail);
  if (name === 'overview') loadActivity();
  if (name === 'registration') loadRegistration().catch(fail);
  if (name === 'knowledge') loadKnowledge().catch(fail);
  if (name === 'todo') loadTodo();
  if (name === 'agent-history') loadAgentHistory();
  if (name === 'settings') { loadMachines().catch(fail); loadVariables(); loadAccountConfig(); loadUpdates(); loadCloud(); loadAgentEndpoint(); }
  const target = anchor && document.getElementById(anchor);
  // A panel inside a tab (a Settings section, an Activities view) opens that tab first.
  const owner = target && document.querySelector(`[aria-controls="${target.closest('[role=tabpanel]')?.id}"]`);
  if (owner) selectTab(owner.parentElement, owner.dataset.group);
  // A folded panel opens when something leads to it.
  if (target?.tagName === 'DETAILS') /** @type {HTMLDetailsElement} */ (target).open = true;
  renderSavebar();
  renderActivityNotice();
  if (target) target.scrollIntoView({ behavior: 'smooth', block: 'start' });
  else window.scrollTo(0, 0);
}

function setNavOverflow(open) {
  $('nav').classList.toggle('overflow-open', open);
  $('#nav-more').setAttribute('aria-expanded', String(open));
}

// ---------------------------------------------------------------- settings sections

/** Settings sections with edits not saved yet, so a hidden section never hides them. */
const dirtyGroups = new Set();

/** Selects the tab `name` of a tab list and shows only its panel. */
function selectTab(list, name, focus = false) {
  clearRevealedVariable();
  for (const button of list.querySelectorAll('[role=tab]')) {
    const selected = button.dataset.group === name;
    button.setAttribute('aria-selected', String(selected));
    button.tabIndex = selected ? 0 : -1;
    document.getElementById(button.getAttribute('aria-controls')).hidden = !selected;
    if (selected && focus) button.focus();
  }
}

/** Marks which sections hold unsaved edits and which need something before the assistant works. */
function renderSettingsFlags(attention = {}) {
  for (const button of document.querySelectorAll('#settings-tabs [role=tab]')) {
    const edited = button.querySelector('.flag.edited');
    const warn = button.querySelector('.flag.warn');
    if (edited) edited.hidden = !dirtyGroups.has(button.dataset.group);
    if (warn && button.dataset.group in attention) warn.hidden = !attention[button.dataset.group];
  }
}

function markDirty(event) {
  setDirty(true);
  // Called as a listener (the event) or right after a click (the focused control).
  const source = event?.target instanceof Element ? event.target : document.activeElement;
  const group = source?.closest?.('[data-settings-group]');
  if (group) dirtyGroups.add(group.dataset.settingsGroup);
  renderSavebar();
}
function renderSavebar() {
  // Settings, Knowledge and To-Do's guides are saved together with the bar.
  $('#savebar').hidden = !(dirty && ['settings', 'knowledge', 'agent-guides'].includes(tab));
  renderSettingsFlags();
}

export { showTab, setNavOverflow, dirtyGroups, selectTab, renderSettingsFlags, markDirty, renderSavebar };
