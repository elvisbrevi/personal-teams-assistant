const rawInvoke = window.ptaTransport.invoke;
const READS = ['snapshot', 'chat', 'github_repositories', 'self_chat_status', 'llm_providers', 'audit', 'activity_status', 'activity_history', 'activity_pending'];
// Revision of the profile files the last reply saw. The form's own revision (`current.revision`)
// changes only when it reloads or writes, so saving a stale form fails instead of overwriting
// changes made elsewhere (e.g. with `pta`).
let lastRevision = null;
async function invoke(method, args = {}) {
  if (window.ptaTransport.browser && method === 'activity_open') {
    selectedActivity = args.id || pendingActivities[0]?.id || null;
    activityEditing = false; activityPayload = '';
    const entry = pendingActivities.find(e => e.id === selectedActivity);
    if (entry) renderActivityEditor(entry);
    showTab('registration', entry ? 'activity-editor' : null);
    return { visible: true, id: selectedActivity };
  }
  let result = await rawInvoke('command', { request: {
    method, args, revision: READS.includes(method) ? null : current?.revision ?? null, contract: 1
  }});
  if (window.ptaTransport.browser && result.code === 'authorization_pending' && method === 'connect_microsoft') {
    result = await browserMicrosoftLogin(result.data.authorization_url);
  }
  if (result.code === 'authorization_pending' && ['connect_microsoft', 'finish_github_login'].includes(method)) {
    const finish = method === 'connect_microsoft' ? 'finish_microsoft' : 'finish_github_login';
    const deadline = Date.now() + 600_000;
    notify(t('Authorization pending in the browser.'), 'info');
    while (result.code === 'authorization_pending' && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 2000));
      result = await rawInvoke('command', { request: { method: finish, args: {}, revision: null, contract: 1 } });
    }
  }
  if (result.revision) lastRevision = result.revision;
  if (result.revision && current && !READS.includes(method)) current.revision = result.revision;
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
  node.append(...children.flat().filter(child => child !== null && child !== undefined && child !== false));
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
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

let noticeTimer;
function notify(text, tone = 'info') {
  const box = $('#notice');
  $('#notice-text').textContent = tx(String(text?.message ?? text));
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
    if (viewedActivity) renderActivityEditor(viewedActivity);
    if (button?.id === 'activity-run') button.disabled = activityRunning;
  }
}

function showTab(name, anchor) {
  tab = name;
  for (const section of ['overview', 'settings', 'messages', 'knowledge', 'chat', 'registration']) $('#' + section).hidden = section !== name;
  for (const button of document.querySelectorAll('nav button')) {
    button.setAttribute('aria-current', button.dataset.tab === name ? 'page' : 'false');
  }
  if (name === 'messages') loadMessages().catch(fail);
  if (name === 'overview') loadActivity();
  if (name === 'registration') loadRegistration().catch(fail);
  renderSavebar();
  renderActivityNotice();
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
    return { tone: 'muted', pill: t('Assistant stopped'), title: t('Assistant stopped'),
      detail: t('It neither receives nor answers Teams messages. The test chat still works.') };
  }
  let tone = observing ? 'info' : 'ok';
  let teams;
  const expected = expectedSubscriptions(config);
  if (!expected) {
    teams = config.graph.self_chat ? t('Personal chat only') : t('No chats set up to receive');
    if (!config.graph.self_chat) tone = 'warn';
  } else if (snapshot.active_subscriptions >= expected) {
    teams = t('Teams: {n} active subscription(s)', { n: snapshot.active_subscriptions });
  } else {
    const code = snapshot.subscription_issue?.match(/graph_http_(\d{3})/)?.[1];
    teams = code ? t('Teams pending (Graph HTTP {code})', { code }) : t('Teams subscription pending');
    tone = 'warn';
  }
  return {
    tone,
    pill: tone === 'warn' ? t('Running · Teams pending') : observing ? t('Running · observing') : t('Running'),
    title: observing ? t('Running in observation mode') : t('Running and answering'),
    detail: `${observing ? t('Drafts proposals without sending them') : t('Answers in Teams as you')} · ${teams}`,
  };
}

/** Phase the host pushes while the assistant starts or stops (`starting`/`stopping`), else null. */
let transition = null;
const transitions = {
  starting: { tone: 'busy', pill: t('Starting…'), title: t('Starting the assistant…'), detail: t('It will receive Teams messages once it is ready.') },
  stopping: { tone: 'busy', pill: t('Stopping…'), title: t('Stopping the assistant…'), detail: t('It stops receiving Teams messages.') },
};

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
  let host = t('Public URL missing');
  try { if (urlReady(config)) host = new URL(config.server.public_url).host; } catch { /* shown as missing */ }
  $('#tiles').replaceChildren(
    tile(t('Microsoft account'), s.microsoft_connected ? t('Connected') : t('Not connected'), s.microsoft_connected ? 'ok' : 'warn',
      s.microsoft_connected ? t('Reads and answers as you') : t('Connect it in Settings')),
    tile(t('Mode'), config.policy.dry_run ? t('Observation') : t('Sending on'), config.policy.dry_run ? 'info' : 'ok',
      config.policy.dry_run ? t('Sends nothing to Teams') : t('Answers in Teams')),
    tile(t('Reception'), config.graph.discover_all_chats ? t('All my chats') : t('{n} chat(s)', { n: config.graph.allowed_chats.length }),
      s.running && s.active_subscriptions ? 'ok' : 'muted',
      s.running ? t('{n} active subscription(s)', { n: s.active_subscriptions }) : t('Inactive while stopped')),
    tile(t('Tunnel'), tunnelManaged ? (s.tunnel_running ? t('Running') : t('Stopped')) : t('External'), tunnelManaged && s.tunnel_running ? 'ok' : 'muted', host),
    tile(t('Models'), first ? `${providerNames[first.provider] || first.provider}` : t('None active'), first ? 'ok' : 'warn',
      first ? `${first.model}${rest.length ? ` · ${t('fallback')}: ${rest.map(c => providerNames[c.provider] || c.provider).join(' → ')}` : ` · ${t('no fallback')}`}` : t('Turn one on in Settings')),
    tile(t('Sources'), t('{ready} of {total} ready', { ready, total: s.map.resources.length }), ready ? 'ok' : 'warn', t('Enabled, authorized and with an audience')),
    tile(t('Personal chat'), config.graph.self_chat ? t('Enabled') : t('Disabled'), config.graph.self_chat ? 'ok' : 'muted',
      config.graph.self_chat ? t('Ask it from your Teams notes') : t('Optional')),
  );
}

function renderSteps(s) {
  const config = s.config;
  const missing = missingCredentials(s.credentials);
  const steps = [
    { done: !missing.length, title: t('Credentials'), text: missing.length ? t('Missing: {names}.', { names: missing.join(', ') }) : t('The required credentials are stored.'), go: ['settings', 'panel-credentials'] },
    { done: enabledChain(config).length > 0, title: t('Language model'), text: t('Turn on and order Codex, Claude Code or DeepSeek.'), go: ['settings', 'panel-llm'] },
    { done: entraReady(config), title: t('Entra registration'), text: t('Tenant and client ID of your existing registration.'), go: ['settings', 'panel-teams'] },
    { done: s.microsoft_connected, title: t('Microsoft account'), text: t('Sign in to read and answer as you.'), go: ['settings', 'panel-teams'] },
    { done: urlReady(config), title: t('Public URL and tunnel'), text: t('A stable HTTPS URL that reaches the local port.'), go: ['settings', 'panel-server'] },
    { done: readySources(s.map).length > 0, title: t('Authorized sources'), text: t('At least one source enabled, with external processing and an audience.'), go: ['knowledge'] },
    { done: s.running, title: t('Start the assistant'), text: t('It starts receiving Teams messages.'), start: true },
    { done: !config.policy.dry_run, optional: true, title: t('Turn sending on'), text: t('Review the proposals in Messages and turn observation mode off.'), go: ['settings', 'panel-behavior'] },
  ];
  const required = steps.filter(step => !step.optional);
  const completed = required.filter(step => step.done).length;
  $('#setup-progress').textContent = t('{ready} of {total} ready', { ready: completed, total: required.length });
  $('#setup-bar').style.width = `${Math.round(completed / required.length * 100)}%`;
  const ready = completed === required.length;
  $('#setup-ready').hidden = !ready;
  $('#setup-steps').replaceChildren(...steps.map((step, index) => ({ step, index })).filter(({ step }) => !ready || !step.done).map(({ step, index }) => {
    const mark = el('span', { class: 'step-mark' }, step.done ? icon('check') : String(index + 1));
    const title = el('strong', {}, step.title, step.optional ? el('span', { class: 'optional', textContent: ` · ${t('optional')}` }) : null);
    let action = null;
    if (!step.done && step.go) {
      action = el('button', { class: 'secondary small', textContent: t('Set up') });
      action.onclick = () => showTab(...step.go);
    } else if (!step.done && step.start) {
      action = el('button', { class: 'small', textContent: t('Start') });
      action.onclick = () => startAssistant(action);
    }
    return el('li', { class: `step${step.done ? ' done' : ''}` }, mark, el('div', {}, title, el('p', { textContent: step.text })), action);
  }));
}

