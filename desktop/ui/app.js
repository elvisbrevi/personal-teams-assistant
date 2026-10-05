const rawInvoke = window.__TAURI__.core.invoke;
const READS = ['snapshot', 'chat', 'github_repositories', 'self_chat_status', 'llm_providers', 'audit'];
async function invoke(method, args = {}) {
  let result = await rawInvoke('command', { request: {
    method, args, revision: READS.includes(method) ? null : current?.revision ?? null, contract: 1
  }});
  if (result.code === 'authorization_pending' && ['connect_microsoft', 'finish_github_login'].includes(method)) {
    const finish = method === 'connect_microsoft' ? 'finish_microsoft' : 'finish_github_login';
    const deadline = Date.now() + 600_000;
    notify('Authorization pending in the browser.', 'info');
    while (result.code === 'authorization_pending' && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 2000));
      result = await rawInvoke('command', { request: { method: finish, args: {}, revision: null, contract: 1 } });
    }
  }
  if (result.revision && current) current.revision = result.revision;
  if (!result.ok) throw result.message || result.code;
  return result.data;
}

const $ = (selector) => document.querySelector(selector);
/** Snapshot being edited by the forms (saved with «Save changes»). */
let current;
/** Latest persisted snapshot: drives status, overview and the test chat (which reads the saved map). */
let live;
let tab = 'overview';
let dirty = false;
let session = crypto.randomUUID();

function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null) continue;
    if (key === 'class') node.className = value;
    else if (key in node) node[key] = value;
    else node.setAttribute(key, value);
  }
  node.append(...children.filter(child => child !== null && child !== undefined && child !== false));
  return node;
}
function icon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', 'icon');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.append(use);
  return svg;
}
const badge = (text, tone = 'muted') => el('span', { class: `badge ${tone}`, textContent: text });
function field(id, value) { $(id).value = value ?? ''; }
function lines(text) { return text.split('\n').map(s => s.trim()).filter(Boolean); }
function names(text) { return text.split(',').map(s => s.trim()).filter(Boolean); }

let noticeTimer;
function notify(text, tone = 'info') {
  const box = $('#notice');
  $('#notice-text').textContent = String(text?.message ?? text);
  box.className = `toast ${tone}`;
  box.hidden = false;
  clearTimeout(noticeTimer);
  if (tone !== 'error') noticeTimer = setTimeout(() => { box.hidden = true; }, 6000);
}
const done = text => notify(text, 'ok');
const fail = error => notify(error, 'error');

/** Runs one operation with the button showing progress; errors become a notice. */
async function busy(button, task) {
  if (button) { button.disabled = true; button.classList.add('busy'); }
  try { return await task(); }
  catch (error) { fail(error); }
  finally {
    if (button) { button.disabled = false; button.classList.remove('busy'); }
    renderLive();
  }
}

function showTab(name, anchor) {
  tab = name;
  for (const section of ['overview', 'settings', 'messages', 'knowledge', 'chat']) $('#' + section).hidden = section !== name;
  for (const button of document.querySelectorAll('nav button')) {
    button.setAttribute('aria-current', button.dataset.tab === name ? 'page' : 'false');
  }
  if (name === 'messages') loadMessages().catch(fail);
  if (name === 'overview') loadActivity();
  renderSavebar();
  const target = anchor && document.getElementById(anchor);
  if (target) target.scrollIntoView({ behavior: 'smooth', block: 'start' });
  else window.scrollTo(0, 0);
}

function markDirty() { dirty = true; renderSavebar(); }
function renderSavebar() { $('#savebar').hidden = !(dirty && ['settings', 'knowledge'].includes(tab)); }

// ---------------------------------------------------------------- status and overview

const GENERATED = ['GRAPH_WEBHOOK_SECRET', 'STATE_ENCRYPTION_KEY'];
const providerNames = { codex: 'Codex', claude: 'Claude Code', deepseek: 'DeepSeek' };

function expectedSubscriptions(config) {
  return config.graph.discover_all_chats ? 1 : config.graph.allowed_chats.length + config.graph.channels.length;
}
const entraReady = config => !config.graph.client_id.endsWith('0002') && !config.graph.tenant_id.endsWith('0001');
const urlReady = config => !config.server.public_url.includes('example.com');
const readySources = map => map.resources.filter(r => r.enabled && r.external_processing && r.allowed_conversations.length);
const missingCredentials = credentials => Object.entries(credentials).filter(([name, source]) => source === 'missing' && !GENERATED.includes(name)).map(([name]) => name);
const enabledChain = config => config.llm.chain.length
  ? config.llm.chain.filter(choice => choice.enabled)
  : [{ provider: config.llm.provider, model: config.llm.model }];

function assistantState(snapshot) {
  const config = snapshot.loaded_config ?? snapshot.config;
  const observing = config.policy.dry_run;
  if (!snapshot.running) {
    return { tone: 'muted', pill: 'Assistant stopped', title: 'Assistant stopped',
      detail: 'It neither receives nor answers Teams messages. The test chat still works.' };
  }
  let tone = observing ? 'info' : 'ok';
  let teams;
  const expected = expectedSubscriptions(config);
  if (!expected) {
    teams = config.graph.self_chat ? 'Personal chat only' : 'No chats set up to receive';
    if (!config.graph.self_chat) tone = 'warn';
  } else if (snapshot.active_subscriptions >= expected) {
    teams = `Teams: ${snapshot.active_subscriptions} active subscription(s)`;
  } else {
    const code = snapshot.subscription_issue?.match(/graph_http_(\d{3})/)?.[1];
    teams = code ? `Teams pending (Graph HTTP ${code})` : 'Teams subscription pending';
    tone = 'warn';
  }
  return {
    tone,
    pill: tone === 'warn' ? 'Running · Teams pending' : observing ? 'Running · observing' : 'Running',
    title: observing ? 'Running in observation mode' : 'Running and answering',
    detail: `${observing ? 'Drafts proposals without sending them' : 'Answers in Teams as you'} · ${teams}`,
  };
}

function tile(label, value, tone, detail) {
  return el('div', { class: 'tile' }, el('small', { textContent: label }),
    el('strong', {}, el('span', { class: `dot ${tone}` }), value), el('p', { textContent: detail }));
}

