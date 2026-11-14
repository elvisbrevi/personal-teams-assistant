// Loading the profile into the forms, refreshing the saved state, and saving or applying changes.
import { t, useLanguage } from './i18n.js';
import { invoke, lastRevision } from './control.js';
import { $, field, lines } from './dom.js';
import { current, dirty, live, loadedProfile, setCurrent, setDirty, setLive, setLoadedProfile, tab } from './state.js';
import { busy, done } from './notices.js';
import { dirtyGroups, renderSavebar } from './nav.js';
import { loadAzureCliStatus, loadGithubCliStatus, renderLive, serverHost, urlReady } from './home.js';
import { collectMessageStyle, renderChatList, renderMessageStyle } from './settings-general.js';
import { collectEmbeddings, llmRows, loadChain, loadSelfChat, renderEmbeddings } from './settings-models.js';
import { collectRegistrationSettings, renderRegistrationSettings } from './settings-daily.js';
import { loadActivity } from './messages.js';
import { renderNoteContexts, renderRepositories, renderResources } from './knowledge.js';
import { renderContexts, renderEntries, renderOrganization, renderPeople, renderQuestions } from './knowledge-map.js';
import { renderAgentPrompts, renderGuides } from './guides.js';
import { renderChatSources } from './chat.js';
import { checkWorkflowSettings, discardWorkflowSettings, saveWorkflowSettings } from './settings-workflows.js';

function profileKey(snapshot) { return JSON.stringify([snapshot.config, snapshot.map]); }

/**
 * Whether the saved configuration changed underneath a form without unsaved edits (an account
 * synchronization, restored setup, `pta config set`): then the whole form reloads, so it shows what
 * is saved and a later save cannot write an older copy over it.
 */
async function reloadIfChanged() {
  if (!current || dirty || profileKey(live) === loadedProfile) return false;
  await reload();
  return true;
}

/** Refreshes persisted state without touching unsaved form edits. */
async function refreshLive() {
  setLive(await invoke('snapshot'));
  if (current) {
    current.credentials = live.credentials; current.github_connected = live.github_connected;
    if (await reloadIfChanged()) return;
  }
  renderLive();
  renderChatSources();
}

/** The regular poll: live state, and the form again when the saved configuration changed. */
async function pollLive() {
  if (!current) return;
  try {
    setLive(await invoke('snapshot'));
    if (!await reloadIfChanged()) renderLive();
  } catch { /* the next tick retries */ }
  if (tab === 'overview') loadActivity();
}

async function reload() {
  setCurrent(await invoke('snapshot'));
  current.revision = lastRevision;
  setLoadedProfile(profileKey(current));
  setLive(structuredClone(current));
  const config = current.config;
  // The interface follows the configured language; a change reloads the page once.
  if (useLanguage(config.llm.language ?? 'es')) return;
  field('#tenant-id', config.graph.tenant_id); field('#client-id', config.graph.client_id);
  field('#user-id', config.graph.user_id); field('#public-url', config.server.public_url);
  // Set up once and rarely changed: the address folds away while it is configured.
  $('#panel-server').open = !urlReady(config);
  $('#server-summary').textContent = urlReady(config) ? serverHost(config) : t('Not configured');
  field('#bind', config.server.bind); field('#llm-style', config.llm.style);
  field('#llm-language', config.llm.language ?? 'es');
  field('#tunnel-config', current.tunnel_config);
  $('#cloudflare-tunnel').checked = config.server.cloudflare_tunnel;
  field('#allowed-chats', config.graph.allowed_chats.join('\n'));
  $('#dry-run').checked = config.policy.dry_run;
  field('#history-messages', config.policy.history_messages ?? 10);
  $('#discover-chats').checked = config.graph.discover_all_chats;
  const reading = config.teams_reading ?? {};
  $('#reading-all-chats').checked = Boolean(reading.all_chats);
  $('#reading-personal-chat').checked = Boolean(reading.personal_chat);
  field('#reading-chats', (reading.chats ?? []).join('\n'));
  renderChatList();
  renderRegistrationSettings();
  renderEmbeddings();
  renderMessageStyle();
  setDirty(false);
  dirtyGroups.clear();
  renderSavebar();
  renderLive();
  renderRepositories(); renderResources(); renderContexts(); renderQuestions(); renderPeople(); renderOrganization(); renderEntries(); renderGuides(); renderAgentPrompts(); renderNoteContexts(); renderChatSources();
  discardWorkflowSettings();
  loadAzureCliStatus(); loadGithubCliStatus();
  await Promise.all([loadChain(), loadSelfChat()]);
}

setInterval(pollLive, 15_000);

function collectSettings() {
  const config = current.config;
  config.llm.chain = llmRows.map(({ provider, model, effort, enabled }) => ({ provider, model, effort, enabled }));
  config.llm.style = $('#llm-style').value.trim();
  config.llm.language = $('#llm-language').value;
  config.graph.tenant_id = $('#tenant-id').value.trim();
  config.graph.client_id = $('#client-id').value.trim();
  config.graph.user_id = $('#user-id').value.trim();
  config.graph.allowed_chats = lines($('#allowed-chats').value);
  config.graph.discover_all_chats = $('#discover-chats').checked;
  config.teams_reading = {
    all_chats: $('#reading-all-chats').checked,
    personal_chat: $('#reading-personal-chat').checked,
    chats: [...new Set(lines($('#reading-chats').value))],
  };
  config.server.public_url = $('#public-url').value.trim();
  config.server.bind = $('#bind').value.trim();
  config.server.cloudflare_tunnel = $('#cloudflare-tunnel').checked;
  config.policy.dry_run = $('#dry-run').checked;
  const history = Number($('#history-messages').value);
  if (!Number.isInteger(history) || history < 1 || history > 50) throw t('Earlier messages read must be a whole number from 1 to 50.');
  config.policy.history_messages = history;
  collectRegistrationSettings();
  collectEmbeddings();
  collectMessageStyle();
  checkWorkflowSettings();
}

async function save() {
  if (!llmRows.some(row => row.enabled)) throw t('Turn on at least one language model.');
  collectSettings();
  await invoke('save_settings', { config: current.config, map: current.map, tunnelConfig: $('#tunnel-config').value.trim() });
  // Settings › Workflows is saved by its own operation, after the profile it does not change.
  await saveWorkflowSettings();
  await reload();
}

/** Operations that change the profile on the host: pending edits are saved first, then everything reloads. */
async function applying(task) {
  if (dirty) await save();
  const result = await task();
  await reload();
  return result;
}

function startAssistant(button) {
  return busy(button, async () => {
    if (dirty) await save();
    await invoke('start_assistant');
    await refreshLive();
    done(t('Assistant started.'));
  });
}

export { refreshLive, pollLive, reload, save, applying, startAssistant };