function renderLive() {
  const s = live;
  if (!s) return;
  const state = transitions[transition] ?? assistantState(s);
  // A running assistant's dot pulses slowly; a starting or stopping one, faster.
  const motion = transition ? '' : s.running ? ' live' : '';
  $('#status').className = `pill ${state.tone}${motion}`;
  $('#status').title = state.detail;
  $('#status span').textContent = state.pill;
  $('#hero-dot').className = `dot ${state.tone}${motion}`;
  $('#hero-title').textContent = state.title;
  $('#hero-detail').textContent = state.detail;
  $('#version').textContent = `v${s.host_version}${s.headless ? ` · ${t('headless')}` : ''}`;
  $('#restart-offer').hidden = !s.restart_offer;
  $('#start').hidden = s.running;
  $('#restart').hidden = $('#stop').hidden = !s.running;
  for (const button of [$('#start'), $('#restart'), $('#stop')]) button.disabled = Boolean(transition) || button.classList.contains('busy');
  $('#microsoft-state').className = `badge ${s.microsoft_connected ? 'ok' : 'warn'}`;
  $('#microsoft-state').textContent = s.microsoft_connected ? t('Account connected') : t('Account not connected');
  $('#teams-login').textContent = s.microsoft_connected ? t('Reconnect the account') : t('Connect Microsoft account');
  $('#github-state').className = `badge ${s.github_connected ? 'ok' : 'muted'}`;
  $('#github-state').textContent = s.github_connected ? t('Connected') : t('Not connected');
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
  current.revision = lastRevision;
  live = structuredClone(current);
  const config = current.config;
  // The interface follows the configured language; a change reloads the page once.
  if (useLanguage(config.llm.language ?? 'es')) return;
  field('#tenant-id', config.graph.tenant_id); field('#client-id', config.graph.client_id);
  field('#user-id', config.graph.user_id); field('#public-url', config.server.public_url);
  field('#bind', config.server.bind); field('#llm-style', config.llm.style);
  field('#llm-language', config.llm.language ?? 'es');
  field('#tunnel-config', current.tunnel_config);
  $('#cloudflare-tunnel').checked = config.server.cloudflare_tunnel;
  field('#allowed-chats', config.graph.allowed_chats.join('\n'));
  $('#dry-run').checked = config.policy.dry_run;
  $('#discover-chats').checked = config.graph.discover_all_chats;
  renderRegistrationSettings();
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
  const origins = { system: t('System store'), environment: t('Environment variable'), file: t('Private file') };
  for (const [name, source] of Object.entries(live.credentials)) {
    const generated = source === 'missing' && GENERATED.includes(name);
    const retired = source === 'retired';
    const empty = source === 'missing' || source === 'unused';
    const state = generated ? badge(t('Generated on start'), 'info')
      : source === 'missing' ? badge(t('Missing'), 'warn')
      : source === 'unused' ? badge(t('Unused'), 'muted')
      : retired ? badge(t('No longer used'), 'muted')
      : badge(t('Set'), 'ok');
    const detail = generated ? t('The assistant creates it on its first start.')
      : source === 'unused' ? t('The current model chain does not use it.')
      : retired ? t('An earlier version needed it; nothing reads it now. You can remove it.')
      : empty ? t('Needed to start the assistant.')
      : origins[source] || t('Set');
    const title = el('span', { class: 'cred-name', textContent: name });
    const input = el('input', { type: 'password', autocomplete: 'new-password', spellcheck: false, hidden: retired,
      placeholder: empty ? t('Enter the credential') : t('Type only to replace it'), 'aria-label': t('New value of {name}', { name }) });
    const save = el('button', { textContent: empty ? t('Save') : t('Replace'), disabled: true, hidden: retired });
    input.oninput = () => { save.disabled = !input.value.trim(); };
    input.onkeydown = event => { if (event.key === 'Enter' && !save.disabled) save.click(); };
    save.onclick = () => busy(save, async () => {
      await invoke('set_credential', { name, value: input.value });
      input.value = '';
      await refreshLive();
      done(t('{name} stored in the system.', { name }));
    });
    const remove = el('button', { class: 'danger', textContent: t('Remove'), disabled: empty, 'aria-label': t('Remove {name}', { name }) });
    remove.onclick = () => busy(remove, async () => {
      await invoke('delete_credential', { name });
      await refreshLive();
      done(t('{name} removed.', { name }));
    });
    container.append(el('div', { class: 'row' }, rowText(title, detail, state), input, save, remove));
  }
}

const effortNames = { none: t('No reasoning'), minimal: t('Minimal'), low: t('Low'), medium: t('Medium'), high: t('High'), xhigh: t('Very high'), max: t('Maximum') };
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
  if (!info) return t('Provider not available in this version');
  if (info.transport === 'cli') return info.available ? `${t('CLI installed')}${info.version ? ' · ' + info.version : ''}` : t('CLI not installed on this computer');
  const source = live.credentials[info.credential];
  return source && source !== 'missing' && source !== 'unused' ? t('API · {name} set', { name: info.credential }) : t('API · {name} missing in Credentials', { name: info.credential });
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
    const rank = el('span', { class: 'rank', textContent: !row.enabled ? t('Off') : active++ === 0 ? t('Default') : t('Fallback {n}', { n: active - 1 }) });
    const toggle = el('input', { type: 'checkbox', checked: row.enabled, disabled: !row.enabled && !(info?.available), 'aria-label': t('Turn on {name}', { name }) });
    toggle.onchange = () => { row.enabled = toggle.checked; markDirty(); renderChain(); };
    const label = el('label', { class: 'check' }, toggle,
      el('div', {}, el('strong', { textContent: name }), el('small', { textContent: providerStatus(info) })));
    const models = (info?.models ?? []).map(model => [model.id, model.name]);
    if (row.model && !models.some(([id]) => id === row.model)) models.unshift([row.model, `${row.model} (${t('current')})`]);
    const [modelField, model] = select(t('Model'), t('{name} model', { name }), models, row.model);
    model.title = row.model;
    const efforts = () => info?.models.find(m => m.id === row.model)?.efforts ?? info?.efforts ?? [row.effort];
    const effortOptions = () => {
      const values = efforts();
      return (values.includes(row.effort) ? values : [row.effort, ...values]).map(value => [value, effortNames[value] || value]);
    };
    const [effortField, effort] = select(t('Effort'), t('{name} effort', { name }), effortOptions(), row.effort);
    model.onchange = () => {
      row.model = model.value;
      const supported = efforts();
      if (!supported.includes(row.effort)) row.effort = info?.models.find(m => m.id === row.model)?.default_effort ?? supported[0];
      markDirty();
      renderChain();
    };
    effort.onchange = () => { row.effort = effort.value; markDirty(); };
    const order = el('div', { class: 'order' });
    for (const [text, offset, aria] of [['↑', -1, t('Raise the priority of {name}', { name })], ['↓', 1, t('Lower the priority of {name}', { name })]]) {
      const button = el('button', { class: 'secondary', textContent: text, 'aria-label': aria, disabled: !llmRows[index + offset] });
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
  $('#self-chat-badge').textContent = config.graph.self_chat ? t('Enabled') : t('Disabled');
  if (!config.graph.self_chat) {
    $('#self-chat-state').textContent = t('Write to yourself in Teams (your notes or a chat with yourself) and the assistant answers right there, with the sources authorized for that chat.');
    return;
  }
  let diagnostic;
  try { diagnostic = (await invoke('self_chat_status')).diagnostics; } catch { /* shown as pending */ }
  $('#self-chat-state').textContent = t('Chat {id} · received by webhook and polled every 10 s · last poll: {poll} · unresolved outputs: {n}', {
    id: config.graph.self_chat.id,
    poll: diagnostic?.cursor ? new Date(diagnostic.cursor[1] * 1000).toLocaleString(uiLocale()) : t('pending'),
    n: diagnostic?.unresolved_outputs ?? 0,
  });
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
  collectRegistrationSettings();
}

async function save() {
  if (!llmRows.some(row => row.enabled)) throw t('Turn on at least one language model.');
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
    done(t('Assistant started.'));
  });
}

// ---------------------------------------------------------------- activity registration