function renderTiles(s) {
  const config = s.config;
  const chain = enabledChain(config);
  const [first, ...rest] = chain;
  const ready = readySources(s.map).length;
  const tunnelManaged = config.server.cloudflare_tunnel || s.tunnel_config.trim();
  let host = 'Public URL missing';
  try { if (urlReady(config)) host = new URL(config.server.public_url).host; } catch { /* shown as missing */ }
  $('#tiles').replaceChildren(
    tile('Microsoft account', s.microsoft_connected ? 'Connected' : 'Not connected', s.microsoft_connected ? 'ok' : 'warn',
      s.microsoft_connected ? 'Reads and answers as you' : 'Connect it in Settings'),
    tile('Mode', config.policy.dry_run ? 'Observation' : 'Sending on', config.policy.dry_run ? 'info' : 'ok',
      config.policy.dry_run ? 'Sends nothing to Teams' : 'Answers in Teams'),
    tile('Reception', config.graph.discover_all_chats ? 'All my chats' : `${config.graph.allowed_chats.length} chat(s)`,
      s.running && s.active_subscriptions ? 'ok' : 'muted',
      s.running ? `${s.active_subscriptions} active subscription(s)` : 'Inactive while stopped'),
    tile('Tunnel', tunnelManaged ? (s.tunnel_running ? 'Running' : 'Stopped') : 'External', tunnelManaged && s.tunnel_running ? 'ok' : 'muted', host),
    tile('Models', first ? `${providerNames[first.provider] || first.provider}` : 'None active', first ? 'ok' : 'warn',
      first ? `${first.model}${rest.length ? ` · fallback: ${rest.map(c => providerNames[c.provider] || c.provider).join(' → ')}` : ' · no fallback'}` : 'Turn one on in Settings'),
    tile('Sources', `${ready} of ${s.map.resources.length} ready`, ready ? 'ok' : 'warn', 'Enabled, authorized and with an audience'),
    tile('Personal chat', config.graph.self_chat ? 'Enabled' : 'Disabled', config.graph.self_chat ? 'ok' : 'muted',
      config.graph.self_chat ? 'Ask it from your Teams notes' : 'Optional'),
  );
}

function renderSteps(s) {
  const config = s.config;
  const missing = missingCredentials(s.credentials);
  const steps = [
    { done: !missing.length, title: 'Credentials', text: missing.length ? `Missing: ${missing.join(', ')}.` : 'The required credentials are stored.', go: ['settings', 'panel-credentials'] },
    { done: enabledChain(config).length > 0, title: 'Language model', text: 'Turn on and order Codex, Claude Code or DeepSeek.', go: ['settings', 'panel-llm'] },
    { done: entraReady(config), title: 'Entra registration', text: 'Tenant and client ID of your existing registration.', go: ['settings', 'panel-teams'] },
    { done: s.microsoft_connected, title: 'Microsoft account', text: 'Sign in to read and answer as you.', go: ['settings', 'panel-teams'] },
    { done: urlReady(config), title: 'Public URL and tunnel', text: 'A stable HTTPS URL that reaches the local port.', go: ['settings', 'panel-server'] },
    { done: readySources(s.map).length > 0, title: 'Authorized sources', text: 'At least one source enabled, with external processing and an audience.', go: ['knowledge'] },
    { done: s.running, title: 'Start the assistant', text: 'It starts receiving Teams messages.', start: true },
    { done: !config.policy.dry_run, optional: true, title: 'Turn sending on', text: 'Review the proposals in Messages and turn observation mode off.', go: ['settings', 'panel-behavior'] },
  ];
  const required = steps.filter(step => !step.optional);
  const completed = required.filter(step => step.done).length;
  $('#setup-progress').textContent = `${completed} of ${required.length} ready`;
  $('#setup-bar').style.width = `${Math.round(completed / required.length * 100)}%`;
  const ready = completed === required.length;
  $('#setup-ready').hidden = !ready;
  $('#setup-steps').replaceChildren(...steps.map((step, index) => ({ step, index })).filter(({ step }) => !ready || !step.done).map(({ step, index }) => {
    const mark = el('span', { class: 'step-mark' }, step.done ? icon('check') : String(index + 1));
    const title = el('strong', {}, step.title, step.optional ? el('span', { class: 'optional', textContent: ' · optional' }) : null);
    let action = null;
    if (!step.done && step.go) {
      action = el('button', { class: 'secondary small', textContent: 'Set up' });
      action.onclick = () => showTab(...step.go);
    } else if (!step.done && step.start) {
      action = el('button', { class: 'small', textContent: 'Start' });
      action.onclick = () => startAssistant(action);
    }
    return el('li', { class: `step${step.done ? ' done' : ''}` }, mark, el('div', {}, title, el('p', { textContent: step.text })), action);
  }));
}

function renderLive() {
  const s = live;
  if (!s) return;
  const state = assistantState(s);
  $('#status').className = `pill ${state.tone}`;
  $('#status').title = state.detail;
  $('#status span').textContent = state.pill;
  $('#hero-dot').className = `dot ${state.tone}`;
  $('#hero-title').textContent = state.title;
  $('#hero-detail').textContent = state.detail;
  $('#version').textContent = `v${s.host_version}${s.headless ? ' · headless' : ''}`;
  $('#restart-offer').hidden = !s.restart_offer;
  $('#start').hidden = s.running;
  $('#restart').hidden = $('#stop').hidden = !s.running;
  $('#microsoft-state').className = `badge ${s.microsoft_connected ? 'ok' : 'warn'}`;
  $('#microsoft-state').textContent = s.microsoft_connected ? 'Account connected' : 'Account not connected';
  $('#teams-login').textContent = s.microsoft_connected ? 'Reconnect the account' : 'Connect Microsoft account';
  $('#github-state').className = `badge ${s.github_connected ? 'ok' : 'muted'}`;
  $('#github-state').textContent = s.github_connected ? 'Connected' : 'Not connected';
  $('#github-list').disabled = !s.github_connected;
  $('#github-disconnect').disabled = !s.github_connected;
  $('#self-chat-disable').disabled = !s.config.graph.self_chat;
  renderTiles(s);
  renderSteps(s);
}

/** Refreshes persisted state without touching unsaved form edits. */
async function refreshLive() {
  live = await invoke('snapshot');
  if (current) { current.credentials = live.credentials; current.github_connected = live.github_connected; }
  renderLive();
  renderCredentials();
  renderChatSources();
}