const activityWindow = new URLSearchParams(window.location.search).get('activity');
let selectedActivity = activityWindow && activityWindow !== 'pending' ? activityWindow : null;
let activityPayload = '';
let activityEditing = false;
let activityRunning = false;
let pendingActivities = [];
let viewedActivity = null;
/** The create/refine/dismiss the host is running for one entry: `{ id, action, day, parentBefore, choice }`. */
let activityOperation = null;
/** The last outcome shown next to the form: `{ id, tone, text }`. */
let activityResult = null;
/** The entry whose day the selector already moved to, so the user can still change the day. */
let activityJumped = null;
let activitySettings = null;
let activitySummary = null;
const activityReviewContexts = new Map();

const activityStates = { pending: [t('Needs input'), 'warn'], refining: [t('Asking assistant'), 'info'], sending: [t('Creating'), 'info'],
  done: [t('Created'), 'ok'], dismissed: [t('Dismissed'), 'muted'], uncertain: [t('Inspect Azure DevOps'), 'error'],
  failed: [t('Rejected'), 'error'], already_registered: [t('Already registered'), 'muted'] };
const runStates = { running: [t('Running'), 'info'], completed: [t('Completed'), 'ok'], failed: [t('Failed'), 'error'], interrupted: [t('Interrupted'), 'error'] };
const hoursText = hours => t('{hours} h', { hours: num(hours) });
const dayText = day => new Date(`${day}T12:00:00`).toLocaleDateString(uiLocale(), { weekday: 'long', day: 'numeric', month: 'long' });

function activityFailureHelp(error) {
  if (/Azure DevOps.*HTTP 401/is.test(error)) return t('Azure DevOps rejected the saved read credential. Check its validity for the selected organization and update it in Settings → Credentials.');
  if (/Azure DevOps.*HTTP 403/is.test(error)) return t('Azure DevOps denied access. Check the credential permissions and access to the selected project.');
  return '';
}
function renderActivityRun(run) {
  const d = run.diagnostics || {};
  const failed = ['failed', 'interrupted'].includes(run.status);
  const error = d.error || (failed ? t('This older run did not save its failure details. The original error cannot be recovered.') : '');
  const prompt = el('button', { class: 'secondary small', textContent: t('Copy diagnostic prompt') });
  prompt.onclick = () => busy(prompt, async () => {
    // Written for a coding agent, so it stays in English like the CLI and the diagnostics.
    const text = [
      `Investigate the activity registration ${run.status} in Personal Teams Assistant ${live?.host_version || ''}.`,
      `Day: ${run.day}. Run ID: ${run.id}. Started: ${new Date(run.created_at * 1000).toISOString()}.`,
      d.finished_at ? `Finished: ${new Date(d.finished_at * 1000).toISOString()}.` : '',
      `Last step: ${d.stage || 'Not recorded'}.`, `Error: ${d.error || (failed ? 'Not recorded by this older run' : 'None recorded')}`,
      `Run configuration: ${JSON.stringify(d.settings || {})}. Agents: ${d.provider || 'Not recorded'}.`,
      `Observation mode: ${d.observation_mode ?? 'Not recorded'}. Summary: ${JSON.stringify(run.summary)}.`,
      d.context ? `Context supplied to this run: ${d.context}` : '',
      $('#activity-review-context').value.trim() ? `Additional context: ${$('#activity-review-context').value.trim()}` : '',
      'Trace the failing step, explain the cause, and propose the smallest fix. Inspect credentials by presence and metadata only; never print secret values. Verify with read-only checks first. Inspect task history before any new registration; never retry an uncertain write.'
    ].filter(Boolean).join('\n\n');
    if (!await copy(text)) throw t('Could not copy the diagnostic prompt.');
    done(t('Diagnostic prompt copied. Paste it into Codex, Claude, or another agent.'));
  });
  const duration = d.finished_at ? t('{n} s', { n: Math.max(0, d.finished_at - run.created_at) }) : t('In progress');
  const help = activityFailureHelp(error);
  const [label, tone] = runStates[run.status] || [run.status, 'info'];
  return el('div', { class: 'activity-card' },
    el('div', { class: 'card-row' }, badge(label, tone),
      el('span', { class: 'grow', textContent: new Date(run.created_at * 1000).toLocaleString(uiLocale()) }), prompt),
    el('p', { class: 'hint', textContent: `${d.finished_at ? duration : failed || run.status === 'completed' ? t('Duration not recorded') : duration} · ${d.stage ? tx(d.stage) : t('Step not recorded')}` }),
    error ? el('p', { class: 'activity-issue', textContent: tx(error) }) : null,
    help ? el('p', { textContent: help }) : null,
    run.summary ? el('p', { textContent: t('{created} tasks created · {pending} need input · {recorded} hours accounted for · {remaining} hours remaining', {
      created: run.summary.created, pending: run.summary.pending,
      recorded: num(run.summary.recorded_hours, 2, 2), remaining: num(run.summary.remaining_hours, 2, 2) }) }) : null,
    ...(run.summary?.warnings || []).map(w => el('p', { class: 'activity-issue', textContent: tx(w) })),
    d.context ? el('details', {}, el('summary', { textContent: t('Context supplied to the agents') }), el('p', { class: 'activity-run-context', textContent: d.context })) : null,
    el('small', { class: 'resource-id', textContent: run.id }));
}

function renderRegistrationSettings() {
  const s = current.config.activity_registration;
  $('#registration-enabled').checked = s.enabled;
  $('#registration-auto').checked = s.auto_register;
  for (const [id, value] of Object.entries({ hours: s.daily_hours, schedule: s.schedule, at: s.at,
    interval: s.interval_minutes, start: s.window_start, end: s.window_end, zone: s.time_zone,
    weekdays: s.weekdays.join(', '), kind: s.task_kind, state: s.done_state, effort: s.effort_field,
    fields: JSON.stringify(s.extra_fields, null, 2) })) field('#registration-' + id, value);
  const sources = current.map.resources.filter(r => ['azure_devops_status', 'azure_devops_wiki', 'teams_messages'].includes(r.tool?.type));
  const writers = sources.filter(r => r.tool.type === 'azure_devops_status' && r.tool.link_secret_ref);
  $('#registration-writer').replaceChildren(el('option', { value: '', textContent: t('Select a source with a write credential') }),
    ...writers.map(r => el('option', { value: r.id, textContent: r.description || r.id, disabled: !r.enabled || !r.external_processing })));
  field('#registration-writer', s.write_source);
  $('#registration-sources').replaceChildren(...sources.map(r => {
    const check = el('input', { type: 'checkbox', checked: s.sources.includes(r.id), disabled: !r.enabled || !r.external_processing });
    check.dataset.source = r.id;
    check.onchange = markDirty;
    return el('label', { class: 'check' }, check, el('span', {}, r.description || r.id,
      !r.enabled || !r.external_processing ? el('small', { textContent: ` ${t('Enable and authorize this source in Knowledge first.')}` }) : null));
  }));
  if (!sources.length) $('#registration-sources').append(el('p', { class: 'hint', textContent: t('Add Azure DevOps activity, Wiki or Teams messages sources in Knowledge to review your work.') }));
}
function jsonFields(text) {
  let value;
  try { value = JSON.parse(text.trim() || '{}'); } catch { throw t('Additional fields must be a JSON object.'); }
  if (!value || Array.isArray(value) || typeof value !== 'object') throw t('Additional fields must be a JSON object.');
  return value;
}
function collectRegistrationSettings() {
  const s = current.config.activity_registration;
  Object.assign(s, { enabled: $('#registration-enabled').checked, auto_register: $('#registration-auto').checked,
    daily_hours: Number($('#registration-hours').value), schedule: $('#registration-schedule').value,
    at: $('#registration-at').value, interval_minutes: Number($('#registration-interval').value),
    window_start: $('#registration-start').value, window_end: $('#registration-end').value,
    time_zone: $('#registration-zone').value.trim(), weekdays: names($('#registration-weekdays').value).map(Number),
    write_source: $('#registration-writer').value, task_kind: $('#registration-kind').value,
    done_state: $('#registration-state').value.trim(), effort_field: $('#registration-effort').value.trim(),
    extra_fields: jsonFields($('#registration-fields').value),
    sources: [...document.querySelectorAll('#registration-sources input:checked')].map(c => c.dataset.source) });
  if (s.write_source && !s.sources.includes(s.write_source)) s.sources.push(s.write_source);
}

function renderActivityNotice() {
  const count = pendingActivities.length;
  // The Activities tab shows the same thing in full.
  $('#activity-notice').hidden = tab === 'registration' || (!count && !activityRunning);
  $('#activity-notice-text').textContent = activityRunning ? t('Reviewing your work…')
    : tn(count, '1 activity needs your context or an HU.', '{n} activities need your context or an HU.');
}

/** Effort of the day against the daily target, from the last saved totals. */
function renderActivityMeter(summary, settings) {
  const target = settings.daily_hours;
  const recorded = summary?.recorded_hours ?? 0;
  $('#activity-hours-text').textContent = summary
    ? t('{recorded} of {target} h registered', { recorded: num(recorded), target: num(target) })
    : t('Daily target: {target} h', { target: num(target) });
  $('#activity-hours-note').textContent = !summary ? t('Review this day to calculate the registered effort.')
    : summary.remaining_hours > 0 ? t('{hours} h left', { hours: num(summary.remaining_hours) }) : t('Daily target reached');
  $('#activity-hours-bar').style.width = `${Math.min(100, Math.round(recorded / target * 100))}%`;
}

function activitySetupText(settings, llm, observing) {
  const agents = llm.chain?.filter(p => p.enabled).map(p => `${providerNames[p.provider] || p.provider} (${p.model})`).join(' → ') || `${llm.provider} (${llm.model})`;
  const mode = observing ? t('Observation mode: nothing is created') : settings.auto_register ? t('Automatic creation when verified') : t('Manual approval');
  const schedule = !settings.enabled ? t('Schedule disabled')
    : settings.schedule === 'daily' ? t('Scheduled at {time}', { time: settings.at })
    : t('Every {n} min ({start}–{end})', { n: settings.interval_minutes, start: settings.window_start, end: settings.window_end });
  return [
    t('Sources: {names}', { names: settings.sources.join(', ') || t('None selected') }),
    t('Agents: {names}', { names: agents }), mode, schedule,
    t('Time zone: {zone}', { zone: settings.time_zone === 'local' ? t('host local time') : settings.time_zone }),
  ].join(' · ');
}

function pendingCard(entry) {
  const open = el('button', { type: 'button', class: `card card-button${entry.id === selectedActivity ? ' selected' : ''}` },
    el('span', { class: 'card-main' },
      el('strong', { textContent: entry.draft.title || t('Activity without a task') }),
      el('span', { class: entry.issue ? 'card-text warn-text' : 'card-text', textContent: tx(entry.issue || entry.draft.question) || t('Review the HU, description and effort before creating the task.') }),
      el('span', { class: 'card-meta' }, badge(dayText(entry.day), 'muted'), entry.draft.hours ? badge(hoursText(entry.draft.hours), 'info') : null,
        entry.state === 'refining' ? badge(t('Asking assistant'), 'info') : null)),
    icon('arrow'));
  open.dataset.id = entry.id;
  open.title = activityWindow || window.ptaTransport.browser ? t('Review') : t('Review in new window');
  open.onclick = () => openActivity(entry, open);
  return open;
}

function historyCard(entry) {
  const [label, tone] = activityStates[entry.state] || [entry.state, 'muted'];
  const parent = entry.parents.find(p => p.id === entry.draft.parent);
  const title = entry.task_url ? link(`#${entry.task_id} ${entry.draft.title}`, entry.task_url) : el('strong', { textContent: entry.draft.title || t('Activity') });
  const facts = el('p', { class: 'card-text' }, parent ? [`${t('HU')} `, link(`#${parent.id} ${parent.title}`, parent.url)] : t('No HU selected'),
    ` · ${entry.draft.hours ? hoursText(entry.draft.hours) : t('No effort yet')}`);
  let action = null;
  if (entry.state === 'pending') {
    action = el('button', { type: 'button', class: 'secondary small', textContent: t('Open') });
    action.onclick = () => openActivity(entry, action);
  }
  return el('div', { class: 'card' }, el('div', { class: 'card-row' }, badge(label, tone), action), title,
    facts, entry.issue ? el('p', { class: 'card-text warn-text', textContent: tx(entry.issue) }) : null);
}

/** Opens an entry inline (browser and the activity window) or in the native activity window. */
async function openActivity(entry, button) {
  if (!activityWindow && !window.ptaTransport.browser) return busy(button, () => invoke('activity_open', { id: entry.id }));
  selectedActivity = entry.id; activityEditing = false; activityPayload = '';
  if (activityResult?.id !== entry.id) activityResult = null;
  renderActivityEditor(entry);
  $('#activity-editor').scrollIntoView({ behavior: 'smooth', block: 'start' });
  await loadRegistration().catch(fail);
}

function closeActivity() {
  selectedActivity = null; viewedActivity = null; activityEditing = false; activityPayload = '';
  $('#activity-editor').hidden = true;
  $('#registration').classList.remove('editing');
  for (const card of document.querySelectorAll('#activity-pending .card-button.selected')) card.classList.remove('selected');
}

/** A result that outlives the editor: a created task, a dismissal. */
function flashActivity(tone, ...content) {
  const box = $('#activity-flash');
  box.className = `callout ${tone}`;
  box.replaceChildren(...content);
  box.hidden = false;
}

async function loadRegistration() {
  let [history, pending] = await Promise.all([
    invoke('activity_history', $('#activity-day').value ? { day: $('#activity-day').value } : {}), invoke('activity_pending')
  ]);
  const selected = pending.entries.find(e => e.id === selectedActivity);
  // Opening an entry shows its day once; afterwards the user can pick any other day.
  if (selected && activityJumped !== selected.id) {
    activityJumped = selected.id;
    if (history.day !== selected.day) {
      field('#activity-day', selected.day);
      field('#activity-review-context', activityReviewContexts.get(selected.day) || '');
      history = await invoke('activity_history', { day: selected.day });
    }
  }
  if (!$('#activity-day').value) field('#activity-day', history.day);
  activityRunning = history.running;
  const latest = history.runs[0];
  const settings = activitySettings = history.settings || live.config.activity_registration;
  const llm = history.llm || live.config.llm;
  const activeRun = history.runs.find(r => r.status === 'running');
  pendingActivities = pending.entries;
  const count = pending.entries.length;
  $('#nav-activities').hidden = !count;
  $('#nav-activities').textContent = count;
  renderActivityNotice();
  $('#activity-progress').hidden = !activityRunning;
  $('#activity-running').textContent = activityOperation ? operationText(activityOperation.action)
    : activeRun?.diagnostics?.stage ? `${tx(activeRun.diagnostics.stage)}…` : t('An activity operation is running.');
  $('#activity-setup').textContent = activitySetupText(settings, llm, history.observation_mode);
  const latestFailed = latest && ['failed', 'interrupted'].includes(latest.status);
  $('#activity-result').hidden = !latestFailed;
  $('#activity-result').textContent = latestFailed ? t('The last review of this day did not finish ({step}). See Process runs and diagnostics below.', {
    step: latest.diagnostics?.stage ? tx(latest.diagnostics.stage) : runStates[latest.status][0] }) : '';
  $('#activity-run').disabled = activityRunning;
  const summary = activitySummary = history.summary || history.runs.find(r => r.status === 'completed' && r.summary)?.summary || null;
  renderActivityMeter(summary, settings);
  const created = history.entries.filter(e => e.state === 'done').length;
  const dayPending = history.entries.filter(e => e.state === 'pending').length;
  $('#activity-summary').replaceChildren(...[
    created ? badge(tn(created, '1 task created', '{n} tasks created'), 'ok') : null,
    dayPending ? badge(tn(dayPending, '1 needs input', '{n} need input'), 'warn') : null,
    history.observation_mode ? badge(t('Observation mode: nothing is created'), 'info') : null,
    summary?.partial ? el('details', { class: 'fold warnings' }, el('summary', { textContent: tn(summary.warnings.length, '1 coverage warning', '{n} coverage warnings') }),
      ...summary.warnings.map(w => el('p', { class: 'activity-issue', textContent: tx(w) }))) : null,
  ].filter(Boolean));
  $('#activity-pending-count').textContent = count || '';
  $('#activity-pending').replaceChildren(...pending.entries.map(pendingCard));
  if (!count) $('#activity-pending').append(el('p', { class: 'empty-note', textContent: t('Nothing needs your input. Review a day to look for unregistered work.') }));
  $('#activity-history').replaceChildren(...history.entries.map(historyCard));
  if (!history.entries.length) $('#activity-history').append(el('p', { class: 'empty-note', textContent: history.runs.length ? t('The review found no activities to register on this day.') : t('This day has not been reviewed yet.') }));
  $('#activity-runs').replaceChildren(...history.runs.map(renderActivityRun));
  $('#activity-runs-count').textContent = history.runs.length || '';
  if (!history.runs.length) $('#activity-runs').append(el('p', { class: 'hint', textContent: t('No reviews have run for this day.') }));
  let entry = [...pending.entries, ...history.entries].find(e => e.id === selectedActivity);
  if (activityWindow && !selectedActivity && count) { entry = pending.entries[0]; selectedActivity = entry.id; }
  if (entry) renderActivityEditor(entry);
  else if (selectedActivity && !activityOperation) closeActivity();
}