async function reload() {
  current = await invoke('snapshot');
  live = structuredClone(current);
  const config = current.config;
  field('#tenant-id', config.graph.tenant_id); field('#client-id', config.graph.client_id);
  field('#user-id', config.graph.user_id); field('#public-url', config.server.public_url);
  field('#bind', config.server.bind); field('#llm-style', config.llm.style);
  field('#llm-language', config.llm.language ?? 'es');
  field('#tunnel-config', current.tunnel_config);
  $('#cloudflare-tunnel').checked = config.server.cloudflare_tunnel;
  field('#allowed-chats', config.graph.allowed_chats.join('\n'));
  $('#dry-run').checked = config.policy.dry_run;
  $('#discover-chats').checked = config.graph.discover_all_chats;
  dirty = false;
  renderSavebar();
  renderLive();
  renderCredentials(); renderRepositories(); renderResources(); renderChatSources();
  await Promise.all([loadChain(), loadSelfChat()]);
}

setInterval(async () => {
  if (!current) return;
  try { live = await invoke('snapshot'); renderLive(); } catch { /* the next tick retries */ }
  if (tab === 'overview') loadActivity();
}, 15_000);

// ---------------------------------------------------------------- settings

function rowText(primary, secondary, extra) {
  return el('div', {}, el('strong', {}, primary, extra), el('small', { textContent: secondary }));
}

function renderCredentials() {
  const container = $('#credentials');
  container.replaceChildren();
  const origins = { system: 'System store', environment: 'Environment variable', file: 'Private file' };
  for (const [name, source] of Object.entries(live.credentials)) {
    const generated = source === 'missing' && GENERATED.includes(name);
    const retired = source === 'retired';
    const empty = source === 'missing' || source === 'unused';
    const state = generated ? badge('Generated on start', 'info')
      : source === 'missing' ? badge('Missing', 'warn')
      : source === 'unused' ? badge('Unused', 'muted')
      : retired ? badge('No longer used', 'muted')
      : badge('Set', 'ok');
    const detail = generated ? 'The assistant creates it on its first start.'
      : source === 'unused' ? 'The current model chain does not use it.'
      : retired ? 'An earlier version needed it; nothing reads it now. You can remove it.'
      : empty ? 'Needed to start the assistant.'
      : origins[source] || 'Set';
    const title = el('span', { class: 'cred-name', textContent: name });
    const input = el('input', { type: 'password', autocomplete: 'new-password', spellcheck: false, hidden: retired,
      placeholder: empty ? 'Enter the credential' : 'Type only to replace it', 'aria-label': `New value of ${name}` });
    const save = el('button', { textContent: empty ? 'Save' : 'Replace', disabled: true, hidden: retired });
    input.oninput = () => { save.disabled = !input.value.trim(); };
    input.onkeydown = event => { if (event.key === 'Enter' && !save.disabled) save.click(); };
    save.onclick = () => busy(save, async () => {
      await invoke('set_credential', { name, value: input.value });
      input.value = '';
      await refreshLive();
      done(`${name} stored in the system.`);
    });
    const remove = el('button', { class: 'danger', textContent: 'Remove', disabled: empty, 'aria-label': `Remove ${name}` });
    remove.onclick = () => busy(remove, async () => {
      await invoke('delete_credential', { name });
      await refreshLive();
      done(`${name} removed.`);
    });
    container.append(el('div', { class: 'row' }, rowText(title, detail, state), input, save, remove));
  }
}

const effortNames = { none: 'No reasoning', minimal: 'Minimal', low: 'Low', medium: 'Medium', high: 'High', xhigh: 'Very high', max: 'Maximum' };
let llmCatalog = { providers: [] };
let llmRows = [];

/** Configured order first (legacy profiles: their single DeepSeek model), then the rest inactive. */
function chainRows(config) {
  const rows = config.llm.chain.length
    ? config.llm.chain.map(choice => ({ ...choice }))
    : [{ provider: config.llm.provider, model: config.llm.model, effort: 'max', enabled: true }];
  for (const info of llmCatalog.providers) {
    if (rows.some(row => row.provider === info.id)) continue;
    const model = info.models[0];
    rows.push({ provider: info.id, model: model?.id ?? '', effort: model?.default_effort ?? info.efforts[0], enabled: false });
  }
  return rows;
}

function providerStatus(info) {
  if (!info) return 'Provider not available in this version';
  if (info.transport === 'cli') return info.available ? `CLI installed${info.version ? ' · ' + info.version : ''}` : 'CLI not installed on this computer';
  const source = live.credentials[info.credential];
  return source && source !== 'missing' && source !== 'unused' ? `API · ${info.credential} set` : `API · ${info.credential} missing in Credentials`;
}

function select(caption, label, options, value) {
  const element = el('select', { 'aria-label': label });
  for (const [optionValue, text] of options) element.append(new Option(text, optionValue));
  element.value = value;
  return [el('label', { class: 'field' }, el('small', { textContent: caption }), element), element];
}

function renderChain() {
  const list = $('#llm-chain');
  list.replaceChildren();
  let active = 0;
  llmRows.forEach((row, index) => {
    const info = llmCatalog.providers.find(provider => provider.id === row.provider);
    const name = providerNames[row.provider] || row.provider;
    const rank = el('span', { class: 'rank', textContent: !row.enabled ? 'Off' : active++ === 0 ? 'Default' : `Fallback ${active - 1}` });
    const toggle = el('input', { type: 'checkbox', checked: row.enabled, disabled: !row.enabled && !(info?.available), 'aria-label': `Turn on ${name}` });
    toggle.onchange = () => { row.enabled = toggle.checked; markDirty(); renderChain(); };
    const label = el('label', { class: 'check' }, toggle,
      el('div', {}, el('strong', { textContent: name }), el('small', { textContent: providerStatus(info) })));
    const models = (info?.models ?? []).map(model => [model.id, model.name]);
    if (row.model && !models.some(([id]) => id === row.model)) models.unshift([row.model, `${row.model} (current)`]);
    const [modelField, model] = select('Model', `${name} model`, models, row.model);
    model.title = row.model;
    const efforts = () => info?.models.find(m => m.id === row.model)?.efforts ?? info?.efforts ?? [row.effort];
    const effortOptions = () => {
      const values = efforts();
      return (values.includes(row.effort) ? values : [row.effort, ...values]).map(value => [value, effortNames[value] || value]);
    };
    const [effortField, effort] = select('Effort', `${name} effort`, effortOptions(), row.effort);
    model.onchange = () => {
      row.model = model.value;
      const supported = efforts();
      if (!supported.includes(row.effort)) row.effort = info?.models.find(m => m.id === row.model)?.default_effort ?? supported[0];
      markDirty();
      renderChain();
    };
    effort.onchange = () => { row.effort = effort.value; markDirty(); };
    const order = el('div', { class: 'order' });
    for (const [text, offset, aria] of [['↑', -1, 'Raise the priority of'], ['↓', 1, 'Lower the priority of']]) {
      const button = el('button', { class: 'secondary', textContent: text, 'aria-label': `${aria} ${name}`, disabled: !llmRows[index + offset] });
      button.onclick = () => {
        [llmRows[index], llmRows[index + offset]] = [llmRows[index + offset], llmRows[index]];
        markDirty();
        renderChain();
      };
      order.append(button);
    }
    list.append(el('li', { class: 'chain-row' + (row.enabled ? '' : ' off') }, rank, label, modelField, effortField, order));
  });
}

async function loadChain() {
  // Without detection the configured order still loads, so saving never drops it.
  try { llmCatalog = await invoke('llm_providers'); }
  catch (error) { fail(error); }
  llmRows = chainRows(current.config);
  renderChain();
}

async function loadSelfChat() {
  const config = current.config;
  $('#self-chat-badge').className = `badge ${config.graph.self_chat ? 'ok' : 'muted'}`;
  $('#self-chat-badge').textContent = config.graph.self_chat ? 'Enabled' : 'Disabled';
  if (!config.graph.self_chat) {
    $('#self-chat-state').textContent = 'Write to yourself in Teams (your notes or a chat with yourself) and the assistant answers right there, with the sources authorized for that chat.';
    return;
  }
  let diagnostic;
  try { diagnostic = (await invoke('self_chat_status')).diagnostics; } catch { /* shown as pending */ }
  $('#self-chat-state').textContent = `Chat ${config.graph.self_chat.id} · received by webhook and polled every 10 s · last poll: ${diagnostic?.cursor ? new Date(diagnostic.cursor[1] * 1000).toLocaleString() : 'pending'} · unresolved outputs: ${diagnostic?.unresolved_outputs ?? 0}`;
}

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
  config.server.public_url = $('#public-url').value.trim();
  config.server.bind = $('#bind').value.trim();
  config.server.cloudflare_tunnel = $('#cloudflare-tunnel').checked;
  config.policy.dry_run = $('#dry-run').checked;
}

async function save() {
  if (!llmRows.some(row => row.enabled)) throw 'Turn on at least one language model.';
  collectSettings();
  await invoke('save_settings', { config: current.config, map: current.map, tunnelConfig: $('#tunnel-config').value.trim() });
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
    done('Assistant started.');
  });
}

// ---------------------------------------------------------------- messages

const statusLabels = {
  sent: ['Answered', 'ok'], dry_run: ['Observed (not sent)', 'info'], sending: ['Sending', 'active'],
  processing: ['In progress', 'active'], pending: ['Queued', 'active'], failed: ['Error', 'error'],
  uncertain: ['Uncertain send', 'error'],
};
const reasonLabels = {
  ineligible_message: 'Not addressed to the assistant: your own, from a group without a mention, too old, empty or an answer of the app.',
  sensitive_question: 'The question contains sensitive data; it is left to you.',
  no_authorized_resource: 'No source is authorized for this conversation.',
  informational_message: 'Informational message: it asks for no answer.',
  personal_request: 'It asks you for a call, a meeting, a joint review or your availability: it is left to you.',
  deterministic_greeting: 'Greeting answered with the configured text.',
  supported_answer: 'Answer generated.',
  unsafe_proposal: 'Withheld: it contained sensitive data or did not fit in a Teams message.',
  unsafe_answer: 'Withheld: it contained sensitive data or did not fit in a Teams message.',
  reference_selection_failed: 'Withheld while choosing references (earlier version).',
  final_gate: 'Withheld by the final review (earlier version).',
  message_changed: 'The message changed, was deleted or stopped being eligible before sending.',
  send_result_unknown_manual_review: 'It is unknown whether the send arrived: check it in Teams (never retried).',
  read_or_provider_error: 'Error reading Teams or calling a provider; it is retried.',
  generating: 'Writing the answer.', retrieving: 'Searching the sources.', tool_started: 'Reading a tool.',
  selecting_references: 'Choosing references.', final_review: 'Final review.',
  holding_reply: 'Sending the holding notice.',
  linking: 'Linking work to work items.',
  links_planned: 'Link plan shown; waiting for your confirmation.',
  links_applied: 'Confirmed links written to Azure DevOps (never retried).',
  links_cancelled: 'Link plan cancelled; nothing was linked.',
  links_unresolved: 'The link request could not be resolved; nothing was planned.',
  links_not_configured: 'Linking asked for, but no source has a write credential.',
};
const kindLabels = { self: 'Personal chat', direct: 'Direct', group: 'Group', unsupported: 'Other' };
const intentLabels = { question: 'question', activity_review: 'activity review', greeting: 'greeting', link: 'link work to work items' };
const linkLabels = { sending: 'sending', linked: 'linked', already_linked: 'already linked', failed: 'failed', uncertain: 'uncertain (check the work item; never retried)' };
let messageRows = [];
let messageFilter = 'all';
const openMessages = new Set();

function messageState(row) {
  const reason = row.audit.reason || '';
  if (row.status === 'ignored') {
    if (reason === 'ineligible_message') return ['Not addressed', 'muted'];
    if (/^(unsafe_|invalid_references|reference_selection_failed|final_gate)/.test(reason)) return ['Withheld', 'warn'];
    return ['No answer', 'muted'];
  }
  return statusLabels[row.status] || [row.status, 'muted'];
}

function reasonText(reason) {
  if (!reason) return '';
  if (reason.startsWith('invalid_references')) return `Withheld: references or links that could not be verified (${reason.replace('invalid_references: ', '')}).`;
  return reasonLabels[reason] || reason;
}