function operationText(action) {
  return action === 'refine' ? t('The assistant is updating the draft…')
    : action === 'dismiss' ? t('Dismissing the activity…') : t('Creating the task in Azure DevOps…');
}

/** The call/work-item confirmation an entry still needs, when the agents asked nothing. */
function confirmationNote(entry) {
  if (!entry.activities.some(a => a.context_required)) return '';
  return entry.activities.some(a => a.context_required && a.organization === 'https://teams.microsoft.com')
    ? t('Confirm that you attended this call, its purpose and its HU before registering it.')
    : t('Confirm the referenced work items and corresponding HU before registering this work.');
}

function renderParents(entry, value) {
  const suggested = entry.draft.suggested_parents;
  const parents = [...entry.parents].sort((a, b) => Number(suggested.includes(b.id)) - Number(suggested.includes(a.id)));
  $('#activity-parent').replaceChildren(el('option', { value: '', textContent: t('Select an HU') }),
    ...parents.map(p => el('option', { value: p.id, textContent: `${suggested.includes(p.id) ? '★ ' : ''}#${p.id} ${p.title}${p.state ? ` · ${p.state}` : ''}` })),
    el('option', { value: 'other', textContent: t('Other HU…') }));
  field('#activity-parent', value ?? entry.draft.parent ?? '');
  $('#activity-other-parent-label').hidden = $('#activity-parent').value !== 'other';
}

function renderActivityOutcome(entry) {
  const box = $('#activity-outcome');
  let tone = '', content = [];
  if (activityOperation?.id === entry.id) {
    tone = 'info';
    content = [el('span', { class: 'spinner', 'aria-hidden': 'true' }), el('span', { textContent: operationText(activityOperation.action) })];
  } else if (activityResult?.id === entry.id) {
    tone = activityResult.tone;
    content = [activityResult.text];
  } else if (entry.issue) {
    tone = ['failed', 'uncertain'].includes(entry.state) ? 'error' : 'warn';
    content = [tx(entry.issue)];
  }
  box.hidden = !tone;
  box.className = `callout ${tone}`;
  box.replaceChildren(...content);
}

function renderActivityEditor(entry) {
  viewedActivity = entry;
  $('#activity-editor').hidden = false;
  $('#registration').classList.add('editing');
  for (const card of document.querySelectorAll('#activity-pending .card-button')) card.classList.toggle('selected', card.dataset.id === entry.id);
  const needsInput = ['pending', 'refining'].includes(entry.state);
  const payload = JSON.stringify(entry);
  if (payload !== activityPayload && !activityEditing) {
    activityPayload = payload;
    $('#activity-editor-day').textContent = dayText(entry.day);
    $('#activity-editor-title').textContent = entry.draft.title || t('Review activity');
    $('#activity-evidence').replaceChildren(...entry.activities.map(a => el('li', {}, link(a.label, a.url),
      a.context_required ? badge(t('Needs your confirmation'), 'warn') : null)));
    const question = entry.draft.question || confirmationNote(entry);
    $('#activity-ask').hidden = !needsInput;
    $('#activity-question').textContent = question ? tx(question) : t('The draft looks complete. Check it below, or tell the assistant what to change.');
    $('#activity-refine-box').open = false;
    renderParents(entry);
    field('#activity-title', entry.draft.title); field('#activity-description', entry.draft.description);
    field('#activity-hours', entry.draft.hours); field('#activity-context', '');
    field('#activity-fields', JSON.stringify(entry.fields, null, 2));
  }
  const [label, tone] = activityStates[entry.state] || [entry.state, 'muted'];
  $('#activity-editor-state').className = `badge ${tone}`;
  $('#activity-editor-state').textContent = label;
  const left = activitySummary && entry.day === $('#activity-day').value ? activitySummary.remaining_hours : null;
  $('#activity-hours-left').textContent = left === null ? '' : t('{hours} h left of the daily target', { hours: num(left) });
  renderActivityOutcome(entry);
  const disabled = activityRunning || Boolean(activityOperation) || entry.state !== 'pending';
  for (const input of document.querySelectorAll('#activity-editor input, #activity-editor textarea, #activity-editor select, #activity-editor form button, #activity-refine')) input.disabled = disabled;
  $('#activity-create').classList.toggle('busy', activityOperation?.id === entry.id && activityOperation.action === 'create');
}

/** What the form still lacks before asking the host to create the task. */
function activityFormProblem() {
  const parent = $('#activity-parent').value === 'other' ? $('#activity-other-parent').value : $('#activity-parent').value;
  const hours = Number($('#activity-hours').value);
  if (!parent || !(Number(parent) > 0)) return [t('Choose the HU the task goes under before creating it.'), $('#activity-parent').value === 'other' ? '#activity-other-parent' : '#activity-parent'];
  if (!$('#activity-title').value.trim()) return [t('Write the task title.'), '#activity-title'];
  if (!$('#activity-hours').value || !(hours >= 0.01 && hours <= 24)) return [t('Enter the effort in hours (between 0.01 and 24).'), '#activity-hours'];
  if (!$('#activity-description').value.trim()) return [t('Describe the work done before creating the task.'), '#activity-description'];
  if ($('#activity-description').value.includes('://')) return [t("Remove the links from the description: the activity's links are added to the task automatically."), '#activity-description'];
  return null;
}

/** Shows what is missing next to the buttons and marks the field, without opening the phone keyboard. */
function showActivityProblem(text, selector) {
  activityResult = { id: selectedActivity, tone: 'warn', text };
  if (viewedActivity) renderActivityOutcome(viewedActivity);
  for (const marked of document.querySelectorAll('#activity-editor .invalid')) { marked.classList.remove('invalid'); marked.removeAttribute('aria-invalid'); }
  const target = selector && $(selector);
  if (target) {
    target.closest('details')?.setAttribute('open', '');
    target.classList.add('invalid');
    target.setAttribute('aria-invalid', 'true');
  }
  $('#activity-outcome').scrollIntoView({ behavior: 'smooth', block: 'center' });
}

async function resolveActivity(action) {
  if (!selectedActivity || !viewedActivity) throw t('Select an activity.');
  if (action === 'create') {
    const problem = activityFormProblem();
    if (problem) return showActivityProblem(...problem);
  }
  if (action === 'refine' && !$('#activity-context').value.trim()) return showActivityProblem(t('Write your answer for the assistant first.'), '#activity-context');
  let fields = {};
  if (action !== 'dismiss') {
    try { fields = jsonFields($('#activity-fields').value); } catch (error) { return showActivityProblem(String(error), '#activity-fields'); }
  }
  const choice = $('#activity-parent').value;
  const parentValue = choice === 'other' ? $('#activity-other-parent').value : choice;
  const resolution = action === 'dismiss' ? { action } : {
    action, parent: parentValue ? Number(parentValue) : null, title: $('#activity-title').value.trim(),
    description: $('#activity-description').value.trim(), hours: $('#activity-hours').value ? Number($('#activity-hours').value) : null,
    context: $('#activity-context').value.trim(), fields
  };
  await invoke('activity_resolve', { id: selectedActivity, resolution });
  activityOperation = { id: selectedActivity, action, day: viewedActivity.day, parentBefore: viewedActivity.draft.parent ?? null, choice };
  activityResult = null;
  activityRunning = true;
  renderActivityEditor(viewedActivity);
  $('#activity-outcome').scrollIntoView({ behavior: 'smooth', block: 'center' });
  watchActivityOperation(activityOperation);
}

/** Polls until the host finishes the operation, then shows its result next to the form. */
async function watchActivityOperation(operation) {
  const deadline = Date.now() + 30 * 60_000;
  while (activityOperation === operation && Date.now() < deadline) {
    await sleep(1500);
    let history;
    try { history = await invoke('activity_history', { day: operation.day }); } catch { continue; }
    if (history.running) continue;
    activityOperation = null;
    finishActivityOperation(operation, history.entries.find(e => e.id === operation.id));
    await loadRegistration().catch(fail);
    return;
  }
}