const millis = row => row.audit.received_at ?? row.created_at * 1000;
const relative = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });
function ago(at) {
  const seconds = Math.round((at - Date.now()) / 1000);
  for (const [unit, size] of [['day', 86400], ['hour', 3600], ['minute', 60]]) {
    if (Math.abs(seconds) >= size) return relative.format(Math.round(seconds / size), unit);
  }
  return 'just now';
}
const when = row => new Date(millis(row)).toLocaleString();
const ineligible = row => row.audit.reason === 'ineligible_message' && row.status === 'ignored';

function excerpt(row) {
  const audit = row.audit;
  const reply = audit.sent || audit.proposed;
  return audit.question
    || (reply ? `Answer: ${reply.replace(/\s+/g, ' ').slice(0, 140)}` : null)
    || (audit.reason === 'ineligible_message' ? '(the text of messages not addressed to the assistant is not stored)' : '(no text recorded)');
}

function kindOf(row) {
  return kindLabels[row.audit.conversation_kind] || (row.resource.includes('48:notes') ? 'Personal chat' : row.resource.startsWith('simulation:') ? 'Simulation' : 'Teams');
}

function messageField(container, label, text, pre = false) {
  if (!text) return;
  const body = pre === 'markdown' ? markdown(text) : el(pre ? 'pre' : 'p', { textContent: text });
  container.append(el('div', { class: 'field-block' }, el('strong', { textContent: label }), body));
}

function matchesFilter(row, filter) {
  const [, tone] = messageState(row);
  return filter === 'all' || (filter === 'sent' && tone === 'ok') || (filter === 'withheld' && (tone === 'warn' || (tone === 'muted' && row.audit.reason !== 'ineligible_message')))
    || (filter === 'error' && tone === 'error') || (filter === 'active' && tone === 'active');
}

function renderMessages() {
  const list = $('#messages-list');
  const hideIneligible = $('#messages-hide-ineligible').checked;
  const rows = messageRows.filter(row => !(hideIneligible && ineligible(row)) && matchesFilter(row, messageFilter));
  const counts = messageRows.reduce((acc, row) => { const [label] = messageState(row); acc[label] = (acc[label] || 0) + 1; return acc; }, {});
  $('#messages-summary').textContent = `${messageRows.length} recent messages · ` + Object.entries(counts).map(([label, n]) => `${label}: ${n}`).join(' · ');
  list.replaceChildren();
  if (!rows.length) {
    list.append(el('div', { class: 'empty' }, el('p', { textContent: messageRows.length ? 'No messages for this filter.' : 'No messages yet. They show up here when the assistant receives one from Teams.' })));
    return;
  }
  for (const row of rows) {
    const audit = row.audit;
    const [label, tone] = messageState(row);
    const item = el('details', { class: 'message', open: openMessages.has(row.resource) });
    item.ontoggle = () => { if (item.open) openMessages.add(row.resource); else openMessages.delete(row.resource); };
    item.append(el('summary', {}, badge(label, tone), el('span', { class: 'kind', textContent: kindOf(row) }),
      el('time', { textContent: when(row) }), el('span', { class: 'excerpt', textContent: excerpt(row) })));
    const body = el('div', { class: 'message-body' });
    messageField(body, 'Message', audit.question, true);
    if (audit.resolved_question) messageField(body, 'Interpreted with the context', `${audit.resolved_question}${audit.topic ? ` · topic searched: ${audit.topic}` : ''}`);
    messageField(body, 'Outcome', reasonText(audit.reason));
    if (row.status === 'sent') messageField(body, 'Answer sent', audit.sent || audit.proposed, 'markdown');
    else messageField(body, row.status === 'dry_run' ? 'Proposal (observation mode)' : 'Proposed answer (not sent)', audit.proposed, 'markdown');
    const details = [];
    if (audit.intent) details.push(`Intent: ${intentLabels[audit.intent] || audit.intent}`);
    if (audit.history_messages) details.push(`Context: ${audit.history_messages} earlier messages`);
    if (audit.provider) details.push(`Model: ${audit.provider}`);
    if (audit.provider_fallbacks?.length) details.push(`Failed first: ${audit.provider_fallbacks.join(', ')}`);
    if (audit.source) details.push(`Sources: ${audit.source}`);
    if (audit.references?.length) details.push(`References: ${audit.used_sources?.length ?? 0} cited of ${audit.references.length}${audit.reference_selection === 'code' ? ' (chosen by code; no model chose them)' : ''}`);
    if (audit.final_check) details.push(`Final review (earlier version): ${audit.final_check === 'unavailable' ? 'unavailable' : audit.final_check}`);
    const notice = { sending: 'sending', sent: 'sent', uncertain: 'uncertain (never retried)' };
    if (audit.holding_reply) details.push(`Holding notice: ${notice[audit.holding_reply] || audit.holding_reply}`);
    if (audit.withheld_notice) details.push(`Withheld-answer notice: ${notice[audit.withheld_notice] || audit.withheld_notice}`);
    for (const link of audit.links || []) details.push(`Link: ${link.activity} → #${link.work_item} ${link.title}: ${linkLabels[link.status] || link.status}`);
    messageField(body, 'Details', details.join('\n'), true);
    if (audit.trace?.length) {
      const log = el('ol', { class: 'trace' });
      for (const step of audit.trace) {
        log.append(el('li', {}, el('time', { textContent: new Date(step.at).toLocaleTimeString() }), el('b', { textContent: step.step }),
          step.detail ? ` — ${step.detail}` : ''));
      }
      body.append(el('div', { class: 'field-block' }, el('strong', { textContent: 'Log' }), log));
    } else {
      messageField(body, 'Log', 'This message was processed by an earlier version, without a step log.');
    }
    body.append(el('small', { class: 'resource-id', textContent: row.resource }));
    item.append(body);
    list.append(item);
  }
}

function renderAttention() {
  const since = Date.now() - 24 * 3600_000;
  const errors = messageRows.filter(row => messageState(row)[1] === 'error' && millis(row) >= since).length;
  const counter = $('#nav-attention');
  counter.hidden = !errors;
  counter.textContent = errors;
  counter.title = `${errors} message(s) with an error or an uncertain send in the last 24 h`;
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
  const stats = [['Answered', tally('sent'), 'ok', 'sent'], ['Withheld or unanswered', tally('withheld'), 'warn', 'withheld'],
    ['Errors', tally('error'), 'error', 'error'], ['In progress', tally('active'), 'active', 'active']];
  $('#activity-stats').replaceChildren(...stats.filter(([, n]) => n).map(([label, n, tone, filter]) => {
    const chip = el('button', { class: `badge ${tone}`, textContent: `${label}: ${n}`, title: 'See in Messages' });
    chip.onclick = () => { setFilter(filter); showTab('messages'); };
    return chip;
  }));
  const recent = relevant.slice(0, 6);
  $('#activity-list').replaceChildren(...(recent.length ? recent.map(row => {
    const [label, tone] = messageState(row);
    return el('div', { class: 'activity-row' }, badge(label, tone), el('time', { textContent: ago(millis(row)), title: when(row) }),
      el('span', { class: 'excerpt', textContent: excerpt(row), title: excerpt(row) }));
  }) : [el('div', { class: 'empty' }, el('p', { textContent: 'No messages addressed to the assistant yet.' }))]));
}

function setFilter(filter) {
  messageFilter = filter;
  for (const button of document.querySelectorAll('#messages-filters button')) {
    button.setAttribute('aria-pressed', String(button.dataset.filter === filter));
  }
  renderMessages();
}

// ---------------------------------------------------------------- knowledge

const toolNames = {
  azure_devops_wiki: 'Azure DevOps Wiki', azure_devops_status: 'Azure DevOps activity', azure_devops: 'Azure DevOps work items',
  sql_server: 'SQL Server', rabbitmq: 'RabbitMQ queue', http: 'HTTP GET', teams_messages: 'My Teams messages',
};

function kindName(resource) {
  if (resource.kind === 'file') return 'File';
  if (resource.kind === 'url') return 'URL';
  return toolNames[resource.tool?.type] || resource.tool?.type || 'Tool';
}

function accessText(resource) {
  if (resource.kind === 'file') return `${resource.repository} / ${resource.path}`;
  if (resource.kind === 'url') return resource.url;
  const tool = resource.tool ?? {};
  if (tool.organization) return `${tool.organization} / ${tool.project}`;
  if (tool.repository) return `Catalog: ${tool.repository} / ${tool.path}`;
  if (tool.url) return tool.url;
  if (tool.operation) return `Fixed query: ${tool.operation}`;
  if (tool.type === 'teams_messages') return 'Your own messages, read with the connected Microsoft account';
  return 'Read-only tool';
}

function sourceState(resource) {
  if (!resource.enabled) return badge('Disabled', 'muted');
  if (!resource.external_processing) return badge('No external processing', 'warn');
  if (!resource.allowed_conversations.length) return badge('No Teams audience', 'warn');
  return badge('Ready', 'ok');
}

function renderResources() {
  const container = $('#resources');
  container.replaceChildren();
  const resources = current.map.resources;
  $('#sources-summary').textContent = resources.length ? `${readySources(current.map).length} of ${resources.length} ready for Teams` : '';
  if (!resources.length) {
    container.append(el('div', { class: 'empty' }, el('p', { textContent: 'No sources yet. Add a file from a repository or register the Wiki with pta sources add.' })));
  }
  for (const resource of resources) {
    const input = (label, value, placeholder, onchange) => {
      const element = el('input', { value, placeholder, spellcheck: false });
      element.oninput = () => { onchange(element.value); markDirty(); };
      return el('label', {}, label, element);
    };
    const toggles = el('div', { class: 'actions' });
    for (const [key, text] of [['enabled', 'Enabled'], ['external_processing', 'Authorize external processing']]) {
      const box = el('input', { type: 'checkbox', checked: resource[key] });
      box.onchange = () => { resource[key] = box.checked; markDirty(); renderResources(); };
      toggles.append(el('label', { class: 'check' }, box, text));
    }
    const remove = el('button', { class: 'danger small', textContent: 'Remove source' });
    remove.onclick = () => { current.map.resources = current.map.resources.filter(r => r !== resource); markDirty(); renderResources(); };
    toggles.append(remove);
    let state = sourceState(resource);
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { textContent: resource.id }), badge(kindName(resource), 'active'), el('span', { class: 'grow' }), state),
      el('div', { class: 'access', textContent: accessText(resource) }),
      el('div', { class: 'grid' },
        input('Description', resource.description, 'Which questions it answers', value => { resource.description = value; }),
        input('Topics', resource.topics.join(', '), 'Comma separated', value => { resource.topics = names(value); }),
        input('Authorized conversations', resource.allowed_conversations.join(', '), 'Comma-separated chat IDs, or *', value => {
          resource.allowed_conversations = names(value);
          const next = sourceState(resource);
          state.replaceWith(next);
          state = next;
        })),
      toggles));
  }
}

function renderRepositories() {
  const container = $('#repositories');
  container.replaceChildren();
  const choices = $('#source-repo');
  choices.replaceChildren();
  const entries = Object.entries(current.map.repositories);
  if (!entries.length) container.append(el('div', { class: 'empty' }, el('p', { textContent: 'No repositories. Add a local checkout or clone one from GitHub.' })));
  for (const [alias, path] of entries) {
    choices.append(new Option(alias, alias));
    const input = el('input', { value: path, spellcheck: false, 'aria-label': `Path of ${alias}` });
    input.oninput = () => { current.map.repositories[alias] = input.value.trim(); markDirty(); };
    const update = el('button', { class: 'secondary', textContent: 'Update' });
    update.onclick = () => busy(update, async () => { await invoke('update_github_repository', { alias }); done(`${alias} updated.`); });
    const remove = el('button', { class: 'danger', textContent: 'Remove' });
    remove.onclick = () => {
      const tools = current.map.resources.filter(r => r.kind === 'tool' && r.tool?.repository === alias);
      if (tools.length) return fail(`${alias} cannot be removed: ${tools.map(r => r.id).join(', ')} use it. Remove those sources first.`);
      const files = current.map.resources.filter(r => r.repository === alias);
      delete current.map.repositories[alias];
      current.map.resources = current.map.resources.filter(r => r.repository !== alias);
      markDirty();
      renderRepositories(); renderResources();
      if (files.length) notify(`Its sources were removed too: ${files.map(r => r.id).join(', ')}.`);
    };
    container.append(el('div', { class: 'row' }, rowText(alias, 'Git checkout'), input, update, remove));
  }
}