function finishActivityOperation(operation, entry) {
  const result = (tone, text) => { activityResult = { id: operation.id, tone, text }; };
  if (!entry) {
    result('error', t('The activity could not be read again. Refresh the list.'));
  } else if (entry.state === 'done') {
    const parent = entry.parents.find(p => p.id === entry.draft.parent);
    const task = entry.task_url ? link(`#${entry.task_id} ${entry.draft.title}`, entry.task_url) : `«${entry.draft.title}»`;
    flashActivity('ok', icon('check'), el('span', {}, t('Task created in Done:'), ' ', task,
      parent ? [` · ${t('HU')} `, link(`#${parent.id} ${parent.title}`, parent.url)] : null, ` · ${hoursText(entry.draft.hours)}`));
    done(t('Task #{id} created in Done.', { id: entry.task_id }));
    if (selectedActivity === operation.id) closeActivity();
    $('#activity-flash').scrollIntoView({ behavior: 'smooth', block: 'center' });
    return;
  } else if (['dismissed', 'already_registered'].includes(entry.state)) {
    flashActivity('info', entry.state === 'dismissed' ? t('Activity dismissed: «{title}». It will not be proposed again.', { title: entry.draft.title || t('Activity') }) : tx(entry.issue));
    if (selectedActivity === operation.id) closeActivity();
    return;
  } else if (['failed', 'uncertain'].includes(entry.state)) {
    result('error', tx(entry.issue));
  } else if (operation.action === 'refine') {
    result(entry.issue ? 'warn' : 'ok', tx(entry.issue) || t('The assistant updated the draft. Review it and create the task.'));
    activityEditing = false; activityPayload = '';
  } else {
    result('warn', tx(entry.issue) || t('The task was not created. Review the details and try again.'));
    // The text the user typed stays; the HU follows the host when it chose or reset one.
    if (selectedActivity === entry.id) {
      const parent = (entry.draft.parent ?? null) !== operation.parentBefore || operation.choice === 'other' ? entry.draft.parent ?? '' : operation.choice;
      renderParents(entry, parent);
      activityEditing = true;
    }
  }
  if (selectedActivity === operation.id) {
    renderActivityEditor(entry || viewedActivity);
    $('#activity-outcome').scrollIntoView({ behavior: 'smooth', block: 'center' });
  }
}

// ---------------------------------------------------------------- messages

const statusLabels = {
  sent: [t('Answered'), 'ok'], dry_run: [t('Observed (not sent)'), 'info'], sending: [t('Sending'), 'active'],
  processing: [t('In progress'), 'active'], pending: [t('Queued'), 'active'], failed: [t('Error'), 'error'],
  uncertain: [t('Uncertain send'), 'error'],
};
const reasonLabels = {
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
};
const kindLabels = { self: t('Personal chat'), direct: t('Direct'), group: t('Group'), unsupported: t('Other') };
const intentLabels = { question: t('question'), activity_review: t('activity review'), greeting: t('greeting'), shared_link: t('shared link'), link: t('link work to work items') };
const linkLabels = { sending: t('sending'), linked: t('linked'), already_linked: t('already linked'), failed: t('failed'), uncertain: t('uncertain (check the work item; never retried)') };
let messageRows = [];
let messageFilter = 'all';
const openMessages = new Set();

function messageState(row) {
  const reason = row.audit.reason || '';
  if (row.status === 'ignored') {
    if (reason === 'ineligible_message') return [t('Not addressed'), 'muted'];
    if (/^(unsafe_|invalid_references|reference_selection_failed|final_gate)/.test(reason)) return [t('Withheld'), 'warn'];
    return [t('No answer'), 'muted'];
  }
  return statusLabels[row.status] || [row.status, 'muted'];
}

function reasonText(reason) {
  if (!reason) return '';
  if (reason.startsWith('invalid_references')) return t('Withheld: references or links that could not be verified ({detail}).', { detail: reason.replace('invalid_references: ', '') });
  return reasonLabels[reason] || reason;
}

const millis = row => row.audit.received_at ?? row.created_at * 1000;
const relative = new Intl.RelativeTimeFormat(uiLocale(), { numeric: 'auto' });
function ago(at) {
  const seconds = Math.round((at - Date.now()) / 1000);
  for (const [unit, size] of [['day', 86400], ['hour', 3600], ['minute', 60]]) {
    if (Math.abs(seconds) >= size) return relative.format(Math.round(seconds / size), unit);
  }
  return t('just now');
}
const when = row => new Date(millis(row)).toLocaleString(uiLocale());
const ineligible = row => row.audit.reason === 'ineligible_message' && row.status === 'ignored';

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
  $('#messages-summary').textContent = `${t('{n} recent messages', { n: messageRows.length })} · ` + Object.entries(counts).map(([label, n]) => `${label}: ${n}`).join(' · ');
  list.replaceChildren();
  if (!rows.length) {
    list.append(el('div', { class: 'empty' }, el('p', { textContent: messageRows.length ? t('No messages for this filter.') : t('No messages yet. They show up here when the assistant receives one from Teams.') })));
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
    messageField(body, t('Message'), audit.question, true);
    if (audit.resolved_question) messageField(body, t('Interpreted with the context'), `${audit.resolved_question}${audit.topic ? ` · ${t('topic searched')}: ${audit.topic}` : ''}`);
    messageField(body, t('Outcome'), reasonText(audit.reason));
    if (row.status === 'sent') messageField(body, t('Answer sent'), audit.sent || audit.proposed, 'markdown');
    else messageField(body, row.status === 'dry_run' ? t('Proposal (observation mode)') : t('Proposed answer (not sent)'), audit.proposed, 'markdown');
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
  $('#activity-stats').replaceChildren(...stats.filter(([, n]) => n).map(([label, n, tone, filter]) => {
    const chip = el('button', { class: `badge ${tone}`, textContent: `${label}: ${n}`, title: t('See in Messages') });
    chip.onclick = () => { setFilter(filter); showTab('messages'); };
    return chip;
  }));
  const recent = relevant.slice(0, 6);
  $('#activity-list').replaceChildren(...(recent.length ? recent.map(row => {
    const [label, tone] = messageState(row);
    return el('div', { class: 'activity-row' }, badge(label, tone), el('time', { textContent: ago(millis(row)), title: when(row) }),
      el('span', { class: 'excerpt', textContent: excerpt(row), title: excerpt(row) }));
  }) : [el('div', { class: 'empty' }, el('p', { textContent: t('No messages addressed to the assistant yet.') }))]));
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
  azure_devops_wiki: 'Azure DevOps Wiki', azure_devops_status: t('Azure DevOps activity'), azure_devops: t('Azure DevOps work items'),
  sql_server: 'SQL Server', rabbitmq: t('RabbitMQ queue'), http: 'HTTP GET', teams_messages: t('My Teams messages'),
};

function kindName(resource) {
  if (resource.kind === 'file') return t('File');
  if (resource.kind === 'url') return 'URL';
  return toolNames[resource.tool?.type] || resource.tool?.type || t('Tool');
}