async function listGithub() {
  const repos = await invoke('github_repositories');
  const container = $('#github-repositories');
  container.replaceChildren();
  if (!repos.length) { container.append(el('div', { class: 'empty' }, el('p', { textContent: 'No repositories are installed for this GitHub App.' }))); return; }
  for (const repo of repos) {
    const alias = el('input', { spellcheck: false, 'aria-label': `Local alias of ${repo.full_name}`,
      value: repo.full_name.split('/').pop().toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/^[^a-z]+/, 'repo-').slice(0, 64) });
    const clone = el('button', { class: 'secondary', textContent: 'Clone and add' });
    clone.onclick = () => busy(clone, () => applying(async () => {
      await invoke('clone_github_repository', { fullName: repo.full_name, alias: alias.value.trim() });
      done(`${repo.full_name} added.`);
    }));
    container.append(el('div', { class: 'row' }, rowText(repo.full_name, repo.private ? 'Private' : 'Public'), alias, clone));
  }
}

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
    container.append(el('p', { class: 'hint', textContent: 'No sources are ready. In Knowledge, enable one, authorize its external processing and save.' }));
    return;
  }
  for (const resource of usable) {
    const box = el('input', { type: 'checkbox', value: resource.id, checked: chosen.has(resource.id) });
    box.onchange = () => { if (box.checked) chosen.add(resource.id); else chosen.delete(resource.id); };
    container.append(el('label', { class: 'check' }, box,
      el('span', {}, el('strong', { textContent: resource.id }), el('small', { textContent: `${kindName(resource)} · ${resource.description}` }))));
  }
}

async function copy(text) {
  try { await navigator.clipboard.writeText(text); return true; } catch { /* fall back below */ }
  const area = el('textarea', { value: text, class: 'sr-only', readOnly: true });
  document.body.append(area);
  area.select();
  let copied = false;
  try { copied = document.execCommand('copy'); } catch { copied = false; }
  area.remove();
  return copied;
}

/** Links never navigate the app window: they copy the URL for the browser. */
function link(label, url) {
  const button = el('button', { type: 'button', class: 'ref', textContent: label, title: `Copy link: ${url}` });
  button.onclick = async () => {
    if (await copy(url)) done(`Link copied: ${url}`);
    else notify(`Link: ${url}`);
  };
  return button;
}