function accessText(resource) {
  if (resource.kind === 'file') return `${resource.repository} / ${resource.path}`;
  if (resource.kind === 'url') return resource.url;
  const tool = resource.tool ?? {};
  if (tool.organization) return `${tool.organization} / ${tool.project}`;
  if (tool.repository) return `${t('Catalog')}: ${tool.repository} / ${tool.path}`;
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

function renderResources() {
  const container = $('#resources');
  container.replaceChildren();
  const resources = current.map.resources;
  $('#sources-summary').textContent = resources.length ? t('{ready} of {total} ready for Teams', { ready: readySources(current.map).length, total: resources.length }) : '';
  if (!resources.length) {
    container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No sources yet. Add a file from a repository or register the Wiki with pta sources add.') })));
  }
  for (const resource of resources) {
    const input = (label, value, placeholder, onchange) => {
      const element = el('input', { value, placeholder, spellcheck: false });
      element.oninput = () => { onchange(element.value); markDirty(); };
      return el('label', {}, label, element);
    };
    const toggles = el('div', { class: 'actions' });
    for (const [key, text] of [['enabled', t('Enabled')], ['external_processing', t('Authorize external processing')]]) {
      const box = el('input', { type: 'checkbox', checked: resource[key] });
      box.onchange = () => { resource[key] = box.checked; markDirty(); renderResources(); };
      toggles.append(el('label', { class: 'check' }, box, text));
    }
    const remove = el('button', { class: 'danger small', textContent: t('Remove source') });
    remove.onclick = () => { current.map.resources = current.map.resources.filter(r => r !== resource); markDirty(); renderResources(); };
    toggles.append(remove);
    let state = sourceState(resource);
    container.append(el('div', { class: 'resource' },
      el('div', { class: 'resource-head' }, el('strong', { textContent: resource.id }), badge(kindName(resource), 'active'), el('span', { class: 'grow' }), state),
      el('div', { class: 'access', textContent: accessText(resource) }),
      el('div', { class: 'grid' },
        input(t('Description'), resource.description, t('Which questions it answers'), value => { resource.description = value; }),
        input(t('Topics'), resource.topics.join(', '), t('Comma separated'), value => { resource.topics = names(value); }),
        input(t('Authorized conversations'), resource.allowed_conversations.join(', '), t('Comma-separated chat IDs, or *'), value => {
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
  if (!entries.length) container.append(el('div', { class: 'empty' }, el('p', { textContent: t('No repositories. Add a local checkout or clone one from GitHub.') })));
  for (const [alias, path] of entries) {
    choices.append(new Option(alias, alias));
    const input = el('input', { value: path, spellcheck: false, 'aria-label': t('Path of {alias}', { alias }) });
    input.oninput = () => { current.map.repositories[alias] = input.value.trim(); markDirty(); };
    const update = el('button', { class: 'secondary', textContent: t('Update') });
    update.onclick = () => busy(update, async () => { await invoke('update_github_repository', { alias }); done(t('{alias} updated.', { alias })); });
    const remove = el('button', { class: 'danger', textContent: t('Remove') });
    remove.onclick = () => {
      const tools = current.map.resources.filter(r => r.kind === 'tool' && r.tool?.repository === alias);
      if (tools.length) return fail(t('{alias} cannot be removed: {ids} use it. Remove those sources first.', { alias, ids: tools.map(r => r.id).join(', ') }));
      const files = current.map.resources.filter(r => r.repository === alias);
      delete current.map.repositories[alias];
      current.map.resources = current.map.resources.filter(r => r.repository !== alias);
      markDirty();
      renderRepositories(); renderResources();
      if (files.length) notify(t('Its sources were removed too: {ids}.', { ids: files.map(r => r.id).join(', ') }));
    };
    container.append(el('div', { class: 'row' }, rowText(alias, t('Git checkout')), input, update, remove));
  }
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

/** In the browser a link opens in a new tab; the app window never navigates, so there it copies the URL. */
function link(label, url) {
  if (window.ptaTransport.browser) return el('a', { class: 'ref', href: url, target: '_blank', rel: 'noopener noreferrer', textContent: label });
  const button = el('button', { type: 'button', class: 'ref', textContent: label, title: t('Copy link: {url}', { url }) });
  button.onclick = async () => {
    if (await copy(url)) done(t('Link copied: {url}', { url }));
    else notify(t('Link: {url}', { url }));
  };
  return button;
}

const INLINE = new RegExp([
  /\\([!-\/:-@[-`{-~])/, // escaped punctuation
  /`([^`]+)`/,
  /\*\*\*(\S(?:.*?\S)?)\*\*\*/,
  /\*\*(\S(?:.*?\S)?)\*\*|(?<!\w)__(\S(?:.*?\S)?)__(?!\w)/,
  /~~(\S(?:.*?\S)?)~~/,
  /\*([^\s*](?:[^*]*?[^\s*])?)\*|(?<!\w)_([^\s_](?:[^_]*?[^\s_])?)_(?!\w)/,
  /\{(red|orange|green|blue|gray):([^{}]*\S[^{}]*)\}/,
  /\[([^\]]+)\]\((https:\/\/[^\s)]+)\)/,
].map(part => part.source).join('|'), 'g');

function inline(text, parent) {
  const wrap = (tag, body, props) => { const node = el(tag, props); inline(body, node); return node; };
  let last = 0;
  for (const match of text.matchAll(INLINE)) {
    if (match.index > last) parent.append(text.slice(last, match.index));
    const [, escaped, code, both, bold, underBold, strike, italic, underItalic, tone, toned, label, url] = match;
    if (escaped !== undefined) parent.append(escaped);
    else if (code !== undefined) parent.append(el('code', { textContent: code }));
    else if (both !== undefined) parent.append(el('strong', {}, wrap('em', both)));
    else if ((bold ?? underBold) !== undefined) parent.append(wrap('strong', bold ?? underBold));
    else if (strike !== undefined) parent.append(wrap('s', strike));
    else if ((italic ?? underItalic) !== undefined) parent.append(wrap('em', italic ?? underItalic));
    else if (tone !== undefined) parent.append(wrap('span', toned.trim(), { class: `tone-${tone}` }));
    else parent.append(link(label, url));
    last = match.index + match[0].length;
  }
  if (last < text.length) parent.append(text.slice(last));
}

const LIST_ITEM = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;
const FENCE = /^\s*```/;
const RULE = /^([-*_])(?:\s*\1){2,}\s*$/;
const HEADING = /^(#{1,6})\s+(\S.*)$/;
const QUOTE = /^\s*>\s?(.*)$/;

/** Cells of a pipe-table row: `|` separates them unless escaped or inside a code span; the outer pipes are optional. */
function cells(row) {
  const text = row.trim().replace(/^\|/, '');
  const out = [''];
  let code = false;
  for (let k = 0; k < text.length; k++) {
    if (text[k] === '\\' && text[k + 1] === '|') { out[out.length - 1] += '|'; k++; }
    else if (text[k] === '|' && !code) out.push('');
    else { code = code !== (text[k] === '`'); out[out.length - 1] += text[k]; }
  }
  if (text.endsWith('|') && out[out.length - 1] === '') out.pop();
  return out.map(cell => cell.trim());
}
const isTable = (lines, i) => lines[i].includes('|') && Boolean(lines[i + 1]?.includes('|'))
  && cells(lines[i + 1]).length === cells(lines[i]).length && cells(lines[i + 1]).every(cell => /^:?-+:?$/.test(cell));
// Lines that open a block of their own end the paragraph or list before them.
const opensBlock = (lines, i, quotes) => FENCE.test(lines[i]) || RULE.test(lines[i]) || HEADING.test(lines[i])
  || (quotes && QUOTE.test(lines[i])) || isTable(lines, i);

function table(header, rows) {
  const row = (tag, values) => el('tr', {}, ...header.map((_, k) => { const cell = el(tag); inline(values[k] ?? '', cell); return cell; }));
  return el('div', { class: 'table' }, el('table', {}, el('thead', {}, row('th', header)), el('tbody', {}, ...rows.map(values => row('td', values)))));
}

/** Renders the Markdown the assistant writes (the same subset Teams receives as HTML), without innerHTML. Quotes are one level deep. */
function markdown(text, quotes = true) {
  const root = el('div', { class: 'md' });
  const source = text.replace(/\r/g, '').split('\n');
  let i = 0;
  while (i < source.length) {
    const line = source[i];
    if (FENCE.test(line)) {
      const code = [];
      i++;
      while (i < source.length && !/^\s*```\s*$/.test(source[i])) code.push(source[i++]);
      i++;
      root.append(el('pre', {}, el('code', { textContent: code.join('\n') })));
      continue;
    }
    if (!line.trim()) { i++; continue; }
    if (RULE.test(line)) { root.append(el('hr')); i++; continue; }
    const heading = line.match(HEADING);
    if (heading) {
      const node = el(heading[1].length <= 2 ? 'h2' : 'h3');
      inline(heading[2].trim(), node);
      root.append(node);
      i++;
      continue;
    }
    if (quotes && QUOTE.test(line)) {
      const quoted = [];
      while (i < source.length && QUOTE.test(source[i])) quoted.push(source[i++].match(QUOTE)[1]);
      root.append(el('blockquote', {}, markdown(quoted.join('\n'), false)));
      continue;
    }
    if (isTable(source, i)) {
      const header = cells(source[i]);
      const rows = [];
      i += 2;
      while (i < source.length && source[i].includes('|') && source[i].trim() && !FENCE.test(source[i])) rows.push(cells(source[i++]));
      root.append(table(header, rows));
      continue;
    }
    if (LIST_ITEM.test(line)) {
      const items = [];
      while (i < source.length && source[i].trim() && !opensBlock(source, i, quotes)) {
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
        const task = item.text.match(/^\[([ xX])\] (.*)$/);
        if (task) entry.append(task[1] === ' ' ? '☐ ' : '☑ ');
        inline(task ? task[2] : item.text, entry);
        top.list.append(entry);
        top.item = entry;
      }
      continue;
    }
    const paragraph = el('p');
    do {
      if (paragraph.childNodes.length) paragraph.append(el('br'));
      inline(source[i].trim(), paragraph);
      i++;
    } while (i < source.length && source[i].trim() && !LIST_ITEM.test(source[i]) && !opensBlock(source, i, quotes));
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
  if (!result.answer) return bubble('bot none', `${t('No answer')}: ${reasonText(result.reason) || result.status}`);
  const meta = el('div', { class: 'meta' });
  for (const id of result.used_sources ?? []) meta.append(badge(id, 'active'));
  if (result.partial) meta.append(badge(t('Partial coverage'), 'warn'));
  for (const warning of result.warnings ?? []) meta.append(badge(warning, 'warn'));
  let refs = null;
  if (result.references?.length) {
    refs = el('div', { class: 'refs' }, el('strong', { textContent: t('Verified references') }),
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
$('#offer-start').onclick = () => busy($('#offer-start'), async () => {
  await invoke('start_assistant'); await refreshLive(); done(t('The assistant keeps running with the new version.'));
});
$('#offer-dismiss').onclick = () => busy($('#offer-dismiss'), async () => {
  await invoke('dismiss_restart_offer'); await refreshLive(); notify(t('The assistant stays stopped. You can start it whenever you want.'));
});
$('#activity-all').onclick = () => showTab('messages');
$('#activity-notice-open').onclick = () => busy($('#activity-notice-open'), () => invoke('activity_open'));
$('#activity-settings').onclick = () => showTab('settings', 'panel-registration');
$('#activity-refresh').onclick = () => busy($('#activity-refresh'), loadRegistration);
$('#activity-day').onchange = () => {
  field('#activity-review-context', activityReviewContexts.get($('#activity-day').value) || '');
  $('#activity-flash').hidden = true;
  loadRegistration().catch(fail);
};
$('#activity-review-context').oninput = () => activityReviewContexts.set($('#activity-day').value, $('#activity-review-context').value);
$('#activity-run').onclick = () => busy($('#activity-run'), async () => {
  if (dirty) await save();
  $('#activity-flash').hidden = true;
  await invoke('activity_run', { day: $('#activity-day').value || null, context: $('#activity-review-context').value.trim() });
  notify(t('Reviewing your work. This can take a few minutes; the page updates by itself.'));
  await loadRegistration();
});
$('#activity-parent').onchange = () => {
  activityEditing = true;
  $('#activity-other-parent-label').hidden = $('#activity-parent').value !== 'other';
  if ($('#activity-parent').value === 'other') $('#activity-other-parent').focus();
};
$('#activity-form').oninput = event => {
  activityEditing = true;
  event.target.classList?.remove('invalid');
  event.target.removeAttribute?.('aria-invalid');
};
$('#activity-context').oninput = () => { activityEditing = true; };
$('#activity-form').onsubmit = event => { event.preventDefault(); busy($('#activity-create'), () => resolveActivity('create')); };
$('#activity-refine').onclick = () => busy($('#activity-refine'), () => resolveActivity('refine'));
let dismissArmed = null;
$('#activity-dismiss').onclick = () => {
  // Dismissing is permanent: the first tap asks, the second one dismisses.
  const button = $('#activity-dismiss');
  if (dismissArmed !== selectedActivity) {
    dismissArmed = selectedActivity;
    button.textContent = t('Tap again to dismiss');
    button.classList.add('danger');
    setTimeout(() => { dismissArmed = null; button.textContent = t('Dismiss'); button.classList.remove('danger'); }, 4000);
    return;
  }
  dismissArmed = null;
  button.textContent = t('Dismiss'); button.classList.remove('danger');
  busy(button, () => resolveActivity('dismiss'));
};
$('#activity-back').onclick = () => {
  closeActivity();
  $('#activity-pending-panel').scrollIntoView({ behavior: 'smooth', block: 'start' });
};
setInterval(() => { if (current && !activityOperation) loadRegistration().catch(() => {}); }, 5_000);
if (window.__TAURI__?.event?.listen) {
  // Pushed on every start/stop, including from the tray and `pta`.
  window.__TAURI__.event.listen('assistant-phase', event => {
    transition = transitions[event.payload] ? event.payload : null;
    // Once settled, render from a fresh snapshot rather than the one from before the change.
    if (transition || !current) renderLive();
    else refreshLive().catch(() => renderLive());
  });
  window.__TAURI__.event.listen('assistant-error', event => fail(event.payload));
  window.__TAURI__.event.listen('activity-registration', () => { if (!activityOperation) loadRegistration().catch(fail); });
  window.__TAURI__.event.listen('activity-open', event => {
    selectedActivity = event.payload.id; activityEditing = false; activityPayload = '';
    showTab('registration');
  });
}
$('#messages-refresh').onclick = () => busy($('#messages-refresh'), loadMessages);
document.querySelectorAll('#messages-filters button').forEach(button => { button.onclick = () => setFilter(button.dataset.filter); });
$('#messages-hide-ineligible').onchange = renderMessages;
setInterval(() => {
  if (tab === 'messages' && $('#messages-auto').checked) loadMessages().catch(() => {});
}, 10_000);
$('#detect-llm').onclick = () => busy($('#detect-llm'), async () => {
  llmCatalog = await invoke('llm_providers'); renderChain(); done(t('CLIs detected again.'));
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
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(alias) || !path) return fail(t('Give a valid alias (lowercase letters, digits, - or _) and the checkout path.'));
  if (current.map.repositories[alias]) return fail(t('That alias already exists.'));
  current.map.repositories[alias] = path;
  field('#repo-alias', ''); field('#repo-path', '');
  markDirty();
  renderRepositories();
};
$('#github-connect').onclick = () => busy($('#github-connect'), async () => {
  const prompt = await invoke('begin_github_login', { clientId: $('#github-client-id').value.trim() });
  $('#github-code').textContent = prompt.user_code;
  $('#github-device').hidden = false;
  notify(t('Code ready. Authorize the GitHub App with the repositories you want.'));
});
$('#github-open').onclick = () => {
  if (window.ptaTransport.browser) window.open('https://github.com/login/device', '_blank', 'noopener,noreferrer');
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
$('#add-source').onclick = () => {
  const id = $('#source-id').value.trim();
  const repository = $('#source-repo').value;
  const path = $('#source-path').value.trim();
  const description = $('#source-description').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || !repository || !path || !description) return fail(t('Fill in the ID (lowercase letters, digits, - or _), repository, file and description.'));
  if (current.map.resources.some(r => r.id === id)) return fail(t('That ID already exists.'));
  current.map.resources.push({ id, description, topics: names($('#source-topics').value), enabled: false,
    external_processing: false, allowed_conversations: [], allowed_senders: [], kind: 'file', repository, path });
  for (const name of ['#source-id', '#source-path', '#source-topics', '#source-description']) field(name, '');
  markDirty();
  renderResources();
  notify(t('Source {id} added, disabled. Save the changes to keep it.', { id }));
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
    answerBubble(result);
  } catch (error) {
    pending.remove();
    bubble('bot none', t('The test could not complete: {error}', { error: tx(String(error?.message ?? error)) }));
  } finally {
    clearInterval(timer);
    send.disabled = false;
  }
};

reload().then(async () => { await loadActivity(); await loadRegistration(); if (activityWindow) showTab('registration'); }).catch(fail);

function browserMicrosoftLogin(url) {
  const dialog = $('#web-microsoft-dialog');
  const address = new URL(url);
  if (address.protocol !== 'https:' || address.hostname !== 'login.microsoftonline.com') throw new Error(t('Invalid Microsoft sign-in address.'));
  $('#web-microsoft-link').href = address.href;
  $('#web-microsoft-redirect').value = '';
  $('#web-microsoft-status').textContent = '';
  dialog.showModal();
  return new Promise((resolve, reject) => {
    let cancelled = false;
    const close = () => { $('#web-microsoft-redirect').value = ''; $('#web-microsoft-link').removeAttribute('href'); dialog.close(); };
    const cancel = async event => {
      event?.preventDefault(); cancelled = true;
      await rawInvoke('command', { request: { method: 'cancel_microsoft', args: {}, contract: 1 } }).catch(() => {});
      close(); reject(new Error(t('Microsoft sign-in cancelled.')));
    };
    dialog.oncancel = cancel;
    $('#web-microsoft-cancel').onclick = cancel;
    $('#web-microsoft-form').onsubmit = async event => {
      event.preventDefault();
      const button = $('#web-microsoft-finish');
      button.disabled = true;
      $('#web-microsoft-status').textContent = t('Finishing sign-in…');
      try {
        let args = { redirect_url: $('#web-microsoft-redirect').value.trim() };
        $('#web-microsoft-redirect').value = '';
        const deadline = Date.now() + 600_000;
        let result;
        do {
          result = await rawInvoke('command', { request: { method: 'finish_microsoft', args, revision: null, contract: 1 } });
          args = {};
          if (result.code === 'authorization_pending') await new Promise(done => setTimeout(done, 2000));
        } while (!cancelled && result.code === 'authorization_pending' && Date.now() < deadline);
        if (cancelled) return;
        if (!result.ok) throw new Error(result.message || result.code);
        close(); resolve(result);
      } catch (error) { $('#web-microsoft-status').textContent = tx(error.message || String(error)); }
      finally { button.disabled = false; }
    };
  });
}

if (window.ptaTransport.browser) {
  window.ptaTransport.ready.then(identity => {
    $('#web-account').hidden = $('#panel-web-account').hidden = $('#web-hosting-note').hidden = false;
    $('#web-username').textContent = identity.username;
    for (const id of ['#public-url', '#bind', '#tunnel-config', '#cloudflare-tunnel']) $(id).disabled = true;
    for (const button of [$('#web-logout'), $('#web-logout-settings')]) {
      button.onclick = () => busy(button, async () => {
        const result = await window.ptaTransport.request('/api/logout', {});
        window.location.replace(result.logout_url);
      });
    }
  }).catch(fail);
}