function inline(text, parent) {
  const pattern = /`([^`]+)`|\*\*(.+?)\*\*|\[([^\]]+)\]\((https:\/\/[^\s)]+)\)/g;
  let last = 0;
  for (const match of text.matchAll(pattern)) {
    if (match.index > last) parent.append(text.slice(last, match.index));
    if (match[1] !== undefined) parent.append(el('code', { textContent: match[1] }));
    else if (match[2] !== undefined) { const strong = el('strong'); inline(match[2], strong); parent.append(strong); }
    else parent.append(link(match[3], match[4]));
    last = match.index + match[0].length;
  }
  if (last < text.length) parent.append(text.slice(last));
}

const LIST_ITEM = /^(\s*)([-*]|\d+[.)])\s+(.*)$/;

/** Renders the bounded Markdown the assistant writes (the same subset Teams receives as HTML), without innerHTML. */
function markdown(text) {
  const root = el('div', { class: 'md' });
  const source = text.replace(/\r/g, '').split('\n');
  let i = 0;
  while (i < source.length) {
    const line = source[i];
    if (/^\s*```/.test(line)) {
      const code = [];
      i++;
      while (i < source.length && !/^\s*```\s*$/.test(source[i])) code.push(source[i++]);
      i++;
      root.append(el('pre', {}, el('code', { textContent: code.join('\n') })));
      continue;
    }
    if (!line.trim()) { i++; continue; }
    if (LIST_ITEM.test(line)) {
      const items = [];
      while (i < source.length && source[i].trim() && !/^\s*```/.test(source[i])) {
        const match = source[i].match(LIST_ITEM);
        if (match) items.push({ indent: match[1].length, ordered: /\d/.test(match[2]), start: parseInt(match[2], 10), text: match[3] });
        else if (items.length) items[items.length - 1].text += ` ${source[i].trim()}`;
        i++;
      }
      const stack = [{ indent: -1, list: null, item: null }];
      for (const item of items) {
        while (stack.length > 1 && item.indent < stack[stack.length - 1].indent) stack.pop();
        let top = stack[stack.length - 1];
        if (item.indent > top.indent) {
          const list = el(item.ordered ? 'ol' : 'ul');
          if (item.ordered && item.start > 1) list.start = item.start;
          (top.item ?? root).append(list);
          top = { indent: item.indent, list, item: null };
          stack.push(top);
        }
        const entry = el('li');
        inline(item.text, entry);
        top.list.append(entry);
        top.item = entry;
      }
      continue;
    }
    const paragraph = el('p');
    let first = true;
    while (i < source.length && source[i].trim() && !LIST_ITEM.test(source[i]) && !/^\s*```/.test(source[i])) {
      if (!first) paragraph.append(el('br'));
      inline(source[i].trim(), paragraph);
      first = false;
      i++;
    }
    root.append(paragraph);
  }
  return root;
}

function bubble(kind, ...content) {
  $('#chat-empty').hidden = true;
  const node = el('div', { class: `bubble ${kind}` }, ...content);
  $('#history').append(node);
  node.scrollIntoView({ block: 'end', behavior: 'smooth' });
  return node;
}

function answerBubble(result) {
  if (!result.answer) return bubble('bot none', `No answer: ${reasonText(result.reason) || result.status}`);
  const meta = el('div', { class: 'meta' });
  for (const id of result.used_sources ?? []) meta.append(badge(id, 'active'));
  if (result.partial) meta.append(badge('Partial coverage', 'warn'));
  for (const warning of result.warnings ?? []) meta.append(badge(warning, 'warn'));
  let refs = null;
  if (result.references?.length) {
    refs = el('div', { class: 'refs' }, el('strong', { textContent: 'Verified references' }),
      el('ul', {}, ...result.references.map(ref => el('li', {}, link(ref.label || ref.id, ref.url)))));
  }
  return bubble('bot', markdown(result.answer), meta.childNodes.length ? meta : null, refs);
}

function resizeComposer() {
  const area = $('#message');
  area.style.height = 'auto';
  area.style.height = `${Math.min(area.scrollHeight + 2, 192)}px`;
}

// ---------------------------------------------------------------- wiring

document.querySelectorAll('nav button').forEach(button => { button.onclick = () => showTab(button.dataset.tab); });
document.querySelectorAll('.cfg').forEach(input => {
  input.addEventListener('input', markDirty);
  input.addEventListener('change', markDirty);
});
$('#notice-close').onclick = () => { $('#notice').hidden = true; };
$('#save').onclick = () => busy($('#save'), async () => { await save(); done('Settings saved.'); });
$('#discard').onclick = () => busy($('#discard'), async () => { await reload(); notify('Changes discarded.'); });
$('#start').onclick = () => startAssistant($('#start'));
$('#restart').onclick = () => busy($('#restart'), async () => {
  if (dirty) await save();
  await invoke('restart');
  await refreshLive();
  done('Assistant restarted with the saved settings.');
});
$('#stop').onclick = () => busy($('#stop'), async () => { await invoke('stop_assistant'); await refreshLive(); done('Assistant stopped.'); });
$('#offer-start').onclick = () => busy($('#offer-start'), async () => {
  await invoke('start_assistant'); await refreshLive(); done('The assistant keeps running with the new version.');
});
$('#offer-dismiss').onclick = () => busy($('#offer-dismiss'), async () => {
  await invoke('dismiss_restart_offer'); await refreshLive(); notify('The assistant stays stopped. You can start it whenever you want.');
});
$('#activity-all').onclick = () => showTab('messages');
$('#messages-refresh').onclick = () => busy($('#messages-refresh'), loadMessages);
document.querySelectorAll('#messages-filters button').forEach(button => { button.onclick = () => setFilter(button.dataset.filter); });
$('#messages-hide-ineligible').onchange = renderMessages;
setInterval(() => {
  if (tab === 'messages' && $('#messages-auto').checked) loadMessages().catch(() => {});
}, 10_000);
$('#detect-llm').onclick = () => busy($('#detect-llm'), async () => {
  llmCatalog = await invoke('llm_providers'); renderChain(); done('CLIs detected again.');
});
$('#teams-login').onclick = () => busy($('#teams-login'), () => applying(async () => {
  await invoke('connect_microsoft'); done('Microsoft account connected.');
}));
$('#import').onclick = () => busy($('#import'), async () => {
  await invoke('import_existing', { path: $('#legacy-path').value.trim() }); await reload(); done('Settings imported.');
});
$('#self-chat-enable').onclick = () => busy($('#self-chat-enable'), () => applying(async () => {
  await invoke('self_chat_enable', { id: $('#self-chat-id').value.trim() || null });
  done('Personal chat validated and enabled. Source audiences are kept.');
}));
$('#self-chat-disable').onclick = () => busy($('#self-chat-disable'), () => applying(async () => {
  await invoke('self_chat_disable'); done('Personal chat disabled.');
}));
$('#self-chat-test').onclick = () => busy($('#self-chat-test'), async () => {
  const result = await invoke('test_self_chat');
  if (result.account_scope_verified) done('Personal chat validated. Write a new question in Teams to check reception and the answer.');
  else notify('Incomplete test.');
});
$('#add-repo').onclick = () => {
  const alias = $('#repo-alias').value.trim();
  const path = $('#repo-path').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(alias) || !path) return fail('Give a valid alias (lowercase letters, digits, - or _) and the checkout path.');
  if (current.map.repositories[alias]) return fail('That alias already exists.');
  current.map.repositories[alias] = path;
  field('#repo-alias', ''); field('#repo-path', '');
  markDirty();
  renderRepositories();
};
$('#github-connect').onclick = () => busy($('#github-connect'), async () => {
  const prompt = await invoke('begin_github_login', { clientId: $('#github-client-id').value.trim() });
  $('#github-code').textContent = prompt.user_code;
  $('#github-device').hidden = false;
  notify('Code ready. Authorize the GitHub App with the repositories you want.');
});
$('#github-open').onclick = () => invoke('open_github_login').catch(fail);
$('#github-finish').onclick = () => busy($('#github-finish'), async () => {
  await invoke('finish_github_login');
  $('#github-device').hidden = true;
  await refreshLive();
  await listGithub();
  done('GitHub connected.');
});
$('#github-list').onclick = () => busy($('#github-list'), listGithub);
$('#github-disconnect').onclick = () => busy($('#github-disconnect'), async () => {
  await invoke('disconnect_github');
  $('#github-repositories').replaceChildren();
  await refreshLive();
  done('GitHub disconnected. Local checkouts stay.');
});
$('#add-source').onclick = () => {
  const id = $('#source-id').value.trim();
  const repository = $('#source-repo').value;
  const path = $('#source-path').value.trim();
  const description = $('#source-description').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || !repository || !path || !description) return fail('Fill in the ID (lowercase letters, digits, - or _), repository, file and description.');
  if (current.map.resources.some(r => r.id === id)) return fail('That ID already exists.');
  current.map.resources.push({ id, description, topics: names($('#source-topics').value), enabled: false,
    external_processing: false, allowed_conversations: [], allowed_senders: [], kind: 'file', repository, path });
  for (const name of ['#source-id', '#source-path', '#source-topics', '#source-description']) field(name, '');
  markDirty();
  renderResources();
  notify(`Source ${id} added, disabled. Save the changes to keep it.`);
};
$('#chat-all').onclick = () => {
  const boxes = [...$('#chat-sources').querySelectorAll('input')];
  const all = boxes.every(box => box.checked);
  for (const box of boxes) { box.checked = !all; if (box.checked) chosen.add(box.value); else chosen.delete(box.value); }
};
$('#chat-reset').onclick = () => {
  session = crypto.randomUUID();
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
  if (!sources.length) return fail('Select at least one source for the test.');
  const send = $('#send');
  send.disabled = true;
  bubble('you', text);
  field('#message', '');
  resizeComposer();
  const elapsed = el('span', { textContent: 'Writing…' });
  const pending = bubble('bot', el('span', { class: 'typing' }, el('i'), el('i'), el('i'), elapsed));
  const started = Date.now();
  const timer = setInterval(() => { elapsed.textContent = `Writing… ${Math.round((Date.now() - started) / 1000)} s`; }, 1000);
  try {
    const result = await invoke('chat', { input: { session, text, group: false, mentioned: false, sources } });
    pending.remove();
    answerBubble(result);
  } catch (error) {
    pending.remove();
    bubble('bot none', `The test could not complete: ${error?.message ?? error}`);
  } finally {
    clearInterval(timer);
    send.disabled = false;
  }
};

reload().then(() => loadActivity()).catch(fail);
